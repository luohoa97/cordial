//! The minimal settings fallback ADR-002 requires.
//!
//! Not the settings surface — the UI plugin owns that once it takes over at
//! T3. This exists purely for the recoverability argument ADR-002 makes: if
//! the UI plugin is broken (bad update, protocol mismatch after a Cordial
//! upgrade), the user still needs a way to see what is installed and turn
//! the offending one off, plus enough of Cordial's own appearance and
//! graphics preference to be usable, without a terminal.
//!
//! `AdwPreferencesDialog` with several `AdwPreferencesPage`s — Roblox, Updates,
//! General, Plugins, Report — each becomes its own tab/sidebar entry for free;
//! that is libadwaita's own page-switcher, not something built here. Appearance
//! used to be its own page; its two groups live inside General now, and
//! `build_preferences_window`'s own comment explains why.
//!
//! The Roblox page is newer than the argument above and is here for a different
//! reason: nothing in Cordial used to record where a Roblox build lived, so the
//! chooser had no target and the only way to run the client was a hand-typed
//! command line. That is configuration rather than a fallback, and it belongs
//! in the shell because the shell is what has to work when nothing else does.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use libadwaita as adw;
use libadwaita::glib;
use libadwaita::gtk;
use libadwaita::prelude::*;

use std::collections::BTreeSet;

use cordial_plugins::capability::Capability;
use cordial_plugins::consent;
use cordial_plugins::denials;
use cordial_plugins::enablement;
use cordial_plugins::grants;
use cordial_plugins::plugin_data;
use cordial_plugins::manifest::{self, Plugin};
use cordial_plugins::marketplace;
use cordial_plugins::registry::Entry;
use cordial_plugins::resolve;
use cordial_plugins::sign;
use cordial_plugins::source::LocalFileSource;
use cordial_plugins::unpack;

use crate::audio_devices;
use crate::install;
use crate::shell_config::{self, AppearanceScheme, AudioOutput, ShellConfig, ThrottleWhen};

/// What a plugin asks for, and what this profile has actually given it.
///
/// Both halves are read from disk every time the page is built. There used to be
/// a `DemoRegistry` here instead: three hand-written manifests, including one
/// claiming `assets.override` and one claiming `presence.set`, parsed against a
/// directory spelled `/nonexistent/demo-plugin-dir`. It rendered on a screen
/// whose entire job is telling somebody what has been granted what, on a machine
/// where `~/.local/share/cordial/plugins` is empty. ADR-003's default-deny means
/// nothing if the surface that displays it is fiction, and "this plugin may
/// change your assets and talk to Discord for you" is not a sentence to invent.
///
/// Three states, kept apart because somebody debugging a plugin that is doing
/// nothing has to be able to tell which one they are in.
///
/// *Enabled and granted something* is the one that runs. *Enabled and granted
/// nothing* also does not run, and collapsing it into "off" would hide the only
/// thing that would fix it — nobody has approved anything yet, which is where
/// ADR-003's default deny leaves every freshly installed plugin. *Disabled* does
/// not run whatever its grants say, and its grants are still shown, because they
/// are still there: turning something off does not cost the approvals.
/// **One line now, and it used to be three.** It listed "Requests:", then
/// "Granted:", then the state, on every row, so a page with four plugins on it
/// opened as twelve lines of protocol vocabulary before the user had touched
/// anything. The three things it must still never do are unchanged and are why
/// this is not simply the word "On": it must not describe an unapproved plugin
/// as switched off, it must not suggest that switching one off took its
/// approvals away, and it must never print a capability the plugin merely
/// asked for as though it had been allowed. The detail lives one click down,
/// in the expander, where a switch per capability says the same thing and can
/// act on it.
pub fn capability_summary(
    plugin: &Plugin,
    granted: Option<&BTreeSet<Capability>>,
    enabled: bool,
) -> String {
    let names = |set: &BTreeSet<Capability>| -> String {
        set.iter().copied().map(Capability::name).collect::<Vec<_>>().join(", ")
    };
    let empty = BTreeSet::new();
    let granted = granted.unwrap_or(&empty);
    let allowed: BTreeSet<Capability> =
        plugin.requested.iter().copied().filter(|c| granted.contains(c)).collect();
    let withheld: BTreeSet<Capability> =
        plugin.requested.iter().copied().filter(|c| !granted.contains(c)).collect();

    if !enabled {
        // Named separately from "Off" alone, because the fear this answers is
        // that turning something off quietly revoked what you had allowed it.
        return if allowed.is_empty() {
            "Off".to_string()
        } else {
            "Off. What you allowed it to do is kept.".to_string()
        };
    }
    if plugin.requested.is_empty() {
        return "On. It asks for no permissions.".to_string();
    }
    if allowed.is_empty() {
        // Says where to go, because the switches are inside the expander and
        // the row gives no sign of it. Reported as "the plugin doesnt work" by
        // somebody looking at a plugin that was installed, enabled, and had
        // simply never been granted anything -- which is ADR-003's default
        // deny working exactly as intended and reading as a broken plugin.
        return "On, but you have not allowed it to do anything yet. Open this row to choose."
            .to_string();
    }
    if withheld.is_empty() {
        format!("On. Allowed: {}.", names(&allowed))
    } else {
        format!("On. Allowed: {}. Not allowed: {}.", names(&allowed), names(&withheld))
    }
}

/// The two tiers, split, with the no-shadowing rule already applied.
///
/// Discovery is per-root and the rule between the roots lives here rather than
/// in `cordial-plugins`, because it is the settings window that has to *show*
/// the collision: `cordial_runtime::flags::collect` resolves the same conflict
/// by printing a line to a stdout nobody opening this window is reading. A user
/// whose plugin silently did not appear has no way to find out why, and "it is
/// not in the list" is indistinguishable from "it did not install".
///
/// Takes both roots rather than calling `manifest::plugin_root` and
/// `manifest::system_plugin_root` itself so this is testable on scratch
/// directories, with no display and no environment variables -- the same reason
/// `host_window.rs`'s `fit_within` is a pure function.
fn discover_tiers(system_root: &Path, user_root: &Path) -> (Vec<Plugin>, Vec<Plugin>, Vec<String>) {
    let system = manifest::discover(system_root);
    let claimed: BTreeSet<String> = system.iter().map(|p| p.manifest.id.clone()).collect();
    let mut user = Vec::new();
    let mut shadowed = Vec::new();
    for plugin in manifest::discover(user_root) {
        if claimed.contains(&plugin.manifest.id) {
            shadowed.push(plugin.manifest.id.clone());
        } else {
            user.push(plugin);
        }
    }
    (system, user, shadowed)
}

/// Which of the two directories a plugin was found in.
///
/// Only two things differ, and both are about the directory rather than the
/// plugin: a built-in one has no Remove button because its files are read-only
/// and not the user's to delete, and it cannot be replaced by a same-id user
/// directory. It can be switched off exactly like any other, which is the whole
/// reason enablement is a separate file from grants.
#[derive(Clone, Copy, PartialEq)]
enum Tier {
    BuiltIn,
    User,
}

/// "Appearance" — Cordial's own theme preference, independent of anything
/// else on the desktop. `AppearanceScheme::apply` is what makes this take
/// effect immediately; this function only wires the row to it and to disk.
/// Appearance's two groups, added to a page somebody else owns.
///
/// **This was its own tab and did not deserve one.** Reported as "why are there
/// so many tabs": Settings had seven, and two of them held two groups between
/// them. A preferences window is a place people arrive at knowing what they
/// want to change and leave again; a tab per group makes them hunt through
/// seven pages to find the one row they came for.
fn add_appearance_groups(page: &adw::PreferencesPage, config: Rc<RefCell<ShellConfig>>, config_path: Rc<PathBuf>) {
    // Cloned up front: the theme row's closure below takes ownership of both,
    // and the title bar row further down needs them too.
    let config_for_bar = config.clone();
    let config_path_for_bar = config_path.clone();

    // No group description. What used to be here explained that this is
    // Cordial's own preference rather than the desktop's, that System reads
    // `org.freedesktop.appearance` and updates live, that it falls back to dark
    // when nothing answers the portal, and that Light and Dark stay put
    // regardless. All of that is true and none of it is why anyone opened this
    // page: they came to change the theme. The one consequence a user can act
    // on -- that System tracks the desktop -- is on the row below, and the rest
    // is here, next to the code, which is where this project keeps its
    // reasoning.
    let group = adw::PreferencesGroup::builder().title("Theme").build();

    // Order has to match AppearanceScheme::index/from_index.
    let model = gtk::StringList::new(&["Light", "Dark", "System"]);
    let row = adw::ComboRow::builder()
        .title("Theme")
        .subtitle("System follows the desktop.")
        .model(&model)
        .selected(config.borrow().appearance.index())
        .build();

    row.connect_selected_notify(move |row| {
        let scheme = AppearanceScheme::from_index(row.selected());
        scheme.apply();
        config.borrow_mut().appearance = scheme;
        if let Err(e) = crate::shell_config::save(&config_path, &config.borrow()) {
            eprintln!("shell: could not save {}: {e}", config_path.display());
        }
    });

    group.add(&row);
    page.add(&group);

    // ---- the game window's chrome ------------------------------------------
    //
    // Two named sizes, not a pixel value. A free number gives chrome that
    // matches nothing else on the desktop and, under about 30px, clips the
    // window controls -- which is the "the x in the title bar looks off" that
    // put this row here in the first place. Somebody who wants the pixels back
    // properly wants fullscreen, which F11 gives and which persists per profile.
    let window_group = adw::PreferencesGroup::builder().title("Game window").build();
    // Order has to match TitleBar::index/from_index.
    let bar_model = gtk::StringList::new(&["Default", "Compact"]);
    let bar_row = adw::ComboRow::builder()
        .title("Title bar")
        .subtitle("Default matches every other window on your desktop.")
        .model(&bar_model)
        .selected(config_for_bar.borrow().title_bar.index())
        .build();
    {
        let config = config_for_bar.clone();
        let config_path = config_path_for_bar.clone();
        bar_row.connect_selected_notify(move |row| {
            let choice = crate::shell_config::TitleBar::from_index(row.selected());
            config.borrow_mut().title_bar = choice;
            // Reported rather than swallowed, and the row is not put back:
            // unlike a switch, a combo that snapped back would hide which value
            // is actually stored. Saying so is the honest half.
            if let Err(e) = crate::shell_config::save(&config_path, &config.borrow()) {
                eprintln!("shell: could not save the title bar choice: {e}");
            }
        });
    }
    // Says when it applies, because it does not apply to a window already open
    // and a setting that appears to do nothing is worse than one that explains
    // itself.
    bar_row.set_subtitle("Default matches every other window on your desktop. Applies to the next launch.");
    window_group.add(&bar_row);
    page.add(&window_group);

}

/// Save, and say so on stderr rather than swallowing the error.
///
/// A settings window that accepts a choice and silently fails to keep it is the
/// worst of the three possible behaviours, because the user finds out one
/// launch later and blames the launch.
pub fn persist(config: &Rc<RefCell<ShellConfig>>, path: &Rc<PathBuf>) {
    if let Err(e) = shell_config::save(path, &config.borrow()) {
        eprintln!("shell: could not save {}: {e}", path.display());
    }
}

/// One row showing a path Cordial is using, where it came from, and how to
/// change it.
///
/// The provenance line is not decoration. The build usually comes from another
/// application's private directory (Sober's), and a user who does not know that
/// has no way to understand why deleting Sober broke Cordial. ADR-002's rule
/// about not silently depending on something applies to a path exactly as much
/// as to a capability.
fn path_row(
    title: &str,
    effective: Option<(PathBuf, String)>,
    chosen: bool,
    on_choose: impl Fn() + 'static,
    on_clear: impl Fn() + 'static,
) -> adw::ActionRow {
    let subtitle = match &effective {
        Some((path, origin)) => format!("{}\n{origin}", path.display()),
        None => "Not found. Press Roblox in the launcher for how to get one.".to_string(),
    };
    let row = adw::ActionRow::builder().title(title).subtitle(subtitle).build();
    row.set_subtitle_lines(3);

    if chosen {
        let clear = gtk::Button::from_icon_name("edit-clear-symbolic");
        clear.set_tooltip_text(Some("Forget this and look again on the next launch"));
        clear.set_valign(gtk::Align::Center);
        clear.add_css_class("flat");
        clear.connect_clicked(move |_| on_clear());
        row.add_suffix(&clear);
    }

    let choose = gtk::Button::with_label("Choose…");
    choose.set_valign(gtk::Align::Center);
    choose.connect_clicked(move |_| on_choose());
    row.add_suffix(&choose);
    row
}

