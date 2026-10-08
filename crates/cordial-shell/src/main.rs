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

// This binary compiles its own copies of `audio_devices.rs` and
// `root_warning.rs` (see the `mod` lines below) rather than depending on the
// `cordial_shell` lib crate for them, so `[lints] workspace = true`'s
// `unsafe_code = "deny"` applies to this crate root independently of the
// `#![allow(unsafe_code)]` on `lib.rs` -- see that file's comment, and
// [ADR-036](../../../docs/adr/ADR-036-unsafe-is-a-boundary-not-a-convention.md),
// for why cordial-shell carries the allow at all.
#![allow(unsafe_code)]

mod audio_devices;
mod chooser;
mod crash;
mod deep_link;
mod browser_account;
mod diagnostics;
mod doctor_run;
mod flag_import;
mod download_progress;
mod install;
mod instructions;
mod launch;
mod live;
mod migration;
mod multi_instance_warning;
mod profile_switcher;
mod refresh_watch;
mod report;
mod roblox_store;
mod roblox_versions;
mod root_warning;
mod settings;
mod shell_config;
mod updater;
mod vr_settings;
mod quest_wizard;
mod x11_notice;
// The window itself needs webkitgtk6.0-devel, which an immutable host does not
// have; the policy beside it needs nothing and is always compiled, because it is
// the part that has to be right and it should be under test everywhere.
#[cfg(feature = "webview")]
mod webview;
mod webview_policy;
mod window;
use cordial_shell::window_state;

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

/// Must match `packaging/io.github.luohoa97.Cordial.desktop`'s file name and
/// `StartupWMClass`. GNOME Shell uses the application id to find the desktop
/// entry for window-to-launcher matching; let the two drift and the taskbar
/// icon and startup notification silently stop matching up rather than erroring.
///
/// It is also what makes Cordial single-instance, which the deep-link handler
/// depends on rather than works around: a `GApplication` with a fixed id
/// registers on the session bus, and a second invocation carrying a URL hands
/// that URL to the process already registered and exits, which is why clicking
/// a link on a website wakes the launcher instead of starting a second one.
const APP_ID: &str = "io.github.luohoa97.Cordial";

