//! `cordial-shell` — the core shell binary.
//!
//! [ADR-002](../../../docs/adr/ADR-002-core-shell-and-ui-handoff.md) draws the
//! line this crate has to stay inside: core owns a window, the chooser that
//! paints at T1, and a minimal settings fallback narrow enough to disable a
//! broken plugin. Everything richer — real settings, themes, plugin-contributed
//! chooser entries, instance management — belongs to the UI plugin that takes
//! over at T3. This binary does not link the plugin host or the engine at all;
//! it is built standalone on purpose, so the window/chooser/settings shape can
//! be proven before either of those exist. See `window.rs` for the seam where
//! the engine's Wayland surface will eventually be embedded.
//!
//! [ADR-011](../../../docs/adr/ADR-011-wayland-and-libadwaita.md) is why this
//! is libadwaita rather than bare GTK: `AdwStyleManager` tracks
//! `org.freedesktop.appearance color-scheme` on its own, live, which is what
//! keeps the area behind the engine's canvas the desktop's actual background
//! colour instead of a flash of white while a resize catches up.

mod chooser;
mod deep_link;
mod install;
mod instructions;
mod launch;
mod profile_switcher;
mod settings;
mod shell_config;
mod updater;
mod window;

/// Guards `CORDIAL_PROFILE_ROOT` across every test in this binary that points
/// it at a scratch directory.
///
/// **Shared rather than one per file, and that distinction is load-bearing.**
/// `profile_switcher.rs` and `launch.rs` each used to keep their own private
/// mutex for this, on the reasonable-looking assumption that a mutex local to
/// a file's own `mod tests` was enough to stop its own tests interleaving.
/// It stops that, and does nothing at all about a *different* file's tests
/// setting the same process-wide variable at the same moment — two locks
/// guarding one variable serialise nothing against each other. Measured, not
/// assumed: adding `launch.rs`'s vpn-gate test surfaced this by actually
/// failing `profile_switcher::tests::the_list_offers_no_profile_that_does_not_exist`
/// on one run out of several, reading back
/// `["CordialTest", "evr_l", "main"]` where only `["alt", "main"]` should have
/// existed — another test's scratch directory, torn into view mid-assertion.
/// It did not reproduce every run, which is exactly the "one-in-three flake"
/// shape `profile.rs`'s own tests already warn about, and exactly why a fix
/// that "seemed to work" on a single clean run would not have been evidence of
/// anything.
#[cfg(test)]
pub(crate) static PROFILE_ROOT_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

use libadwaita::gtk::gio;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Must match `packaging/org.cordial.Cordial.desktop`'s file name and
/// `StartupWMClass`. GNOME Shell uses the application id to find the desktop
/// entry for window-to-launcher matching; let the two drift and the taskbar
/// icon and startup notification silently stop matching up rather than erroring.
///
/// It is also what makes Cordial single-instance, which the deep-link handler
/// depends on rather than works around: a `GApplication` with a fixed id
/// registers on the session bus, and a second invocation carrying a URL hands
/// that URL to the process already registered and exits, which is why clicking
/// a link on a website wakes the launcher instead of starting a second one.
const APP_ID: &str = "org.cordial.Cordial";