/// "Roblox" — where the build is.
///
/// The whole reason `cordial-shell` could not launch anything: nothing
/// persisted where a Roblox build lived, so the only route to a running client
/// was a hand-typed command line. Both rows here are *overrides* — left empty,
/// which is the normal state, `install::locate` looks for a build every time,
/// because a remembered "yes it is installed" goes stale the moment somebody
/// moves it.
fn build_roblox_page(
    parent: &impl IsA<gtk::Window>,
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
) -> adw::PreferencesPage {
    let page =
        adw::PreferencesPage::builder()
            .title("Roblox")
            .name("roblox")
            .icon_name("applications-games-symbolic")
            .build();

    let group = adw::PreferencesGroup::builder()
        .title("Roblox build")
        // Kept, shortened, because it is the one thing on this page a user
        // cannot work out from the controls: that leaving both empty is a
        // working state rather than an unfinished one. The "and never will"
        // half was the project talking about itself.
        .description("Leave these empty and Cordial finds a build. Choose a file to pin one.")
        .build();

    let apk = install::effective_apk(&config.borrow().roblox)
        .map(|(path, origin)| (path, origin.describe().to_string()));
    let apk_chosen = config.borrow().roblox.apk.is_some();

    let window = parent.as_ref().clone();
    let apk_row = {
        let config = config.clone();
        let config_path = config_path.clone();
        let window = window.clone();
        let cleared = config.clone();
        let cleared_path = config_path.clone();
        path_row(
            "APK",
            apk,
            apk_chosen,
            move || {
                let config = config.clone();
                let config_path = config_path.clone();
                choose_file(&window, "Choose the Roblox APK", false, move |path| {
                    config.borrow_mut().roblox.apk = Some(path);
                    persist(&config, &config_path);
                });
            },
            move || {
                cleared.borrow_mut().roblox.apk = None;
                persist(&cleared, &cleared_path);
            },
        )
    };
    group.add(&apk_row);

    // Shown separately from the APK because it usually is separate: on a split
    // build `libroblox.so` is inside `split_config.x86_64.apk`, not `base.apk`,
    // and Cordial extracts it into its own cache rather than writing into
    // whichever application's directory the APK came from.
    //
    // The extracted case carries `updater::cache_line`, which says whether the
    // engine still matches the APK above. That used to be a row in the
    // header-bar button's window; it belongs here, beside the directory it is
    // about, and it is the only place a Cordial that re-extracts 115 MB on every
    // launch says what it is comparing.
    let lib = config.borrow().roblox.lib_dir.clone().map(|p| (p, "Chosen in Settings".to_string())).or_else(
        || {
            let cache = install::engine_cache();
            if !cache.join(install::LIBRARY).is_file() {
                return None;
            }
            let apk = install::effective_apk(&config.borrow().roblox).map(|(path, _)| path);
            let line = crate::updater::cache_line(
                true,
                cordial_update::cache::stamp_of(&cache),
                apk.as_deref().is_some_and(|apk| cordial_update::cache::is_current(&cache, apk)),
            );
            Some((cache, line))
        },
    );
    let lib_chosen = config.borrow().roblox.lib_dir.is_some();

    let lib_row = {
        let config = config.clone();
        let config_path = config_path.clone();
        let window = window.clone();
        let cleared = config.clone();
        let cleared_path = config_path.clone();
        path_row(
            "Engine directory",
            lib,
            lib_chosen,
            move || {
                let config = config.clone();
                let config_path = config_path.clone();
                choose_file(&window, "Choose the directory holding libroblox.so", true, move |path| {
                    config.borrow_mut().roblox.lib_dir = Some(path);
                    persist(&config, &config_path);
                });
            },
            move || {
                cleared.borrow_mut().roblox.lib_dir = None;
                persist(&cleared, &cleared_path);
            },
        )
    };
    group.add(&lib_row);
    page.add(&group);

    // The update settings were a group on this page, on the argument that they
    // govern the build the two rows above point at. They are a page of their own
    // now, and the argument did not survive what it produced: a dropdown, two
    // switches, a conditional warning row and six lines of description sitting
    // on top of the two path rows this page exists for, so that "where is my
    // Roblox build" and "when does Cordial look for a new one" were answered by
    // one scroll. Two questions, two tabs, and libadwaita draws the tab.

    // The profile used to be a third row here, a text entry, and it is
    // deliberately not replaced by anything. It lives in the header bar now —
    // see `profile_switcher.rs` — because choosing one is a thing done on the
    // way to launching rather than a setting to go and find, and because two
    // controls writing `ShellConfig.profile` would be two things to keep
    // agreeing with each other.

    // ---- stored data --------------------------------------------------------
    //
    // Here rather than in General because it is Roblox's data, not Cordial's,
    // and because this page is where somebody already goes to think about the
    // build.
    let data_group = adw::PreferencesGroup::builder()
        .title("Stored data")
        .description(
            "Roblox keeps its downloaded assets, caches and local storage in this profile. \
             Clearing it does not sign you out and does not touch your plugins.",
        )
        .build();

    let profile_name = config.borrow().profile.clone();
    let profile_dir = cordial_shell::profile::dir(&profile_name).ok();
    let bytes = profile_dir.as_ref().map(|d| cordial_shell::profile::engine_data_bytes(d)).unwrap_or(0);

    let clear_row = adw::ActionRow::builder()
        .title("Clear Roblox's stored data")
        .subtitle(if bytes == 0 {
            format!("Nothing stored for the {profile_name} profile yet.")
        } else {
            format!(
                "{} for the {profile_name} profile. It is downloaded again as you use Roblox.",
                cordial_shell::profile::human_bytes(bytes)
            )
        })
        .build();
    clear_row.set_subtitle_lines(2);

    let clear = gtk::Button::with_label("Clear");
    clear.set_valign(gtk::Align::Center);
    clear.add_css_class("destructive-action");
    // Nothing to clear is not a reason to draw a button that cannot work, the
    // same rule the plugin gear and the remove-dialog checkbox already follow.
    clear.set_sensitive(bytes > 0 && profile_dir.is_some());
    clear_row.add_suffix(&clear);
    {
        let window = parent.as_ref().clone();
        let dir = profile_dir.clone();
        let name = profile_name.clone();
        let row = clear_row.clone();
        let clear_btn = clear.clone();
        clear.connect_clicked(move |_| {
            let Some(dir) = dir.clone() else { return };
            // A client holding the profile has these files open, and deleting
            // them under it is how a profile gets half-removed while something
            // is still writing to it. ADR-012's lock is what answers "is
            // anything using this", so ask it rather than guessing from a
            // process list.
            if let Err(e) = cordial_shell::profile::acquire(&name) {
                let busy = adw::AlertDialog::builder()
                    .heading("Roblox is open")
                    .body(format!(
                        "Close the client using the {name} profile first, then clear its data.

{e}"
                    ))
                    .build();
                busy.add_response("ok", "OK");
                busy.present(Some(&window));
                return;
            }

            let dialog = adw::AlertDialog::builder()
                .heading("Clear Roblox's stored data?")
                .body(
                    "Its downloaded assets, caches and local storage for this profile are deleted, and downloaded again as you use Roblox.

You stay signed in, and your plugins and what you allowed them to do are untouched.",
                )
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("clear", "Clear");
            dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");

            let row = row.clone();
            let clear_btn = clear_btn.clone();
            let name = name.clone();
            dialog.connect_response(None, move |dialog, response| {
                if response == "clear" {
                    match cordial_shell::profile::clear_engine_data(&dir) {
                        Ok(freed) => {
                            row.set_subtitle(&format!(
                                "Cleared {}. Nothing stored for the {name} profile now.",
                                cordial_shell::profile::human_bytes(freed)
                            ));
                            clear_btn.set_sensitive(false);
                        }
                        // Said on the row rather than only to stderr: this is a
                        // destructive control and "did it work" is the only
                        // question the person pressing it has.
                        Err(e) => row.set_subtitle(&format!("Could not clear it: {e}")),
                    }
                }
                dialog.close();
            });
            dialog.present(Some(&window));
        });
    }
    data_group.add(&clear_row);
    page.add(&data_group);

    page
}

/// A file or folder picker, portal-backed under Flatpak.
///
/// `GtkFileDialog` rather than `GtkFileChooserNative`: the latter is deprecated
/// as of GTK 4.10, which is this project's floor anyway.
pub(crate) fn choose_file(
    parent: &gtk::Window,
    title: &str,
    directory: bool,
    on_chosen: impl Fn(PathBuf) + 'static,
) {
    let dialog = gtk::FileDialog::builder().title(title).modal(true).build();
    let handle = move |result: Result<gtk::gio::File, glib::Error>| {
        // A dismissed dialog arrives here as an error, and it is the common
        // case rather than a fault — reporting it would mean an error message
        // every time somebody changes their mind.
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                on_chosen(path);
            }
        }
    };
    if directory {
        dialog.select_folder(Some(parent), gtk::gio::Cancellable::NONE, handle);
    } else {
        dialog.open(Some(parent), gtk::gio::Cancellable::NONE, handle);
    }
}

/// "Performance" — the two host-side switches, which are not engine flags.
///
/// Separate from the Graphics group above because they are a different kind of
/// thing: the renderer preference is written into `flags.json` and read by the
/// engine, whereas these two are environment the shell sets on the client
/// process and neither of them is Roblox's business.
///
/// **The MangoHUD row is insensitive when MangoHUD is not installed, and says
/// so.** That is the whole reason this function is longer than it looks like it
/// should be. `MANGOHUD=1` with no layer installed is not an error — the client
/// starts, nothing appears, and nothing anywhere says why — so a switch offered
/// unconditionally would be a control that reports success and does not act.
/// The plugins page in this same window exists in its current form because that
/// exact defect shipped here twice.
fn build_performance_group(
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
) -> adw::PreferencesGroup {
    // No group description here or on Audio below: Graphics, the first group
    // on this same page, already says "takes effect the next time you press
    // Roblox", and this page has no search (search_enabled(false)), so every
    // reader meets that sentence before reaching this one. Saying it a third
    // time in a row is the exact restated-boilerplate the Appearance group's
    // own history argues against.
    let group = adw::PreferencesGroup::builder().title("Performance").build();

    let gamemode = adw::SwitchRow::builder()
        .title("Feral GameMode")
        // The caveat stays and the inventory goes. Which four knobs gamemoded
        // turns is its business; that the switch silently does nothing without
        // it installed is the surprise, and a switch that hides that is the
        // "stub that returns success" shape in an interface.
        .subtitle("Performance governor and no screensaver while you play. Needs gamemoded installed.")
        .active(config.borrow().gamemode)
        .build();
    gamemode.set_subtitle_lines(2);
    {
        let config = config.clone();
        let config_path = config_path.clone();
        gamemode.connect_active_notify(move |row| {
            config.borrow_mut().gamemode = row.is_active();
            persist(&config, &config_path);
        });
    }
    group.add(&gamemode);

    // Order has to match ThrottleWhen::index/from_index.
    let throttle_model = gtk::StringList::new(&[
        "When the window is not visible",
        "When the window is not focused",
        "Never",
    ]);
    let throttle = adw::ComboRow::builder()
        .title("Slow the game down in the background")
        // Six sentences became one, and the one that survived is the
        // misleading-if-omitted half: **Roblox is told the window lost focus
        // whichever option is chosen**, so "Never" does not mean the engine
        // never learns it is in the background. Someone who read only a tidy
        // label would pick Never expecting that and be wrong. The rest -- that
        // the three options trade battery against a second-monitor window
        // staying live, and why "not visible" is the default -- is a design
        // rationale, and the option names carry enough of it to choose by.
        .subtitle("Roblox is told when the window loses focus either way; this is only what Cordial does about it.")
        .model(&throttle_model)
        .selected(config.borrow().throttle.index())
        .build();
    throttle.set_subtitle_lines(3);
    {
        let config = config.clone();
        let config_path = config_path.clone();
        throttle.connect_selected_notify(move |row| {
            config.borrow_mut().throttle = ThrottleWhen::from_index(row.selected());
            persist(&config, &config_path);
        });
    }
    group.add(&throttle);

    // Order has to match PointerAcceleration::index/from_index.
    //
    // Two options rather than three, and the missing one is "never". While the
    // cursor is unlocked Cordial is handed an absolute position the compositor
    // has already accelerated, so there is no unaccelerated absolute to fall
    // back to and the desktop's setting applies regardless. A "never" entry
    // would be a menu item that silently does nothing, which is the interface
    // shape of a stub that returns success. Naming the default after what
    // actually happens -- the cursor follows the system, the camera does not --
    // says the true thing in the same space.
    let accel_model = gtk::StringList::new(&["Only the cursor", "Cursor and camera"]);
    let accel = adw::ComboRow::builder()
        .title("Use system mouse acceleration settings")
        // The half that is misleading if omitted: this is not an "add
        // acceleration" switch. It decides whether the desktop's own pointer
        // profile reaches the camera, so somebody who has already turned
        // acceleration off system-wide will find it changes only speed. Said
        // here because the obvious reading of the title is that turning it on
        // introduces acceleration from nowhere.
        .subtitle("Camera movement is raw by default, which is what a camera wants. Choosing both follows your desktop pointer profile instead — so if acceleration is already off system-wide, only speed changes.")
        .model(&accel_model)
        .selected(config.borrow().pointer_acceleration.index())
        .build();
    accel.set_subtitle_lines(4);
    {
        let config = config.clone();
        let config_path = config_path.clone();
        accel.connect_selected_notify(move |row| {
            config.borrow_mut().pointer_acceleration =
                crate::shell_config::PointerAcceleration::from_index(row.selected());
            persist(&config, &config_path);
        });
    }
    group.add(&accel);

    let layer = crate::launch::mangohud_layer();
    let mangohud = adw::SwitchRow::builder()
        .title("MangoHUD overlay")
        .subtitle(match &layer {
            Some(path) => format!(
                "Frame rate, frame times and CPU/GPU load, drawn over the game by MangoHUD's \
                 Vulkan layer.\n{}",
                path.display()
            ),
            // Named as the reason the switch is dead, with the fix. A row that
            // was simply greyed out would leave somebody toggling it and
            // wondering what they had done wrong.
            None => format!(
                "Not available: MangoHUD's Vulkan layer is not installed on this machine, so \
                 turning this on would do nothing.\n{}",
                crate::launch::mangohud_install_hint()
            ),
        })
        // A stale `true` in shell.json must not read as on when the layer has
        // since been uninstalled, because at launch it would not be on.
        .active(config.borrow().mangohud && layer.is_some())
        .sensitive(layer.is_some())
        .build();
    mangohud.set_subtitle_lines(4);
    {
        let config = config.clone();
        let config_path = config_path.clone();
        mangohud.connect_active_notify(move |row| {
            config.borrow_mut().mangohud = row.is_active();
            persist(&config, &config_path);
        });
    }
    group.add(&mangohud);

    group
}