fn main() -> libadwaita::glib::ExitCode {
    // **Answered before the `GApplication` exists, and that is the whole
    // point.** This binary is a single-instance application: with a Cordial
    // already running, anything handed to a second invocation is forwarded to
    // the first over D-Bus and this process exits. A diagnostics flag that went
    // through that would print nothing, or print into the other process's
    // terminal, which is worse than printing nothing.
    //
    // It also has to work when the shell cannot start at all -- a missing
    // WebKitGTK, a GTK too old, no display -- because that is exactly the
    // report that most needs the distribution and the package format in it.
    // Reading `argv` directly costs nothing and depends on none of that.
    let flags: Vec<String> = std::env::args().skip(1).collect();
    if flags.iter().any(|a| a == "--diagnostics") {
        print!("{}", diagnostics::report());
        return libadwaita::glib::ExitCode::SUCCESS;
    }
    // `--doctor` for the same reasons, and it must not go through the
    // single-instance forwarding either: it is a question about this machine,
    // asked of this process.
    if flags.iter().any(|a| a == "--doctor") {
        return libadwaita::glib::ExitCode::from(doctor_run::run(&flags));
    }
    // Also before the `GApplication`, and for a scripting reason: importing a
    // flag list should work over ssh and from a dotfiles script, with no window.
    if flags.iter().any(|a| a == "--import-flags") {
        return libadwaita::glib::ExitCode::from(flag_import::run(&flags));
    }
    // The Quest build for "Play in VR", from a file, with no window: the same
    // `quest::import` Settings → VR runs, for a machine where a file picker is
    // the hard part (ADR-053).
    if flags.iter().any(|a| a == "--import-quest-apk") {
        return libadwaita::glib::ExitCode::from(import_quest_apk(&flags));
    }
    // What `--doctor` runs, in a child of this binary, to ask Vulkan for its
    // devices without loading a driver into the launcher. Not in `--help`: it
    // prints a private line format for the doctor to read.
    if flags.iter().any(|a| a == "--vulkan-probe") {
        return libadwaita::glib::ExitCode::from(cordial_shell::vulkan_probe::run_probe_mode());
    }
    // **`--help` printed nothing at all and exited 0**, which is how a flag
    // gets shipped and never found. `GApplication` only prints its own usage
    // for options it was told about, and this binary registers none -- it takes
    // a deep link positionally and now one flag. Four lines beat a user
    // discovering `--diagnostics` from a bug report template they cannot open
    // because the shell will not start.
    if flags.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "cordial {version}\n\
             \n\
             Usage: cordial [ROBLOX-LINK]\n\
             \n\
             With no arguments it opens the launcher. A `roblox-player:` or\n\
             `roblox:` link joins that experience, which is what your browser\n\
             hands over when you press Play on the website.\n\
             \n\
             Options:\n\
             \x20 --diagnostics  Print which Cordial and Roblox build this is, the\n\
             \x20                distribution, and how Cordial was installed. Paste\n\
             \x20                it into a bug report. The main menu's Report a\n\
             \x20                Problem shows the same block behind a Copy button.\n\
             \x20 --doctor       Check this machine for what stops Roblox running: the\n\
             \x20                display, GPU and Vulkan, sound, keyring and more, each\n\
             \x20                with what to do about it. Exits 1 only if something\n\
             \x20                will stop it starting. --offline skips the update check.\n\
             \x20 --import-flags FILE|-|--sober\n\
             \x20                Merge a Bloxstrap, Fishstrap or Sober FastFlag list into\n\
             \x20                a profile (--profile NAME, --replace). Skips a bad entry\n\
             \x20                and names it; the rest are kept.\n\
             \x20 --import-quest-apk FILE\n\
             \x20                File the Quest build of Roblox, pulled from your own\n\
             \x20                headset, for Play in VR. Checks Roblox's signature.\n\
             \x20 -h, --help     This.\n\
             \n\
             `cordial-run` is the loader this launches and is not meant to be run\n\
             by hand. Issues: https://github.com/luohoa97/cordial/issues\n\
             \n\
             {notice}",
            version = cordial_shell::version::full(),
            notice = cordial_shell::version::NOTICE,
        );
        return libadwaita::glib::ExitCode::SUCCESS;
    }

    // From here on this process shows a window, and the log export (Report a
    // Problem -> Save logs) wants what it has printed. After every flag that
    // answers and exits, so those keep their plain stdout.
    cordial_shell::session_log::install();

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
        app.connect_activate(move |app| {
            start(app, &shell);
            if let Some(shell) = shell.borrow().as_ref() {
                shell.present();
            }
        });
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
/// somebody's click. [`deep_link::accept`] validates the envelope before the
/// window attempts account routing or holds it for a manual launch.
fn queue(shell: &Rc<RefCell<Option<window::Shell>>>, raw: &str) {
    match deep_link::accept(raw) {
        Ok(url) => match shell.borrow().as_ref() {
            Some(shell) => {
                println!("  shell: received Roblox link");
                shell.queue_join(url);
            }
            // `start` built the window immediately above, so this is
            // unreachable rather than merely unlikely — and said out loud,
            // because a link silently going nowhere is the failure this whole
            // path exists to avoid.
            None => println!("  shell: no window to receive Roblox link"),
        },
        // Reported rather than swallowed: somebody whose browser opens Cordial
        // and appears to do nothing has no other way to find out that the link
        // was refused, or why.
        Err(why) => {
            println!("  shell: ignoring {why}");
            if let Some(shell) = shell.borrow().as_ref() {
                shell.present();
            }
        }
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

    // ADR-054: before the window, because everything it draws and launches
    // reads the store, and the first launch after the upgrade has things to
    // move into it. Once, by construction -- see `migration`.
    let migrated = migration::run(&mut config.borrow_mut());
    migrated.print();
    if migrated.config_changed() {
        if let Err(e) = shell_config::save(&config_path, &config.borrow()) {
            eprintln!("shell: could not save {}: {e}", config_path.display());
        }
    }

    // Applied before the window exists so the very first paint already
    // matches whatever the user last chose in Appearance, rather than
    // flashing the libadwaita default and then correcting itself.
    config.borrow().appearance.apply();

    // Running games follow Settings (ADR-044). Leaked on purpose: the watch has
    // to outlive every window and dropping the guard would stop it.
    let live_watch = live::start(&config_path, &config.borrow());
    std::mem::forget(live_watch);

    *shell.borrow_mut() = Some(window::build(app, config, config_path, migrated.offer_newest()));
}

/// `cordial --import-quest-apk FILE`. Exit 0 when filed (or already held), 1
/// when refused, 2 on a bad command line.
fn import_quest_apk(flags: &[String]) -> u8 {
    // Filing a Quest build in a build that cannot run it would only leave a
    // copy nothing reads.
    if !cordial_shell::vr::HOST_SUPPORTED {
        eprintln!("cordial: this build has no VR support, so there is nowhere to use a Quest build");
        return 1;
    }
    let Some(path) = flags.iter().skip_while(|a| *a != "--import-quest-apk").nth(1) else {
        eprintln!("cordial: --import-quest-apk needs a FILE");
        return 2;
    };
    let root = cordial_update::quest::root();
    match cordial_update::quest::import(
        std::path::Path::new(path),
        &root,
        &cordial_update::apk_signature::pinned_quest(),
    ) {
        Ok(version) => {
            println!("Quest build {version} is ready for Play in VR.");
            0
        }
        Err(e) => {
            eprintln!("cordial: {e}");
            1
        }
    }
}