fn main() -> libadwaita::glib::ExitCode {
    // Both flags, and the second one is the load-bearing one.
    //
    // `HANDLES_OPEN` says this application takes URLs at all; a `GApplication`
    // without it refuses arguments outright. On its own it delivers them as
    // `GFile`s to the `open` signal, and **`GFile` reshapes a Roblox link**:
    // `roblox-player:1+launchmode:play+gameinfo:AAA` comes back out as
    // `roblox-player:///1+launchmode:play+gameinfo:AAA`, because GIO parses it
    // as a URL with an empty authority. `deep_link`'s tests pin that
    // measurement. So `HANDLES_COMMAND_LINE` is added, which hands over the
    // invoking process's `argv` — remote invocations included, which is where
    // the string would otherwise have already been rewritten before this
    // process saw it — and the link is taken from there, byte for byte.
    //
    // `open` stays connected for the other route in: a caller that speaks
    // `org.freedesktop.Application.Open` over D-Bus hands over URIs and never
    // an `argv`, and a link arriving that way is better carried in GIO's
    // spelling than dropped.
    let app = libadwaita::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    // The shell, once there is one. A link that arrives while Cordial is
    // already up is the ordinary case — somebody clicks Play on the website
    // with the launcher open — and it must reach that window rather than build
    // a second one.
    let shell: Rc<RefCell<Option<window::Shell>>> = Rc::new(RefCell::new(None));

    {
        let shell = shell.clone();
        app.connect_activate(move |app| start(app, &shell));
    }
    {
        // The path every desktop launch takes, local or remote: `Exec=` in the
        // desktop entry passes `%u` as an argument, and this is where it lands
        // unaltered.
        let shell = shell.clone();
        app.connect_command_line(move |app, command_line| {
            start(app, &shell);
            let mut links = 0;
            for argument in command_line.arguments().into_iter().skip(1) {
                // Lossy is safe here rather than convenient: anything that was
                // not valid UTF-8 comes out with replacement characters, which
                // `accept` refuses along with everything else that is not
                // printable ASCII.
                queue(&shell, &argument.to_string_lossy());
                links += 1;
            }
            // A second invocation with nothing on it is somebody starting
            // Cordial again — from the desktop icon, most likely — and what
            // they want is the window they already have, in front.
            if links == 0 {
                if let Some(shell) = shell.borrow().as_ref() {
                    shell.present();
                }
            }
            // The status the *invoking* process exits with, which for a remote
            // invocation is the one the browser waits on. Nothing here can
            // fail in a way that process should hear about: a refused link is
            // reported on the primary's stdout and the launcher is up either
            // way.
            libadwaita::glib::ExitCode::SUCCESS
        });
    }
    {
        let shell = shell.clone();
        app.connect_open(move |app, files, _hint| {
            start(app, &shell);
            for file in files {
                queue(&shell, &file.uri());
            }
        });
    }

    app.run()
}

/// Check a link and hand it to the window, or say why not.
///
/// Nothing about the string is trusted: it was produced by a browser acting on
/// somebody's click. [`deep_link::accept`] is what decides, and the only thing
/// that ever consumes the result is `Command::arg`.
fn queue(shell: &Rc<RefCell<Option<window::Shell>>>, raw: &str) {
    match deep_link::accept(raw) {
        Ok(url) => match shell.borrow().as_ref() {
            Some(shell) => {
                // Printed as well as shown, because the banner shows the first
                // sixty characters and this is the only place the whole of what
                // arrived can be compared with what the browser sent.
                println!("  shell: holding {url} until you press Roblox");
                shell.queue_join(url);
            }
            // `start` built the window immediately above, so this is
            // unreachable rather than merely unlikely — and said out loud,
            // because a link silently going nowhere is the failure this whole
            // path exists to avoid.
            None => println!("  shell: no window to hand {url} to"),
        },
        // Reported rather than swallowed: somebody whose browser opens Cordial
        // and appears to do nothing has no other way to find out that the link
        // was refused, or why.
        Err(why) => println!("  shell: ignoring {why}"),
    }
}

/// Build the window, once.
///
/// Called from all three entry points because any of them can be the first
/// thing that happens, and called again on every subsequent one because that is
/// what a remote invocation looks like from in here. The second call does
/// nothing.
fn start(app: &libadwaita::Application, shell: &Rc<RefCell<Option<window::Shell>>>) {
    if shell.borrow().is_some() {
        return;
    }

    // Before anything can be launched, because the launcher points the
    // engine at a profile directory and the storage that has a login in it
    // is still at the pre-ADR-012 path. Skipped when there is nothing to
    // move, which is every run after the first.
    cordial_shell::profile::migrate_legacy_layout();

    let config_path = Rc::new(shell_config::path());
    let config = Rc::new(RefCell::new(shell_config::load(&config_path)));

    // Applied before the window exists so the very first paint already
    // matches whatever the user last chose in Appearance, rather than
    // flashing the libadwaita default and then correcting itself.
    config.borrow().appearance.apply();

    *shell.borrow_mut() = Some(window::build(app, config, config_path));
}