/// The entries an output picker should show, and which one is selected.
///
/// Split out from the widget so the awkward case can be checked without a
/// window: **a device that has gone away must keep its row**. Somebody who
/// chose their USB headset and then unplugged it must not open settings, see
/// "System default" selected, close it again, and have quietly lost the
/// choice — which is what a naive "look the name up in the live list" would
/// do. So a stored name the session no longer has is appended as its own
/// entry, labelled as not connected, and selected.
///
/// Returns the labels in row order and the `node.name` behind each row after
/// the leading "System default", which is what `AudioOutput::index_in` and
/// `AudioOutput::from_index` are indexed against.
fn output_picker_rows(
    sinks: &[audio_devices::Sink],
    chosen: &AudioOutput,
) -> (Vec<String>, Vec<String>) {
    let mut labels = vec!["System default".to_string()];
    let mut names = Vec::with_capacity(sinks.len());
    for sink in sinks {
        labels.push(audio_devices::row_label(sink));
        names.push(sink.node_name.clone());
    }

    if !chosen.is_system_default() && !names.iter().any(|n| n == chosen.0.trim()) {
        // Named by its `node.name` rather than by a remembered description,
        // because no description was ever stored -- see `AudioOutput`, which
        // explains why storing one would have been the wrong thing to persist.
        // A routing name is not pretty, and it is the true answer to "which
        // device did I pick?" when the device is not there to say.
        labels.push(format!("{} (not connected)", chosen.0.trim()));
        names.push(chosen.0.trim().to_string());
    }
    (labels, names)
}

/// "Audio" — which output Roblox plays through.
fn build_audio_group(
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
) -> adw::PreferencesGroup {
    // No description: the Graphics group is first on the General page this
    // group is added to, and it already carries "takes effect the next time
    // you press Roblox" -- with no search on this dialog
    // (search_enabled(false)), every reader meets that sentence before this
    // group, so repeating it here is the boilerplate the Appearance group's
    // own history argues against.
    let group = adw::PreferencesGroup::builder().title("Audio").build();

    let sinks = audio_devices::sinks();
    let chosen = config.borrow().audio_output.clone();
    let (labels, names) = output_picker_rows(&sinks, &chosen);

    // Nothing to choose between, and the reason is not the user's fault. An
    // empty list means no PipeWire session was reachable, which is the same
    // condition under which Roblox gets no audio at all -- so the row says
    // that rather than offering a menu with one entry that would not work
    // either.
    if sinks.is_empty() {
        let row = adw::ActionRow::builder()
            .title("Output device")
            .subtitle(
                "No PipeWire audio devices found. Roblox has nowhere to send sound on this \
                 machine either, so this is worth looking into before the setting is.",
            )
            .sensitive(false)
            .build();
        row.set_subtitle_lines(3);
        group.add(&row);
        return group;
    }

    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&label_refs);
    let row = adw::ComboRow::builder()
        .title("Output device")
        // The half that is misleading if omitted, twice over. First: this is
        // Cordial's routing, not Roblox's own device list -- the in-game
        // picker is FMOD's and cannot be filled from here (see `AudioOutput`).
        // Second: "System default" is a standing instruction rather than a
        // snapshot, so somebody who switches their desktop's default while
        // playing will find the game follows, which is what they want and not
        // what a picker usually implies.
        .subtitle(
            "System default follows your desktop's own choice, including when you change it \
             later. Picking a device here sends only Roblox's sound to it.",
        )
        .model(&model)
        .selected(chosen.index_in(&names))
        .build();
    row.set_subtitle_lines(3);

    {
        let config = config.clone();
        let config_path = config_path.clone();
        // Moved into the closure: the model outlives this function and the row
        // is indexed against exactly the list it was built from, including the
        // "not connected" entry. Re-reading the live sinks on selection would
        // reindex against a different list.
        let names = names.clone();
        row.connect_selected_notify(move |row| {
            config.borrow_mut().audio_output = AudioOutput::from_index(row.selected(), &names);
            persist(&config, &config_path);
        });
    }
    group.add(&row);
    group
}

/// "General" — ordinary client settings.
///
/// Resolution and monitor placement are genuine `cordial-run` options and are
/// still not offered here: `CORDIAL_MONITOR` and `CORDIAL_FULLSCREEN` are read
/// only by the X11 backend and do nothing on the Wayland one the launcher asks
/// for, so a row for either would be a control that changes nothing — which is
/// exactly what the Renderer row turned out to be until this change.
/// Where a report goes. One constant, because the Settings row and the page
/// description both name it and two copies drift.
const ISSUES_URL: &str = "https://github.com/luohoa97/cordial/issues/new/choose";

/// The page that turns "it doesn't work" into a report somebody can act on.
///
/// **A terminal is not a reasonable thing to require here.** `cordial-shell
/// --diagnostics` prints the same block and is the right answer for anyone
/// already in a shell, but the people whose reports are hardest to act on are
/// the ones who installed a Flatpak from a link and have never opened one. The
/// button and the flag share `diagnostics::report`, so the two can never drift
/// into telling different stories about the same machine.
///
/// The text is shown as well as copied. Somebody is about to paste it into a
/// public issue and is entitled to read it first -- and to edit out the
/// hostname in `uname -a` if they would rather, which is the argument for a
/// visible block over a button that silently uploads.
fn build_report_page(parent: &impl IsA<gtk::Window>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Report")
        .name("report")
        .icon_name("dialog-warning-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Diagnostics")
        // **One line.** This said four, explaining what the block contains and
        // what it does not -- true, and none of it what somebody opening this
        // page wants. They came to copy the thing. The block is right there and
        // says what it contains by containing it, and the reasoning about what
        // is deliberately absent lives in `diagnostics.rs` next to the code
        // that decides it.
        .description("Paste this into a GitHub issue.")
        .build();

    let text = crate::diagnostics::report();

    // Monospace and selectable: the columns only line up in a fixed-width font,
    // and somebody who wants one line rather than the block should be able to
    // take it without the button's all-or-nothing.
    let view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .cursor_visible(false)
        // **Wrapped, because `uname -a` is longer than any settings window.**
        // Without this the System line ran off the right edge with no scrollbar
        // and no way to see the rest -- the Copy button had the whole string,
        // but somebody checking what they were about to paste into a public
        // issue could not read the one line most likely to make them think
        // twice. `WordChar` rather than `Word`: a kernel version has no spaces
        // to break at.
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    view.buffer().set_text(&text);
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&view));
    frame.set_margin_top(6);

    let copy = gtk::Button::with_label("Copy");
    copy.set_valign(gtk::Align::Center);
    let copied = text.clone();
    copy.connect_clicked(move |b| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&copied);
            // The label is the confirmation. A toast needs an overlay this page
            // does not own, and a button that looks identical after a press is
            // one people press three times.
            b.set_label("Copied");
            let b = b.clone();
            glib::timeout_add_seconds_local_once(2, move || b.set_label("Copy"));
        }
    });

    let row = adw::ActionRow::builder().title("Copy diagnostics").build();
    row.add_suffix(&copy);
    group.add(&row);
    group.add(&frame);
    page.add(&group);

    let where_group = adw::PreferencesGroup::builder()
        .title("Bugs and feature requests")
        // The row below says where it goes and the form asks for the block. A
        // paragraph here repeated both.
        .build();

    // A link row rather than prose with a URL in it: this window is the last
    // place somebody is before they give up, and it should take one press to
    // get from here to the form.
    let issues = adw::ActionRow::builder()
        .title("Open an issue")
        .subtitle(ISSUES_URL.trim_start_matches("https://"))
        .activatable(true)
        .build();
    // `go-next-symbolic`, and checked on disk rather than guessed: the first
    // attempt used `external-link-symbolic`, which is in no icon theme here and
    // rendered as the missing-image glyph. `go-next-symbolic` ships in Adwaita
    // (`symbolic/actions/`) and is the affordance libadwaita already uses for a
    // row that takes you somewhere.
    issues.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    // **`GtkUriLauncher`, not `cordial_plugins::urlopen`, and the difference is
    // focus.** Both reach `org.freedesktop.portal.OpenURI`, so both work inside
    // the Flatpak sandbox where a spawned `xdg-open` would not. But `urlopen`
    // is the *plugin* path: a plugin has no window, so it passes an empty
    // parent handle and no activation token, and GNOME's focus-stealing
    // prevention answers that by declining to raise the browser -- you get a
    // "Firefox is ready" notification instead of the page you asked for.
    //
    // This row has a window to offer. `UriLauncher::launch` takes the parent
    // and hands the portal what it needs to raise the browser properly, which
    // is the whole reason to use the GTK wrapper rather than call the portal by
    // hand a second time. `urlopen` is left exactly as it is: it is correct for
    // the caller it has, which never has a window (ADR-007).
    let parent_for_link = parent.as_ref().clone();
    issues.connect_activated(move |_| {
        gtk::UriLauncher::new(ISSUES_URL).launch(
            Some(&parent_for_link),
            gtk::gio::Cancellable::NONE,
            |result| {
                if let Err(e) = result {
                    eprintln!("[cordial] could not open the issue tracker: {e}");
                }
            },
        );
    });
    where_group.add(&issues);
    page.add(&where_group);

    page
}

fn build_general_page(
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("General")
        .name("general")
        .icon_name("preferences-other-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Graphics")
        // "The choice is settled before Roblox loads its renderer" is why it
        // needs a relaunch; the user only needs the "needs a relaunch".
        .description("Takes effect the next time you press Roblox.")
        .build();

    // **Not backed by `FStringDebugGraphicsPreferredBackend` any more.** That
    // flag was measured on 2026-08-03 and changes nothing: deliberate rubbish,
    // five GLES spellings and the "confirmed" `"Vulkan"` all produced the same
    // `pack vulkan_mobile`, because Vulkan is what the engine takes regardless.
    // The row it backed looked like a setting and was one only by coincidence.
    //
    // What decides the backend is whether Cordial offers a Vulkan loader at all,
    // since `libroblox.so` links none and `dlopen`s it. So this writes
    // `shell.json` and `launch.rs` passes `CORDIAL_GRAPHICS`; the runtime's
    // `graphics` module has the full measurement.
    let model = gtk::StringList::new(&["Automatic", "Vulkan", "OpenGL ES"]);
    let selected = match config.borrow().graphics.as_str() {
        "vulkan" => 1,
        "gles" => 2,
        _ => 0,
    };
    let row = adw::ComboRow::builder()
        .title("Renderer")
        // The per-option mechanics (Vulkan keeps the engine's own GLES3
        // fallback; OpenGL ES withholds the Vulkan loader and has no fallback
        // of its own; Automatic is the only value a plugin may override,
        // because an absent `CORDIAL_GRAPHICS` is what leaves it room) are in
        // `launch.rs` beside the code that sends them. What a user needs is
        // which one to leave it on.
        .subtitle("Automatic lets Roblox choose, and is the only setting a plugin may override.")
        .model(&model)
        .selected(selected)
        .build();
    row.set_subtitle_lines(2);

    {
        let config = config.clone();
        let config_path = config_path.clone();
        row.connect_selected_notify(move |row| {
            let value = match row.selected() {
                1 => "vulkan",
                2 => "gles",
                _ => "automatic",
            };
            config.borrow_mut().graphics = value.to_string();
            persist(&config, &config_path);
        });
    }

    group.add(&row);

    // Order has to match GraphicsOptimization::index/from_index.
    //
    // Five entries rather than a device dropdown beside a CPU dropdown. The
    // reasoning is in `GraphicsOptimization`'s own doc: they are two
    // parameters and one question, and a cross product of them would be six
    // rows for combinations nobody has measured. Each label here names what it
    // actually sets.
    // **Short enough that the row can show which one is selected.** These read
    // "Windows PC, more CPU cores" and the row rendered "Windows PC, more..."
    // -- the ellipsis landing exactly on the word that distinguishes the three
    // Windows PC entries from each other, so the control could not be read at
    // a glance and the two core-count variants were indistinguishable.
    // Reported alongside the same fault on Frame pacing.
    //
    // The device and the core count are two facts, so they are separated by a
    // dash rather than run together into a sentence: the eye can find "more
    // cores" at the end of a short label in a way it cannot find "more CPU
    // cores" at the end of a long one.
    let optimisation_model = gtk::StringList::new(&[
        "Windows PC",
        "Roblox app",
        "Android tablet",
        "Windows PC - more cores",
        "Windows PC - fewer cores",
    ]);
    let optimisation = adw::ComboRow::builder()
        .title("Graphics optimisation")
        // **The honest half, and the half that would be misleading if left
        // out.** It would be easy to write this row as though picking a
        // different device made Roblox draw less, and nothing has shown that.
        // What is established is what each identity makes roblox.com serve
        // (`native/init_params.cpp`'s `device_identity`) and that `isTablet` is
        // a field the engine reads; the step from there to a frame rate is an
        // inference, and a settings page is the last place it should be stated
        // as fact.
        // Trimmed to three lines from five, for the same reason as the labels:
        // a subtitle long enough to squeeze the value out of the row is not
        // helping anybody read the row. The parts kept are the two that change
        // what somebody picks -- that Android tablet has been reported to
        // break things, and that none of this has been measured to change the
        // frame rate. The rest, including which identity roblox.com serves
        // what to, is in `GraphicsOptimization`'s own doc beside the code.
        .subtitle(
            "What Cordial tells Roblox it is, and how many cores the engine may use. Android \
             tablet claims a mobile screen and has been reported to break some features. None \
             of these has been measured to change the frame rate.",
        )
        .model(&optimisation_model)
        .selected(config.borrow().graphics_optimization_mode.index())
        .build();
    optimisation.set_subtitle_lines(3);
    {
        let config = config.clone();
        let config_path = config_path.clone();
        optimisation.connect_selected_notify(move |row| {
            config.borrow_mut().graphics_optimization_mode =
                crate::shell_config::GraphicsOptimization::from_index(row.selected());
            persist(&config, &config_path);
        });
    }
    group.add(&optimisation);

    // Order has to match PresentMode::index/from_index.
    //
    // **Automatic is last, not first, and that is the opposite of the Renderer
    // row above.** There it is first because it is the recommended value; here
    // the recommended value is FIFO, and putting the one that means "no
    // opinion" at the top of a list whose whole point is a power default would
    // invite people to leave it there. It stays on the list because an absent
    // `CORDIAL_PRESENT_MODE` is the only state in which a plugin's
    // `CordialPresentMode` counts, and removing the row's only route to that
    // state would silently retire a capability ADR-020 documents.
    // **Short enough to read in the row, which the first draft was not.**
    // An `AdwComboRow` puts the selected value in whatever space the title and
    // subtitle leave and ellipsises it: "Uncapped, no tearing (Mailbox)" came
    // out as "Uncapped, no tearing (..." with the one word that distinguishes
    // it cut off. Reported with a screenshot. These are the names the request
    // used, they fit, and what each means is one line below instead of five
    // squeezed into the row.
    //
    // Mailbox first, because it is the default and the recommended value, and
    // the Renderer row above puts its own default first for that reason. FIFO
    // was briefly the default and briefly first; it is neither now, and the
    // subtitle names the cost rather than leaving somebody to find it the way
    // the first reporter did.
    let present_model = gtk::StringList::new(&["Mailbox", "FIFO", "Immediate", "Automatic"]);
    let present = adw::ComboRow::builder()
        .title("Frame pacing")
        // What a user can act on, in the order they need it: what the default
        // does, what changing it costs, and the one caveat that would
        // otherwise read as a bug. The mechanics -- that this is
        // `VkSwapchainCreateInfoKHR::presentMode`, that FIFO is the only mode
        // the specification guarantees, that a mode the driver does not
        // advertise leaves the engine's own choice standing -- are in
        // `shell_config::PresentMode` and in the runtime beside the code.
        .subtitle(
            "Mailbox is responsive and costs battery. FIFO matches your display and saves power, \
             at the cost of a floatier mouse.",
        )
        .model(&present_model)
        .selected(config.borrow().present_mode.index())
        .build();
    present.set_subtitle_lines(2);
    {
        let config = config.clone();
        let config_path = config_path.clone();
        present.connect_selected_notify(move |row| {
            config.borrow_mut().present_mode =
                crate::shell_config::PresentMode::from_index(row.selected());
            persist(&config, &config_path);
        });
    }
    group.add(&present);

    page.add(&group);
    page.add(&build_audio_group(config.clone(), config_path.clone()));
    page.add(&build_performance_group(config, config_path));
    page
}

/// "Plugins" — what is installed, and what this profile has granted it.
///
/// Read from `manifest::discover` and from the selected profile's own grants
/// file, both at the moment the window is built. Grants stopped being global in
/// ADR-013 precisely because an approval given in a throwaway profile was
/// silently in force in the profile someone plays on, so a page that showed one
/// set for all profiles would be displaying the bug that change removed.
///
/// **The switch writes to disk, and is not the grants file.** It used to write
/// to an in-memory list nothing ever read, so pressing it changed nothing
/// anywhere — a control that reports success and does not act, which is the
/// shape AGENTS.md rules out for stubs and is no better in an interface. It now
/// goes through `cordial_plugins::enablement`, which keeps the answer in the
/// profile beside the grants and deliberately apart from them: disabling must
/// not revoke, or turning something back on would cost every approval already
/// given, and the cheaper response to that price is to leave a suspect plugin
/// enabled.
///
/// **It takes effect at the next launch, and the page says so.** Nothing here
/// can stop a plugin that is already running: the plugin host is not wired to a
/// running client at all yet, and the shell is a separate process from the
/// instance in any case. A toggle that looked immediate and was not is exactly
/// the small lie this project keeps writing ADRs about, so the wording is the
/// weaker claim.
/// A short, user-facing sentence for one capability, from
/// [`Capability::consequence`].
///
/// **This used to be a second table, and it had already drifted.** It
/// described `flags.write` as "Contribute FastFlag overrides that take effect
/// at the next launch", which is what the capability was called and not what
/// it does — ADR-020 records that the same capability sets
/// `CordialGraphicsBackend` and `CordialPresentMode`, so a user reading that
/// line had been told something technically true and materially misleading.
/// The sentences now live beside the enum, where a variant cannot be added
/// without one and where the install prompt and this page read the same words.
/// The wrapper stays because two call sites want it and a rename in the
/// protocol crate should not reach into widget-building code.
fn capability_description(cap: Capability) -> &'static str {
    cap.consequence()
}

/// A capability switch's subtitle: what granting it does, and — if `denials`
/// has ever recorded this exact plugin trying and failing — the one line that
/// wires `Broker::denials()` to somewhere a person can actually read it.
///
/// **This is the two-minute fix `broker.rs`'s own doc comment describes.**
/// Without it, "not allowed" and "allowed and simply never called yet" look
/// identical: both are a switch sitting off. `denials` only ever records a
/// capability the plugin *asked for and was refused*, so this line is never
/// speculative — it says a call was actually made and actually failed, which
/// is a stronger and more specific fact than the row's own off state already
/// shows.
fn capability_row_subtitle(cap: Capability, refused: bool) -> String {
    if refused {
        format!(
            "{}\n\nCordial has refused this at least once because it was not granted.",
            capability_description(cap)
        )
    } else {
        capability_description(cap).to_string()
    }
}

/// Recompute and set `expander`'s subtitle from what is actually on disk,
/// rather than from whatever a closure happened to capture when the row was
/// built. Both the enable switch and every capability switch below call this
/// after they write, so the summary line never lags behind the file it
/// describes — the same reasoning the single switch this replaced already
/// had, extended to cover more than one control changing the same plugin.
fn refresh_plugin_subtitle(expander: &adw::ExpanderRow, plugin: &Plugin, profile_dir: Option<&PathBuf>, enabled: bool) {
    let granted = profile_dir.map(|dir| grants::load(&grants::path_in(dir))).unwrap_or_default();
    expander.set_subtitle(&capability_summary(plugin, granted.get(&plugin.manifest.id), enabled));
}

/// One installed plugin's row: enable switch, one capability switch per
/// capability it requested, and a Remove button in the header. Factored out
/// of `build_plugins_page` so the exact same row — grants wiring, remove
/// wiring, everything — is what a freshly installed plugin gets too, rather
/// than a second, drifting copy of this logic living in the install button's
/// callback.
fn build_plugin_row(
    parent: &adw::PreferencesDialog,
    plugin: &Plugin,
    profile_dir: Option<&PathBuf>,
    root: &Path,
    group: &adw::PreferencesGroup,
    tier: Tier,
) -> adw::ExpanderRow {
    let title = if plugin.manifest.name.is_empty() {
        plugin.manifest.id.clone()
    } else {
        plugin.manifest.name.clone()
    };
    let id = plugin.manifest.id.clone();
    let enabled = profile_dir.is_none_or(|dir| enablement::is_enabled(dir, &id));

    let expander = adw::ExpanderRow::builder()
        .title(title.clone())
        .subtitle(capability_summary(plugin, profile_dir.map(|dir| grants::load(&grants::path_in(dir))).unwrap_or_default().get(&id), enabled))
        .build();
    expander.set_subtitle_lines(2);

    // A freshly installed plugin has never been granted anything (ADR-003's
    // default deny), so this button is reachable — and needed — the moment
    // the row appears, not only after a page reload.
    //
    // Not offered on a built-in one. Its directory is installed alongside the
    // binary and is not the user's to delete; a Remove that failed with a
    // permission error would be a control that looks live and refuses, and the
    // thing the user actually wants there is the switch below.
    if tier == Tier::BuiltIn {
        let tag = gtk::Label::new(Some("Built-in"));
        tag.add_css_class("dim-label");
        tag.add_css_class("caption");
        tag.set_valign(gtk::Align::Center);
        expander.add_suffix(&tag);
    }
    // The gear, and only when the plugin declares something to configure —
    // GNOME's Extensions app does the same, and an extension with nothing to
    // set simply has no gear there. A permanently insensitive button would
    // read as broken rather than as "nothing here". ADR-020.
    //
    // Placed before Remove so the order down the row is configure-then-destroy,
    // and so the destructive control stays furthest from the one people press
    // often.
    // **The dialog itself, passed as one.** This used to take a
    // `&impl IsA<gtk::Window>` and downcast it to the preferences window to
    // reach `push_subpage`. That worked only because every caller happened to
    // pass the preferences window under a wider static type -- and it would
    // have failed *silently* the moment one did not, dropping the gear from
    // every plugin row with no error anywhere. An `AdwPreferencesDialog` is
    // not a `GtkWindow` at all, so the migration would have done exactly that.
    {
        let window = parent.clone();
        // `false`: Cordial has no plugin update detection, so nothing can
        // truthfully ask for the accent-coloured gear yet. Passing the literal
        // rather than omitting the argument keeps the state visible at the one
        // call site that will have to change when updates land.
        if let Some(gear) =
            cordial_shell::plugin_preferences::gear_for(&window, plugin, profile_dir, false)
        {
            expander.add_suffix(&gear);
        }
    }

    let remove = gtk::Button::from_icon_name("user-trash-symbolic");
    remove.set_valign(gtk::Align::Center);
    remove.add_css_class("flat");
    remove.set_tooltip_text(Some("Remove this plugin"));
    if tier == Tier::User {
        expander.add_suffix(&remove);
    }
    {
        let window = parent.clone();
        let root = root.to_path_buf();
        let id = id.clone();
        let title = title.clone();
        let group = group.clone();
        let expander = expander.clone();
        let window = window.clone();
        let profile_dir_for_remove = profile_dir.cloned();
        remove.connect_clicked(move |_| {
            // What the *profile* remembers, which uninstalling does not touch:
            // the settings chosen, the capabilities allowed, whether it was
            // switched off. The plugin's own files -- manifest, entry module,
            // assets, the flag layer it wrote -- all live in its installed
            // directory and go with it either way, so none of them are counted
            // here and none of them are what this checkbox is about.
            let footprint = profile_dir_for_remove
                .as_ref()
                .map(|dir| plugin_data::footprint(dir, &id))
                .unwrap_or_default();

            let dialog = adw::AlertDialog::builder()
                .heading(format!("Remove {title}?"))
                .body(if footprint.is_empty() {
                    "Its files are deleted. This profile has nothing saved for it.".to_string()
                } else {
                    "Its files are deleted. What you allowed it to do, and anything it saved, \
                     are kept for this profile — install it again later and it picks up where \
                     it left off."
                        .to_string()
                })
                .build();

            // Only when there is something to delete. A permanently unticked
            // checkbox over nothing reads as a control that does not work,
            // which is the same reason the gear above appears only for a
            // plugin that declares preferences.
            let also_delete = (!footprint.is_empty()).then(|| {
                let label = if footprint.bytes > 0 {
                    format!(
                        "Also delete its settings and permissions ({})",
                        plugin_data::human_bytes(footprint.bytes)
                    )
                } else {
                    // Grants and the off switch are single entries in documents
                    // shared by every plugin, so they weigh nothing worth
                    // quoting -- and a "0 bytes" on the label would read as
                    // "this does nothing".
                    "Also delete its settings and permissions".to_string()
                };
                let check = gtk::CheckButton::with_label(&label);
                // Off by default. The destructive half of a destructive dialog
                // does not get to be pre-ticked.
                check.set_active(false);
                check.set_tooltip_text(Some(
                    "Installing it again will ask for permission from scratch.",
                ));
                check.set_margin_top(6);
                dialog.set_extra_child(Some(&check));
                check
            });
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("remove", "Remove");
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");

            let root = root.clone();
            let id = id.clone();
            let group = group.clone();
            let expander = expander.clone();
            let profile_for_forget = profile_dir_for_remove.clone();
            dialog.connect_response(None, move |dialog, response| {
                if response == "remove" {
                    match unpack::uninstall(&root, &id) {
                        Ok(()) => group.remove(&expander),
                        Err(e) => eprintln!("shell: could not remove plugin {id}: {e}"),
                    }
                    // After the files, and only if asked. Ordered this way so a
                    // failure to delete the plugin does not silently take the
                    // user's settings with it -- uninstall is the thing they
                    // pressed the button for, and this is the extra.
                    if also_delete.as_ref().is_some_and(|c| c.is_active()) {
                        if let Some(dir) = profile_for_forget.as_ref() {
                            if let Err(e) = plugin_data::forget(dir, &id) {
                                eprintln!("shell: could not delete {id}'s saved data: {e}");
                            }
                        }
                    }
                }
                dialog.close();
            });
            dialog.present(Some(&window));
        });
    }

    // The switch that used to be the whole row is now one child row among
    // several, so switching a plugin off sits beside — and does not
    // disturb — the capability switches it does not revoke.
    let enable_row = adw::SwitchRow::builder().title("Enabled").active(enabled).build();
    // Nothing here can stop a plugin that is already running: the plugin host
    // is not wired to a running client, and the shell is a separate process
    // from the instance in any case. The page used to say so in a paragraph at
    // the top of it; one line on the control it is about is where a user will
    // actually meet it.
    enable_row.set_subtitle("Takes effect the next time you press Roblox.");
    {
        let plugin = plugin.clone();
        let dir = profile_dir.cloned();
        let id = id.clone();
        let expander = expander.clone();
        enable_row.connect_active_notify(move |row| {
            let on = row.is_active();
            if let Some(dir) = &dir {
                if let Err(e) = enablement::set_enabled(dir, &id, on) {
                    // Reported rather than swallowed, and the row is put back
                    // to what is actually on disk: a switch that stays where
                    // the user left it while the file says otherwise is a lie
                    // the user has no way to see.
                    eprintln!("shell: could not record that {id} is {}: {e}", if on { "on" } else { "off" });
                    row.set_active(!on);
                    return;
                }
            }
            refresh_plugin_subtitle(&expander, &plugin, dir.as_ref(), on);
        });
    }
    expander.add_row(&enable_row);

    // One switch per capability the plugin actually requested — offering
    // one for something it never asked for would be a control with nothing
    // to grant. Collected as they are built so a built-in's first-appearance
    // consent prompt, below, can flip them on accept rather than duplicating
    // the grant-and-refresh logic each one already carries in its own
    // `connect_active_notify`.
    let mut cap_rows: Vec<(Capability, adw::SwitchRow)> = Vec::new();
    if plugin.requested.is_empty() {
        expander.add_row(&adw::ActionRow::builder().title("Requests no capabilities").build());
    } else if let Some(dir) = profile_dir {
        let granted_here = grants::load(&grants::path_in(dir)).get(&id).cloned().unwrap_or_default();
        // Read once per row build, the same moment grants and enablement
        // already are — see the Plugins page's own doc comment for why "as of
        // when the window opened" is the posture every control here takes.
        let refused_here = denials::load(&denials::path_in(dir)).get(&id).cloned().unwrap_or_default();
        for &cap in &plugin.requested {
            let cap_row = adw::SwitchRow::builder()
                .title(cap.name())
                .subtitle(capability_row_subtitle(cap, refused_here.contains(&cap)))
                .active(granted_here.contains(&cap))
                .build();
            cap_row.set_subtitle_lines(3);
            expander.add_row(&cap_row);
            cap_rows.push((cap, cap_row.clone()));

            let dir = dir.clone();
            let id = id.clone();
            let plugin = plugin.clone();
            let expander = expander.clone();
            let enable_row = enable_row.clone();
            cap_row.connect_active_notify(move |row| {
                let on = row.is_active();
                if let Err(e) = grants::set(&grants::path_in(&dir), &id, cap, on) {
                    eprintln!("shell: could not record {id}'s {} grant: {e}", cap.name());
                    row.set_active(!on);
                    return;
                }
                if on {
                    // A denial recorded before this grant existed is no
                    // longer telling the user anything true -- the plugin
                    // will succeed the next time it asks -- so it is cleared
                    // the moment the capability is, rather than left to
                    // describe a state that has already changed underneath
                    // it. Best effort: a row that fails to update its own
                    // subtitle text is a cosmetic miss, not a reason to
                    // refuse a grant that was otherwise recorded correctly.
                    if let Err(e) = denials::clear(&denials::path_in(&dir), &id, cap) {
                        eprintln!("shell: could not clear {id}'s recorded {} denial: {e}", cap.name());
                    }
                    row.set_subtitle(&capability_row_subtitle(cap, false));
                }
                refresh_plugin_subtitle(&expander, &plugin, Some(&dir), enable_row.is_active());
            });
        }
    } else {
        // No resolvable profile directory: there is nowhere a grant could be
        // written, so say that rather than offering switches that could not
        // persist anything a user did with them.
        expander.add_row(
            &adw::ActionRow::builder()
                .title("No profile to grant against")
                .subtitle("This profile's directory could not be resolved.")
                .build(),
        );
    }

    if tier == Tier::BuiltIn {
        maybe_prompt_builtin_consent(parent, plugin, profile_dir, &expander, &enable_row, &cap_rows);
    }

    expander
}

/// Ask a built-in plugin's first-appearance consent question, if this profile
/// has not already been asked and there is anything to ask about.
///
/// **When this asks, and why not on enabling instead.** A user-installed
/// plugin has one clear moment to ask: the install button click, which
/// happens exactly once and starts the plugin switched off (`consent::
/// starts_disabled`) until somebody turns it on. A built-in has no such
/// moment and, unless it is one of `enablement::SHIPS_DISABLED`, already
/// ships *enabled* — so tying this to the Enabled switch's off-to-on
/// transition would never fire for the common case, because there is no such
/// transition: the plugin was already on the first time this row was ever
/// built, silently running with nothing granted, which is the exact bug this
/// change exists to close. Asking the first time this profile's Plugins page
/// renders the row is the moment that actually happens for every profile,
/// old or new.
///
/// **This only ever touches capabilities, never `enablement`.** Built-ins are
/// governed by `SHIPS_DISABLED`, a decision this file does not reopen (see
/// the commit this landed in): consent here answers "may it do X", not
/// "does it run at all", and conflating the two would make this prompt a
/// second, competing policy for which built-ins start off.
fn maybe_prompt_builtin_consent(
    parent: &adw::PreferencesDialog,
    plugin: &Plugin,
    profile_dir: Option<&PathBuf>,
    expander: &adw::ExpanderRow,
    enable_row: &adw::SwitchRow,
    cap_rows: &[(Capability, adw::SwitchRow)],
) {
    // Nowhere to record having asked, and nowhere to grant into — the same
    // posture the capability switches above already take for an
    // unresolvable profile.
    let Some(dir) = profile_dir else { return };
    let seen_path = consent::seen_path_in(dir);
    let id = plugin.manifest.id.clone();
    if consent::has_been_asked(&seen_path, &id) {
        return;
    }
    // `Silent` means no code and no capabilities requested -- nothing to run
    // and nothing it could reach. `verdict` is a pure read of the manifest,
    // so there is nothing worth persisting for this case; the check above
    // costs one small file read on every Settings page open, which is the
    // same price every other row on this page already pays for grants and
    // enablement.
    let consent::Verdict::Ask(prompt) = consent::verdict(plugin) else { return };

    let dialog = adw::AlertDialog::builder()
        .heading(prompt.heading())
        .body(consent_body_for_builtin(&prompt))
        .build();
    dialog.add_response("skip", "Not now");
    dialog.add_response("allow", "Allow");
    dialog.set_response_appearance("allow", adw::ResponseAppearance::Suggested);
    // The safe answer survives Escape, exactly as it does for a user install
    // — ADR-003's default deny has to hold even when the dialog is dismissed
    // rather than answered.
    dialog.set_default_response(Some("skip"));
    dialog.set_close_response("skip");

    let dir = dir.clone();
    let cap_rows: Vec<(Capability, adw::SwitchRow)> = cap_rows.to_vec();
    let expander = expander.clone();
    let enable_row = enable_row.clone();
    let plugin = plugin.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "allow" {
            // Each row's own `connect_active_notify` already does exactly
            // what accepting this prompt means -- write the grant, clear any
            // stale denial, refresh the summary line -- so flipping the
            // switch is the grant, not a shortcut around it. This is the
            // same mechanism the install flow uses, reached through the row
            // that already exists here instead of a second copy of the
            // grants::set loop that flow needs because it has no rows yet.
            for (_, row) in &cap_rows {
                row.set_active(true);
            }
            refresh_plugin_subtitle(&expander, &plugin, Some(&dir), enable_row.is_active());
        }
        if let Err(e) = consent::mark_asked(&seen_path, &id) {
            eprintln!("shell: could not record that {id} has been asked about: {e}");
        }
        dialog.close();
    });
    dialog.present(Some(parent));
}

/// The body of a built-in's first-appearance prompt: the same itemised
/// effects `consent_body` lists for a user install, closed with a line that
/// is actually true of a built-in.
///
/// **Why this is not `consent_body`.** `Prompt::footer` says "It starts
/// switched off" whenever the plugin has code — right for something a user
/// just chose to install, and wrong here: a built-in ships enabled by default
/// unless it is one of `enablement::SHIPS_DISABLED`, and by the time this
/// profile's Plugins page is first opened it may already have been running,
/// silently denied, for as long as the profile has existed. Reusing that
/// sentence verbatim would tell the user something false about a plugin that
/// might be running at that exact moment — the failure AGENTS.md's rule about
/// a comment that lies exists to rule out, applied here to a dialog instead.
fn consent_body_for_builtin(prompt: &consent::Prompt) -> String {
    let mut out = String::new();
    for effect in &prompt.effects {
        out.push_str("• ");
        out.push_str(effect.description);
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(
        "This comes with Cordial. Its own Enabled switch below governs whether it runs at all, \
         separately from this — allowing something here does not turn it on, and refusing it \
         does not turn it off. You can change any of these permissions at any time.",
    );
    out
}

/// "Install a plugin" — a `.tar.zst` picked from disk, unpacked the same
/// hardened way an index install would be (ADR-014), with no index involved.
///
/// This is the piece that used to be missing entirely: `cordial-plugins`
/// shipped a fully tested unpacker (`unpack::install_local`) that nothing in
/// the shell ever called, so "installing a plugin" meant unpacking a `tar`
/// archive by hand into `~/.local/share/cordial/plugins/` — which is not a
/// user-facing installer, it is the absence of one. Fetching from a remote
/// index — the marketplace half of ADR-014 — still needs a network fetcher
/// this change does not add; this is the half that was reachable without one.
/// `parent` and `dialog` are both here on purpose and are not interchangeable.
/// `GtkFileDialog` needs a real `GtkWindow` to be modal against; the plugin
/// rows need the `AdwPreferencesDialog` because that is what owns the subpages
/// a plugin's gear pushes. An `AdwDialog` is not a `GtkWindow`, so one handle
/// cannot serve both.
fn build_install_group(
    parent: &impl IsA<gtk::Window>,
    dialog: &adw::PreferencesDialog,
    root: &Path,
    profile_dir: Option<PathBuf>,
    installed_group: &adw::PreferencesGroup,
) -> adw::PreferencesGroup {
    let row_dialog = dialog.clone();
    let group = adw::PreferencesGroup::builder()
        .title("Install from a file")
        .description("Installing a plugin does not allow it to do anything. You choose that afterwards.")
        .build();

    let row = adw::ActionRow::builder().title("Plugin archive (.tar.zst)").build();
    row.set_subtitle_lines(3);
    let button = gtk::Button::with_label("Choose file…");
    button.set_valign(gtk::Align::Center);
    row.add_suffix(&button);

    let window = parent.as_ref().clone();
    let root = root.to_path_buf();
    let installed_group = installed_group.clone();
    let row_for_status = row.clone();
    button.connect_clicked(move |_| {
        let picker_window = window.clone();
        let window = window.clone();
        let root = root.clone();
        let profile_dir = profile_dir.clone();
        let installed_group = installed_group.clone();
        let row = row_for_status.clone();
        let row_dialog = row_dialog.clone();
        choose_file(&picker_window, "Choose a plugin archive (.tar.zst)", false, move |path| {
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    row.set_subtitle(&format!("Could not read {}: {e}", path.display()));
                    return;
                }
            };
            match unpack::install_local(&bytes, &root) {
                Ok((plugin, _dir)) => {
                    // ADR-021's first consent rule, and the one the other two
                    // depend on: a plugin with nothing to run and nothing it
                    // could reach is installed without interrupting anybody.
                    // If every import prompts, the prompt means nothing by the
                    // third one — and then it is not protecting the install
                    // that mattered.
                    match consent::verdict(&plugin) {
                        consent::Verdict::Silent => {
                            row.set_subtitle(&format!(
                                "Installed {}. It contains no code and asks for no permissions.",
                                display_name(&plugin)
                            ));
                        }
                        consent::Verdict::Ask(prompt) => {
                            let dialog = adw::AlertDialog::builder()
                                .heading(prompt.heading())
                                .body(consent_body(&prompt))
                                .build();
                            dialog.add_response("skip", "Not now");
                            dialog.add_response("allow", "Allow");
                            dialog.set_response_appearance("allow", adw::ResponseAppearance::Suggested);
                            // The safe answer is the default and the close
                            // action, so dismissing the dialog grants nothing
                            // — ADR-003's default deny has to survive somebody
                            // pressing Escape.
                            dialog.set_default_response(Some("skip"));
                            dialog.set_close_response("skip");

                            let profile_for_dialog = profile_dir.clone();
                            let id = plugin.manifest.id.clone();
                            let requested: Vec<_> = plugin.requested.iter().copied().collect();
                            let row_for_dialog = row.clone();
                            let name = display_name(&plugin);
                            dialog.connect_response(None, move |dialog, response| {
                                if response == "allow" {
                                    if let Some(profile_dir) = profile_for_dialog.as_ref() {
                                        let path = grants::path_in(profile_dir);
                                        for cap in &requested {
                                            if let Err(e) = grants::set(&path, &id, *cap, true) {
                                                eprintln!(
                                                    "shell: could not record {id}'s {} grant: {e}",
                                                    cap.name()
                                                );
                                            }
                                        }
                                    }
                                    row_for_dialog.set_subtitle(&format!(
                                        "Installed {name} and allowed what it asked for. It is \
                                         switched off until you turn it on."
                                    ));
                                } else {
                                    row_for_dialog.set_subtitle(&format!(
                                        "Installed {name}. It was allowed nothing; choose what \
                                         it may do on the Plugins page."
                                    ));
                                }
                                dialog.close();
                            });
                            dialog.present(Some(&window));
                        }
                    }
                    park_new_plugin_disabled(profile_dir.as_ref(), &plugin);
                    let new_row = build_plugin_row(
                        &row_dialog,
                        &plugin,
                        profile_dir.as_ref(),
                        &root,
                        &installed_group,
                        Tier::User,
                    );
                    installed_group.add(&new_row);
                }
                Err(e) => row.set_subtitle(&format!("Could not install {}: {e}", path.display())),
            }
        });
    });

    group.add(&row);
    group
}

/// The confirmation text for a marketplace install: every step the resolver
/// chose, dependencies included, and exactly what each one would be granted.
///
/// Built once, before the dialog exists, and never held onto — ADR-014 is
/// explicit that "resolution and approval are two calls, not one" because a
/// user cannot approve a plan they have not been shown, and this is the
/// showing. What actually grants anything is the dialog's response handler
/// below, which resolves a second time once the user has agreed, rather than
/// carrying this borrow of the index across the time a modal dialog is open.
fn plan_confirmation_text(plan: &resolve::Plan<'_>) -> String {
    let mut out = String::new();
    for step in &plan.steps {
        out.push_str(&format!("{} {}\n", step.id, step.version));
        if step.capabilities.is_empty() {
            out.push_str("    Asks for no permissions.\n");
        }
        // The sentences, not the wire names. A dialog reading "requests:
        // flags.write, presence.set" is accurate, unreadable, and answered
        // yes by everybody — ADR-021 argues the wording is the whole job, and
        // `Capability::consequence` is where it is kept so it cannot drift
        // from the enum.
        for cap in &step.capabilities {
            out.push_str("    • ");
            out.push_str(cap.consequence());
            out.push('\n');
        }
        out.push('\n');
    }
    format!(
        "Installing this grants each plugin below exactly what is listed, in this profile only. \
         Nothing is granted anywhere else, and nothing is granted at all if you cancel. Anything \
         with code in it starts switched off.\n\n{}",
        out.trim_end()
    )
}

/// A plugin's display name, falling back to its id — the same fallback the
/// consent prompt makes, kept in one place so a row and a dialog cannot
/// disagree about what a plugin is called.
fn display_name(plugin: &Plugin) -> String {
    if plugin.manifest.name.is_empty() {
        plugin.manifest.id.clone()
    } else {
        plugin.manifest.name.clone()
    }
}

/// The body of the install prompt for one plugin: what it will be able to do,
/// in sentences, and the line saying it starts switched off.
///
/// Pure, so it can be tested without a display connection — the dialog around
/// it cannot be, and the part worth testing is the text.
fn consent_body(prompt: &consent::Prompt) -> String {
    let mut out = String::new();
    for effect in &prompt.effects {
        out.push_str("• ");
        out.push_str(effect.description);
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(prompt.footer());
    out
}

/// Record that a freshly installed plugin with code starts switched off.
///
/// **Consent and enablement are two acts, and this is the second one not
/// happening.** Before this, an install dialog's OK button both granted the
/// capabilities and — through `enablement`'s "absence means enabled" — started
/// the process at the next launch, which is one act wearing the clothes of
/// two. Data-only plugins are left absent, and therefore on: there is nothing
/// to start, and a switch with no argument behind it is worse than no switch.
fn park_new_plugin_disabled(profile_dir: Option<&PathBuf>, plugin: &Plugin) {
    if !consent::starts_disabled(plugin) {
        return;
    }
    let Some(profile_dir) = profile_dir else { return };
    if let Err(e) = enablement::set_enabled(profile_dir, &plugin.manifest.id, false) {
        eprintln!("shell: could not park {} as disabled: {e}", plugin.manifest.id);
    }
}

/// One entry a marketplace index offers: what it is, what installing it (and
/// whatever it depends on) would request, and an Install button that runs the
/// whole ADR-014 pipeline — resolve, show the plan, grant what was shown,
/// fetch, and unpack through `cordial_plugins::unpack::install`.
///
/// `index_dir` is re-read into a fresh [`LocalFileSource`] inside the click
/// handler rather than the row holding one: the source is cheap to build
/// again and doing so avoids threading a borrow of `opened.index` through a
/// GTK callback that outlives the button press that created it.
fn build_marketplace_entry_row(
    parent: &impl IsA<gtk::Window>,
    dialog: &adw::PreferencesDialog,
    opened: &Rc<marketplace::Opened>,
    entry: &Entry,
    index_dir: &Path,
    root: &Path,
    profile_dir: Option<&PathBuf>,
    installed_group: &adw::PreferencesGroup,
) -> adw::ActionRow {
    let row_dialog = dialog.clone();
    let title = if entry.name.is_empty() { entry.id.clone() } else { entry.name.clone() };
    let requests = if entry.capabilities.is_empty() {
        "Requests no capabilities".to_string()
    } else {
        format!(
            "Requests: {}",
            entry.capabilities.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ")
        )
    };
    let deps = if entry.dependencies.is_empty() {
        String::new()
    } else {
        format!(
            "\nDepends on: {}",
            entry.dependencies.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(", ")
        )
    };
    let row = adw::ActionRow::builder()
        .title(format!("{title} {}", entry.version))
        .subtitle(format!("{requests}{deps}"))
        .build();
    row.set_subtitle_lines(4);

    let install = gtk::Button::with_label("Install");
    install.set_valign(gtk::Align::Center);
    row.add_suffix(&install);

    if !opened.verified {
        // The refusal `marketplace::install` itself makes, said up front
        // rather than only after a click: a button that looks live and then
        // reports a refusal is the small lie AGENTS.md keeps finding here.
        install.set_sensitive(false);
        install.set_tooltip_text(Some(
            "Refused: this index's signature has not been checked against a configured key.",
        ));
        return row;
    }
    let Some(profile_dir) = profile_dir.cloned() else {
        install.set_sensitive(false);
        install.set_tooltip_text(Some("No profile to grant against."));
        return row;
    };

    let window = parent.as_ref().clone();
    let opened = opened.clone();
    let entry_id = entry.id.clone();
    let entry_version = entry.version.clone();
    let index_dir = index_dir.to_path_buf();
    let root = root.to_path_buf();
    let installed_group = installed_group.clone();
    let row_for_status = row.clone();
    install.connect_clicked(move |button| {
        // Pinned to exactly the version the user clicked on, not a caret
        // range: the row they are looking at names one version, and `^`
        // would let the resolver silently choose a newer one that was not
        // what was shown.
        let wanted = vec![manifest::Dependency::new(&entry_id, &format!("={entry_version}")).unwrap()];

        let body = match resolve::resolve(&opened.index, &wanted) {
            Ok(plan) => plan_confirmation_text(&plan),
            Err(e) => {
                row_for_status.set_subtitle(&format!("Cannot resolve: {e}"));
                return;
            }
        };

        let dialog = adw::AlertDialog::builder()
            .heading(format!("Install {entry_id} {entry_version}?"))
            .body(body)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("install", "Install and approve");
        dialog.set_response_appearance("install", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let opened = opened.clone();
        let wanted = wanted.clone();
        let entry_id = entry_id.clone();
        let index_dir = index_dir.clone();
        let root = root.clone();
        let profile_dir = profile_dir.clone();
        let installed_group = installed_group.clone();
        let window = window.clone();
        let row_for_status = row_for_status.clone();
        let button = button.clone();
        let row_dialog = row_dialog.clone();
        // The closure consumes its copy, and `present` below still needs one.
        let present_parent = window.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "install" {
                // Resolved again, now that the user has agreed to the plan
                // shown above — see `plan_confirmation_text`'s doc comment
                // for why this is a second call rather than the same `Plan`
                // carried across the dialog.
                match resolve::resolve(&opened.index, &wanted) {
                    Ok(plan) => {
                        let grants_path = grants::path_in(&profile_dir);
                        for step in &plan.steps {
                            for cap in &step.capabilities {
                                if let Err(e) = grants::set(&grants_path, &step.id, *cap, true) {
                                    eprintln!(
                                        "shell: could not record {}'s {} grant: {e}",
                                        step.id,
                                        cap.name()
                                    );
                                }
                            }
                        }
                        let granted = grants::load(&grants_path);
                        let source = LocalFileSource::new(&index_dir);
                        match marketplace::install(&source, &opened, &wanted, &granted, &root) {
                            Ok(dirs) if dirs.is_empty() => {
                                row_for_status.set_subtitle(&format!(
                                    "{entry_id} is already installed at this version."
                                ));
                            }
                            Ok(dirs) => {
                                for dir in &dirs {
                                    let Ok(text) = std::fs::read_to_string(dir.join("plugin.json"))
                                    else {
                                        continue;
                                    };
                                    let Ok(plugin) = manifest::parse(&text, dir) else { continue };
                                    // The plan above granted what it listed;
                                    // starting the process is the separate
                                    // act, and it is the user's.
                                    park_new_plugin_disabled(Some(&profile_dir), &plugin);
                                    let new_row = build_plugin_row(
                                        &row_dialog,
                                        &plugin,
                                        Some(&profile_dir),
                                        &root,
                                        &installed_group,
                                        Tier::User,
                                    );
                                    installed_group.add(&new_row);
                                }
                                button.set_sensitive(false);
                                button.set_label("Installed");
                            }
                            Err(e) => {
                                eprintln!("shell: marketplace install of {entry_id} failed: {e}");
                                row_for_status.set_subtitle(&format!("Install failed: {e}"));
                            }
                        }
                    }
                    Err(e) => row_for_status.set_subtitle(&format!("Cannot resolve: {e}")),
                }
            }
            dialog.close();
        });
        dialog.present(Some(&present_parent));
    });

    row
}

/// "Marketplace" — browsing and installing from an index source, per
/// ADR-014's design and the seam it deliberately leaves open.
///
/// **No index is configured by default, and no key is either.** ADR-014
/// declines to name a host or ship a signing key — see
/// `cordial_plugins::sign` and `cordial_plugins::source` for why — so there is
/// nothing here to point at until the user supplies a directory of their own:
/// a local checkout of an index repository (`index.json`, an optional
/// `index.json.minisig`, and an `archives/` directory), exactly what
/// ADR-014 already designs an index to be. Pointing this at a directory with
/// no key configured still lists what it offers, because a user cannot decide
/// whether to trust something they cannot see; every Install button stays
/// refused, with the reason stated on it, until a key is set and verifies.
///
/// Two groups rather than one, because they change on different occasions:
/// configuration (the directory, the key) changes rarely, and the listing is
/// rebuilt every time "Check for plugins" is pressed. Rebuilding the whole
/// page on every refresh would also discard whatever the user had just typed
/// into the other group.
fn build_marketplace_groups(
    parent: &impl IsA<gtk::Window>,
    dialog: &adw::PreferencesDialog,
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
    root: &Path,
    profile_dir: Option<PathBuf>,
    installed_group: &adw::PreferencesGroup,
) -> (adw::PreferencesGroup, adw::PreferencesGroup) {
    let row_dialog = dialog.clone();
    let config_group = adw::PreferencesGroup::builder()
        .title("Marketplace")
        .description(
            "Cordial comes with no marketplace. Point this at a folder holding an index.json \
             to browse one. Without a key you can look but not install.",
        )
        .build();

    let dir_row = adw::ActionRow::builder()
        .title("Index directory")
        .subtitle(
            config
                .borrow()
                .marketplace_index_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Not set".to_string()),
        )
        .build();
    let choose = gtk::Button::with_label("Choose…");
    choose.set_valign(gtk::Align::Center);
    dir_row.add_suffix(&choose);
    config_group.add(&dir_row);
    {
        let window = parent.as_ref().clone();
        let config = config.clone();
        let config_path = config_path.clone();
        let dir_row = dir_row.clone();
        choose.connect_clicked(move |_| {
            let config = config.clone();
            let config_path = config_path.clone();
            let dir_row = dir_row.clone();
            choose_file(&window, "Choose an index directory", true, move |path| {
                dir_row.set_subtitle(&path.display().to_string());
                config.borrow_mut().marketplace_index_dir = Some(path);
                persist(&config, &config_path);
            });
        });
    }

    let key_row = adw::EntryRow::builder().title("Trusted public key (minisign, base64)").build();
    key_row.set_text(&config.borrow().marketplace_public_key.clone().unwrap_or_default());
    config_group.add(&key_row);
    {
        let config = config.clone();
        let config_path = config_path.clone();
        key_row.connect_changed(move |row| {
            let text = row.text().to_string();
            config.borrow_mut().marketplace_public_key =
                if text.trim().is_empty() { None } else { Some(text) };
            persist(&config, &config_path);
        });
    }

    let status_row = adw::ActionRow::builder()
        .title("Nothing checked yet")
        .subtitle("Press \"Check for plugins\" to fetch the index.")
        .build();
    status_row.set_subtitle_lines(3);
    let check = gtk::Button::with_label("Check for plugins");
    check.set_valign(gtk::Align::Center);
    status_row.add_suffix(&check);
    config_group.add(&status_row);

    let listing_group = adw::PreferencesGroup::builder()
        .title("Available")
        .description("What the index above currently lists. Empty until you press \"Check for plugins\".")
        .build();
    let listing_rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let window = parent.as_ref().clone();
        let config = config.clone();
        let root = root.to_path_buf();
        let profile_dir = profile_dir.clone();
        let installed_group = installed_group.clone();
        let listing_group = listing_group.clone();
        let listing_rows = listing_rows.clone();
        let status_row = status_row.clone();
        check.connect_clicked(move |_| {
            for row in listing_rows.borrow_mut().drain(..) {
                listing_group.remove(&row);
            }
            let Some(index_dir) = config.borrow().marketplace_index_dir.clone() else {
                status_row.set_title("No index directory set");
                status_row.set_subtitle("Choose one above first.");
                return;
            };
            let key_text = config.borrow().marketplace_public_key.clone();
            let source = LocalFileSource::new(&index_dir);
            let trust_key = match key_text.as_deref().map(str::trim) {
                Some(t) if !t.is_empty() => match sign::parse_key(t) {
                    Ok(k) => Some(k),
                    Err(e) => {
                        status_row.set_title("Public key does not parse");
                        status_row.set_subtitle(&e.to_string());
                        return;
                    }
                },
                _ => None,
            };
            let trust = match &trust_key {
                Some(k) => marketplace::Trust::Key(k),
                None => marketplace::Trust::Unconfigured,
            };
            let opened = match marketplace::open(&source, trust) {
                Ok(o) => o,
                Err(e) => {
                    status_row.set_title("Could not open the index");
                    status_row.set_subtitle(&e.to_string());
                    return;
                }
            };

            let n = opened.index.entries.len();
            status_row.set_title(&format!("{n} release{} listed", if n == 1 { "" } else { "s" }));
            status_row.set_subtitle(if opened.verified {
                "Signature verified against the configured key."
            } else {
                "Not verified — no key is configured, or none was given. Installing is refused \
                 until one is set and verifies."
            });

            let opened = Rc::new(opened);
            for entry in &opened.index.entries {
                let row = build_marketplace_entry_row(
                    &window,
                    &row_dialog,
                    &opened,
                    entry,
                    &index_dir,
                    &root,
                    profile_dir.as_ref(),
                    &installed_group,
                );
                listing_group.add(&row);
                listing_rows.borrow_mut().push(row);
            }
        });
    }

    (config_group, listing_group)
}

/// "Plugins" — a master switch, then what is installed, in the two tiers that
/// actually exist on disk.
///
/// **Modelled on GNOME's Extensions app, deliberately.** It solves the same
/// problem with the same materials: one switch that governs the lot with a
/// one-line caveat under it, System and User-Installed as separate lists, a
/// switch on every entry, and browsing for more somewhere else entirely. What
/// was here before was four groups deep in a page that opened with a paragraph
/// about which document enablement is stored in and which ADR decided that.
///
/// **Everything greys out rather than disappearing when the master switch is
/// off.** Hiding the lists would leave somebody unable to see what they have
/// installed, or to find the one plugin they wanted to check on, without first
/// turning plugins back on — and the state they are trying to reason about is
/// exactly the one where plugins are off.
/// Returns the page and the Installed group, because "Get Plugins" has to add
/// a row to that exact group the moment an install succeeds. Handing the group
/// back is what keeps one `build_plugin_row` serving both — the alternative
/// this project has already been bitten by is a second, similar-looking row
/// built in the installer's callback that drifts from this one.
fn build_plugins_page(
    parent: &adw::PreferencesDialog,
    config: Rc<RefCell<ShellConfig>>,
) -> (adw::PreferencesPage, adw::PreferencesGroup) {
    let page = adw::PreferencesPage::builder()
        .title("Plugins")
        .name("plugins")
        .icon_name("application-x-addon-symbolic")
        .build();

    let root = manifest::plugin_root();
    let system_root = manifest::system_plugin_root();
    let (system, user, shadowed) = discover_tiers(&system_root, &root);

    let profile_name = config.borrow().profile.clone();
    // A profile whose name will not resolve has no grants and no enablement
    // file to read, and the page still has to render. `None` here means every
    // row shows nothing allowed, which is what such a profile would in fact
    // give a plugin.
    let profile_dir = cordial_shell::profile::dir(&profile_name).ok();

    // ---- the master switch ------------------------------------------------
    // Named on the page, because permissions are per profile and the page gave
    // no sign of which one it meant. Somebody who allowed FPS Flex everything
    // it asked for under one profile, then opened this window under another,
    // was told "you have not allowed it to do anything yet" -- true, and
    // indistinguishable from the grant having failed. Grants stopped being
    // global on purpose; saying so costs one line.
    let master_group = adw::PreferencesGroup::builder()
        .description(format!(
            "What you allow here applies to the {profile_name} profile. Other profiles decide separately."
        ))
        .build();
    let master = adw::SwitchRow::builder()
        .title("Use Plugins")
        // GNOME's register, and the same honesty: a consequence the user can
        // weigh in one line. Not a promise that plugins are dangerous, and not
        // silence either.
        .subtitle("Plugins can cause performance and stability issues.")
        .active(profile_dir.as_ref().is_none_or(|d| enablement::plugins_allowed(d)))
        .build();
    master_group.add(&master);
    page.add(&master_group);

    // ---- built-in ---------------------------------------------------------
    let builtin_group = adw::PreferencesGroup::builder().title("Built-In").build();
    if system.is_empty() {
        // Said rather than left as an empty group, and said without naming the
        // path: a user who has never installed Cordial from a package has no
        // built-in plugins and does not need to know where they would live.
        builtin_group.add(&adw::ActionRow::builder().title("None").build());
    } else {
        for plugin in &system {
            let row = build_plugin_row(
                parent,
                plugin,
                profile_dir.as_ref(),
                &system_root,
                &builtin_group,
                Tier::BuiltIn,
            );
            builtin_group.add(&row);
        }
    }

    // ---- user-installed ---------------------------------------------------
    let user_group = adw::PreferencesGroup::builder().title("Installed").build();
    if user.is_empty() {
        user_group.add(&adw::ActionRow::builder().title("None yet").build());
    } else {
        for plugin in &user {
            let row =
                build_plugin_row(parent, plugin, profile_dir.as_ref(), &root, &user_group, Tier::User);
            user_group.add(&row);
        }
    }

    // A user plugin that was dropped for sharing a built-in id is the one thing
    // on this page a user cannot see the cause of: the plugin is on disk, it
    // parses, and it is simply not in either list. `flags::collect` reports the
    // same collision on stdout, which is no use to somebody in a settings
    // window.
    for id in &shadowed {
        user_group.add(
            &adw::ActionRow::builder()
                .title(id.clone())
                .subtitle("Not used: a built-in plugin already has this name. Built-in plugins can be switched off, but not replaced.")
                .build(),
        );
    }

    page.add(&builtin_group);
    page.add(&user_group);

    // ---- what the master switch governs -----------------------------------
    let governed = [builtin_group.clone(), user_group.clone()];
    let apply = move |groups: &[adw::PreferencesGroup], on: bool| {
        for g in groups {
            g.set_sensitive(on);
        }
    };
    apply(&governed, master.is_active());
    {
        let dir = profile_dir.clone();
        master.connect_active_notify(move |row| {
            let on = row.is_active();
            if let Some(dir) = &dir {
                if let Err(e) = enablement::set_plugins_allowed(dir, on) {
                    // Put back to what is on disk, the same posture the
                    // per-plugin switch takes: a switch resting somewhere the
                    // file disagrees with is a lie the user cannot see.
                    eprintln!("shell: could not record that plugins are {}: {e}", if on { "on" } else { "off" });
                    row.set_active(!on);
                    return;
                }
            }
            apply(&governed, on);
        });
    }
    if profile_dir.is_none() {
        master.set_sensitive(false);
        master.set_subtitle("This profile's directory could not be read, so nothing can be saved.");
    }

    (page, user_group)
}

/// "Get Plugins" — installing from a file, and the marketplace.
///
/// **Its own page, and that is the point.** These three groups were stacked on
/// the Plugins page above the list of what is installed, so the first thing the
/// page showed was a file picker, a directory chooser and a text field for a
/// signing key — configuration for acquiring plugins, in front of somebody who
/// came to switch one off. GNOME's Extensions app puts browsing behind its own
/// entry for the same reason. Nothing here changed except where it lives.
/// The install-a-plugin groups, added to the Plugins page rather than a tab.
///
/// **Two tabs both about plugins was the worst of the seven.** Somebody looking
/// at what they had found "Plugins"; somebody wanting another had to guess that
/// "Get Plugins" was a separate page rather than a button on the first. They
/// are one page now, in the order the task happens -- what you have, then how
/// to get more.
fn add_get_plugins_groups(
    parent: &impl IsA<gtk::Window>,
    dialog: &adw::PreferencesDialog,
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
    installed_group: &adw::PreferencesGroup,
    page: &adw::PreferencesPage,
) {

    let root = manifest::plugin_root();
    let profile_name = config.borrow().profile.clone();
    let profile_dir = cordial_shell::profile::dir(&profile_name).ok();

    page.add(&build_install_group(parent, dialog, &root, profile_dir.clone(), installed_group));
    let (marketplace_config, marketplace_listing) =
        build_marketplace_groups(parent, dialog, config, config_path, &root, profile_dir, installed_group);
    page.add(&marketplace_config);
    page.add(&marketplace_listing);
}

/// Builds the settings dialog: Roblox, Updates, General, Plugins, Report, one
/// `AdwPreferencesPage` each.
///
/// **`AdwPreferencesDialog`, not `AdwPreferencesWindow`.** The window form has
/// been deprecated since libadwaita 1.6 and the compiler had been saying so on
/// every build -- eleven warnings across this file and
/// `plugin_preferences.rs`, read past all session.
///
/// The difference is not cosmetic. An `AdwDialog` is adaptive: it floats over
/// the parent on a wide screen and becomes a bottom sheet on a narrow one,
/// which is what the platform now expects of transient in-app content. It also
/// takes no `transient_for` and no `modal` -- it is presented *against* a
/// parent widget instead, so those builder calls go away rather than moving.
pub fn build_preferences_window(
    parent: &impl IsA<gtk::Window>,
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
) -> adw::PreferencesDialog {
    let window = adw::PreferencesDialog::builder()
        .title("Settings")
        .search_enabled(false)
        // Taller than libadwaita's default, which was chosen for the Roblox
        // page's two rows and cut the Updates page off mid-switch: its warning
        // row appears exactly when both download switches are off, and a
        // warning below the fold is a warning nobody is shown. Only a default —
        // the window resizes, and GTK clamps this to the work area on a screen
        // that cannot hold it.
        .content_width(640)
        .content_height(720)
        .build();

    // First, because it is the one that has to be right before anything else
    // in this window matters: without a build there is nothing to launch, and
    // a renderer preference for a client that cannot start is decoration.
    window.add(&build_roblox_page(parent, config.clone(), config_path.clone()));
    // Second, next to the page about the build it is about.
    window.add(&crate::updater::build_update_page(config.clone(), config_path.clone()));
    // **Five pages where there were seven**, reported as "why are there so many
    // tabs". Appearance held two groups and Get Plugins three; neither was a
    // destination, and a tab per group makes somebody hunt through seven pages
    // for the one row they came to change.
    let general = build_general_page(config.clone(), config_path.clone());
    add_appearance_groups(&general, config.clone(), config_path.clone());
    window.add(&general);
    let (plugins, installed) = build_plugins_page(&window, config.clone());
    add_get_plugins_groups(parent, &window, config, config_path, &installed, &plugins);
    window.add(&plugins);
    window.add(&build_report_page(parent));

    window
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest built for this test and nowhere near the UI. The distinction
    /// matters here more than it usually would: a fixture that could reach the
    /// plugins page is how that page came to be listing software nobody had
    /// installed, requesting capabilities nobody had granted.
    fn fixture(json: &str) -> Plugin {
        manifest::parse(json, std::path::Path::new("/fixture")).expect("well-formed test manifest")
    }

    fn granted(names: &[&str]) -> BTreeSet<Capability> {
        names.iter().map(|n| Capability::parse(n).expect("a real capability name")).collect()
    }

    fn sink(node_name: &str, description: &str, is_default: bool) -> audio_devices::Sink {
        audio_devices::Sink {
            node_name: node_name.into(),
            description: description.into(),
            is_default,
        }
    }

    fn two_sinks() -> Vec<audio_devices::Sink> {
        vec![
            sink("alsa_output.pci-0000_00_1f.3.analog-stereo", "Built-in Audio", true),
            sink("alsa_output.usb-Generic_USB_Audio-00.analog-stereo", "USB Headset", false),
        ]
    }

    #[test]
    fn the_picker_leads_with_system_default_and_marks_which_device_that_is() {
        let (labels, names) = output_picker_rows(&two_sinks(), &AudioOutput::default());
        assert_eq!(labels[0], "System default");
        // Both readable at once: the standing instruction, and where it
        // currently points. A picker that showed only the first makes somebody
        // guess where their sound is going.
        assert_eq!(labels[1], "Built-in Audio (current system default)");
        assert_eq!(labels[2], "USB Headset");
        assert_eq!(names.len(), 2, "the leading entry is not a device and has no name");
        assert_eq!(AudioOutput::default().index_in(&names), 0);
    }

    #[test]
    fn a_chosen_device_that_is_present_selects_its_own_row() {
        let sinks = two_sinks();
        let chosen = AudioOutput("alsa_output.usb-Generic_USB_Audio-00.analog-stereo".into());
        let (labels, names) = output_picker_rows(&sinks, &chosen);
        assert_eq!(labels.len(), 3, "nothing extra is added for a device that is here");
        assert_eq!(chosen.index_in(&names), 2);
    }

    #[test]
    fn a_chosen_device_that_has_been_unplugged_keeps_its_row_and_stays_selected() {
        // The failure this exists to prevent, in one test: open settings with
        // the headset unplugged, and a picker that only listed live devices
        // would show "System default" selected. Close the window and the
        // choice is gone -- silently, and only discovered the next time the
        // headset is plugged in and the sound comes out of the speakers.
        let gone = AudioOutput("bluez_output.AC_12_2F_9E_00_11.1".into());
        let (labels, names) = output_picker_rows(&two_sinks(), &gone);
        assert_eq!(labels.len(), 4);
        assert_eq!(labels[3], "bluez_output.AC_12_2F_9E_00_11.1 (not connected)");
        assert_eq!(gone.index_in(&names), 3);
        // And selecting that row back gives the same value, so merely opening
        // and closing the window is a no-op.
        assert_eq!(AudioOutput::from_index(3, &names), gone);
    }

    #[test]
    fn an_empty_session_offers_only_the_system_default_row() {
        // `build_audio_group` shows an insensitive explanation instead of a
        // menu in this case; what is checked here is that the row-building
        // half does not invent a device to fill the gap.
        let (labels, names) = output_picker_rows(&[], &AudioOutput::default());
        assert_eq!(labels, vec!["System default".to_string()]);
        assert!(names.is_empty());
    }

    #[test]
    fn every_row_the_picker_offers_can_be_selected_and_read_back() {
        let gone = AudioOutput("alsa_output.somewhere-else".into());
        let (labels, names) = output_picker_rows(&two_sinks(), &gone);
        for index in 0..labels.len() as u32 {
            let chosen = AudioOutput::from_index(index, &names);
            assert_eq!(chosen.index_in(&names), index, "row {index} did not survive the trip");
        }
    }

    #[test]
    fn a_plugin_with_nothing_approved_is_not_described_as_off() {
        // The state the owner asked not to be collapsed into "off": installed,
        // switched on, and inert because ADR-003 starts everything at nothing.
        // A user in this state has to be told to approve something, not to
        // toggle a switch that is already where they want it.
        let plugin = fixture(r#"{"id":"p","name":"P","entry":"main.ts","capabilities":["flags.read"]}"#);
        let summary = capability_summary(&plugin, None, true);
        assert!(summary.starts_with("On"), "{summary}");
        assert!(summary.contains("not allowed it to do anything yet"), "{summary}");
        assert!(!summary.contains("Off"), "an unapproved plugin must not read as switched off: {summary}");
    }

    #[test]
    fn a_disabled_plugin_still_shows_the_grants_it_keeps() {
        // Turning something off must not look like it revoked anything,
        // because it does not: that is the whole reason enablement is a
        // separate file from grants.
        let plugin = fixture(r#"{"id":"p","name":"P","entry":"main.ts","capabilities":["flags.read"]}"#);
        let summary = capability_summary(&plugin, Some(&granted(&["flags.read"])), false);
        assert!(summary.starts_with("Off"), "{summary}");
        assert!(summary.contains("kept"), "switching off must not read as revoking: {summary}");
    }

    #[test]
    fn a_requested_capability_that_was_not_granted_is_not_shown_as_granted() {
        // The security-relevant half. Requested and granted are different
        // lists, and a summary that blurred them would say a plugin may do
        // something nobody approved.
        let plugin =
            fixture(r#"{"id":"p","name":"P","entry":"main.ts","capabilities":["flags.read","log"]}"#);
        let summary = capability_summary(&plugin, Some(&granted(&["log"])), true);
        assert!(summary.contains("Allowed: log"), "{summary}");
        assert!(summary.contains("Not allowed: flags.read"), "{summary}");
        assert!(!summary.contains("Allowed: flags.read"), "{summary}");
        // One line, now and in future: the three-line version is what made the
        // page unreadable with four plugins on it.
        assert!(!summary.contains('\n'), "the row summary must stay one line: {summary}");
    }

    #[test]
    fn every_capability_has_a_description_a_user_could_read() {
        // `capability_description` is matched exhaustively over
        // `Capability`, so the compiler already refuses a variant added there
        // and missed here — this is the other half: that no arm quietly
        // returns an empty or placeholder string a real checkbox would show
        // blank.
        for cap in Capability::all() {
            let text = capability_description(*cap);
            assert!(!text.trim().is_empty(), "{cap} has no description");
            assert!(text.len() > 10, "{cap}'s description is too short to be real prose: {text:?}");
        }
    }

    #[test]
    fn a_user_plugin_may_not_shadow_a_built_in_one_and_the_page_can_say_which() {
        // The rule `flags::collect` enforces on the flag layers, enforced the
        // same way on what the window lists -- and, unlike `collect`, reported
        // somewhere the person affected will see it. A user plugin that simply
        // vanished from the list is indistinguishable from one that failed to
        // install.
        let root = std::env::temp_dir().join("cordial-settings-tiers-test");
        let _ = std::fs::remove_dir_all(&root);
        let system = root.join("system");
        let user = root.join("user");
        for (dir, id) in [(&system, "builtin-one"), (&user, "builtin-one"), (&user, "mine")] {
            let d = dir.join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("plugin.json"),
                format!(r#"{{"id":"{id}","name":"{id}","entry":"main.ts"}}"#),
            )
            .unwrap();
        }

        let (sys, usr, shadowed) = discover_tiers(&system, &user);
        assert_eq!(sys.iter().map(|p| p.manifest.id.as_str()).collect::<Vec<_>>(), ["builtin-one"]);
        assert_eq!(
            usr.iter().map(|p| p.manifest.id.as_str()).collect::<Vec<_>>(),
            ["mine"],
            "the shadowing copy must not be listed as installed"
        );
        assert_eq!(shadowed, ["builtin-one"], "and it must be reported by name rather than dropped");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `plan_confirmation_text` is what stands between a marketplace click
    /// and a grant that gets written to disk, so it has to actually name
    /// every plugin the plan touches and what each one asks for — not only
    /// the one the user clicked on. No GTK involved: `resolve::resolve` and
    /// this function are both plain Rust, which is why this test can run
    /// without a display the way the widget-building code around it cannot.
    #[test]
    fn the_confirmation_text_names_every_step_and_what_it_requests_including_pulled_in_dependencies() {
        let index_json = r#"{"format":1,"plugins":[
            {"id":"app","version":"1.0.0","capabilities":["log"],
             "dependencies":{"lib":"^1.0.0"},
             "url":"https://x.invalid/app-1.0.0.tar.zst",
             "hash":"sha256:0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8"},
            {"id":"lib","version":"1.0.0","capabilities":["assets.override"],"dependencies":{},
             "url":"https://x.invalid/lib-1.0.0.tar.zst",
             "hash":"sha256:1111111111111111111111111111111111111111111111111111111111111111"}
        ]}"#;
        let index = cordial_plugins::registry::Index::parse_unverified(index_json).unwrap();
        let wanted = vec![cordial_plugins::manifest::Dependency::new("app", "^1.0.0").unwrap()];
        let plan = resolve::resolve(&index, &wanted).unwrap();
        let text = plan_confirmation_text(&plan);
        assert!(text.contains("app 1.0.0"), "{text}");
        assert!(text.contains("lib 1.0.0"), "{text}");
        // The sentences, not the wire names: the point of the dialog is that
        // somebody can judge what they are agreeing to, and "requests:
        // assets.override" is not something anybody judges.
        assert!(
            text.contains(Capability::AssetsOverride.consequence()),
            "a dependency's own capability must be shown, not only the plugin the user clicked \
             on: {text}"
        );
        assert!(text.contains(Capability::Log.consequence()), "{text}");
        assert!(
            !text.contains("assets.override"),
            "the wire name is not what a user reads: {text}"
        );
        assert!(text.contains("starts switched off"), "{text}");
    }

    #[test]
    fn a_data_only_plugin_is_installed_without_a_prompt_and_stays_enabled() {
        // ADR-021's first consent rule, checked where it actually decides
        // something. A texture pack has no entry module and no capabilities,
        // so there is nothing to run, nothing it could reach, and nothing to
        // ask — and nothing to switch off either, because a switch with no
        // argument behind it is worse than no switch.
        let pack = fixture(r#"{"id":"retro-ui","name":"Retro UI"}"#);
        assert_eq!(consent::verdict(&pack), consent::Verdict::Silent);

        let profile = std::env::temp_dir().join("cordial-settings-consent-data");
        let _ = std::fs::remove_dir_all(&profile);
        std::fs::create_dir_all(&profile).unwrap();
        park_new_plugin_disabled(Some(&profile), &pack);
        assert!(
            enablement::is_enabled(&profile, "retro-ui"),
            "data has nothing to start, so it must not be parked off"
        );
        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn a_plugin_with_code_is_parked_switched_off_however_the_prompt_was_answered() {
        // Consent and enablement are two acts. Before this, an install
        // dialog's OK both granted the capabilities and — through
        // `enablement`'s "absence means enabled" — started the process at the
        // next launch, which is one act wearing the clothes of two.
        let plugin = fixture(r#"{"id":"tweaks","name":"Tweaks","entry":"main.ts","capabilities":["flags.write"]}"#);
        let profile = std::env::temp_dir().join("cordial-settings-consent-code");
        let _ = std::fs::remove_dir_all(&profile);
        std::fs::create_dir_all(&profile).unwrap();
        park_new_plugin_disabled(Some(&profile), &plugin);
        assert!(!enablement::is_enabled(&profile, "tweaks"));
        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn the_consent_body_says_what_it_does_and_that_it_starts_off() {
        // The whole argument of ADR-021's consent section, in one assertion:
        // a prompt that says "wants to run code" is answered yes by
        // everybody, and one that names the effect is not.
        let plugin = fixture(r#"{"id":"t","name":"T","entry":"m.ts","capabilities":["flags.write"]}"#);
        let consent::Verdict::Ask(prompt) = consent::verdict(&plugin) else {
            panic!("code must be asked about")
        };
        let body = consent_body(&prompt);
        assert!(body.contains("graphics backend"), "{body}");
        assert!(body.contains("present mode"), "{body}");
        assert!(body.contains("starts switched off"), "{body}");
        assert!(!body.contains("run code"), "{body}");
        assert!(prompt.heading().contains("will be able to"), "{}", prompt.heading());
    }

    #[test]
    fn the_builtin_consent_body_does_not_claim_it_starts_switched_off() {
        // The whole reason `consent_body_for_builtin` exists rather than
        // reusing `consent_body` outright: a built-in ships enabled by
        // default (`enablement::SHIPS_DISABLED` is the only exception) and
        // may already have been running, denied, for as long as the profile
        // has existed. "It starts switched off" would be false of exactly
        // the plugin this dialog exists to explain.
        let plugin = fixture(
            r#"{"id":"discord-presence","name":"Discord Presence","entry":"main.ts",
                "capabilities":["presence.set","log"]}"#,
        );
        let consent::Verdict::Ask(prompt) = consent::verdict(&plugin) else {
            panic!("code must be asked about")
        };
        let body = consent_body_for_builtin(&prompt);
        assert!(!body.contains("starts switched off"), "{body}");
        assert!(body.contains(Capability::PresenceSet.consequence()), "{body}");
        assert!(body.contains("Enabled switch"), "{body}");
    }

    #[test]
    fn a_capability_row_names_the_denial_only_when_one_was_recorded() {
        let plain = capability_row_subtitle(Capability::PresenceSet, false);
        let refused = capability_row_subtitle(Capability::PresenceSet, true);
        assert_eq!(plain, Capability::PresenceSet.consequence());
        assert!(refused.starts_with(Capability::PresenceSet.consequence()));
        assert!(refused.contains("refused"), "{refused}");
        assert_ne!(plain, refused);
    }

    #[test]
    fn a_built_in_that_has_never_been_asked_is_not_marked_seen() {
        // `maybe_prompt_builtin_consent` itself needs a display to exercise
        // end to end; this pins the guard it reads before ever building a
        // dialog, the same way the rest of this file tests the logic around
        // GTK widgets rather than the widgets themselves.
        let dir = std::env::temp_dir().join("cordial-settings-builtin-consent-unseen");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!consent::has_been_asked(&consent::seen_path_in(&dir), "discord-presence"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
