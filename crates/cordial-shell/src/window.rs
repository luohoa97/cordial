//! The shell window: [`cordial_shell::host_window`]'s window with the chooser
//! in its content slot.
//!
//! ADR-002 calls the core shell "a bridge measured in milliseconds, not a
//! product" — window, chooser, and an escape hatch to settings. Nothing here
//! should grow features; anything richer belongs to the UI plugin that takes
//! over at T3.
//!
//! What this file owns beyond the widgets is the *decision*: ADR-002's
//! correction says a plugin declares what and core decides how, and this is
//! core deciding. The chooser hands over an id; everything between that id and
//! a running client — finding a build, claiming a profile, refusing when the
//! profile is taken, saying so when there is nothing installed — happens here,
//! and every branch of it ends in something the user can see.

use libadwaita as adw;
use libadwaita::glib;
use libadwaita::gtk;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::os::unix::process::ExitStatusExt;
use std::rc::Rc;
use std::time::Duration;

use crate::chooser;
use crate::deep_link;
use crate::install::{self, NotFound};
use crate::instructions;
use crate::root_warning;
use crate::launch;
use crate::profile_switcher;
use crate::refresh_watch;
use crate::crash;
use crate::settings;
use crate::shell_config::ShellConfig;
use crate::updater;
use crate::window_state;
use cordial_shell::host_window::HostWindow;
use cordial_shell::profile;

/// A `roblox-player://` link the desktop handed over, held until the user
/// presses Roblox.
///
/// **Deliberately not a launch.** A link could start the client outright, and
/// that is what a browser handler usually does; here it would skip the two
/// decisions the launcher exists to take — which profile, and against which
/// build — and it would do it in response to a click in another application.
/// So the link opens the launcher, the launcher says a join is waiting, and the
/// user presses Roblox as they otherwise would.
///
/// The banner is not decoration either. A link that vanished into a variable
/// would be indistinguishable from a link that was dropped, and "it ignored my
/// click" is the report that follows.
#[derive(Clone)]
pub struct PendingJoin {
    url: Rc<RefCell<Option<String>>>,
    banner: adw::Banner,
}

impl PendingJoin {
    fn new() -> Self {
        let banner = adw::Banner::builder().revealed(false).button_label("Discard").build();
        let url: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        {
            // A queued join the user cannot get rid of is a trap: the next
            // launch would carry a link they have changed their mind about, and
            // the only way out would be closing the launcher.
            let url = url.clone();
            banner.connect_button_clicked(move |banner| {
                *url.borrow_mut() = None;
                banner.set_revealed(false);
            });
        }
        PendingJoin { url, banner }
    }

    /// Show a link as waiting. Replaces whatever was queued: two links means the
    /// second click is the one the user is looking at.
    pub fn queue(&self, url: String) {
        self.banner.set_title(&banner_line(&url));
        self.banner.set_revealed(true);
        *self.url.borrow_mut() = Some(url);
    }

    /// What the next launch should carry, without consuming it: a launch that
    /// fails must leave the link where it was, or a busy profile would cost the
    /// user the link as well as the launch.
    fn peek(&self) -> Option<String> {
        self.url.borrow().clone()
    }

    fn clear(&self) {
        *self.url.borrow_mut() = None;
        self.banner.set_revealed(false);
    }

    fn banner(&self) -> &adw::Banner {
        &self.banner
    }
}

/// What the banner says about a waiting link.
///
/// Pure, and separate from the widget, for the reason `busy_body` is: a string
/// that can only be inspected by building a window and photographing it is a
/// string that drifts. It also has one genuine hazard in it — `AdwBanner`'s
/// title is Pango markup and this text came from a browser, so an unescaped `&`
/// in a query string is enough to make GTK drop the label, and anything sharper
/// is worse than that.
fn banner_line(url: &str) -> String {
    let shown = glib::markup_escape_text(&deep_link::summarise(url));
    format!("Roblox link waiting: {shown} — press Roblox to join")
}

/// The running shell, for the handful of things that happen to it from outside.
///
/// `main.rs` holds one so that a second invocation carrying a link — which is
/// what the desktop does when a browser opens `roblox-player://` while Cordial
/// is up — reaches the window that already exists rather than starting another.
pub struct Shell {
    window: adw::Window,
    join: PendingJoin,
}

impl Shell {
    /// Bring the launcher forward and show the link as waiting.
    pub fn queue_join(&self, url: String) {
        self.join.queue(url);
        self.window.present();
    }

    /// Bring the launcher forward, for a second invocation carrying nothing.
    pub fn present(&self) {
        self.window.present();
    }
}

/// How long the "starting" dialog stays up before it is taken away.
///
/// `cordial-run` reaching its first frame takes seconds; failing at load takes
/// a fraction of one. This used to be the *only* moment the launcher ever asked
/// whether the client was still alive, which meant a crash ten minutes into a
/// session produced nothing at all -- the window simply vanished. It is now
/// only the dialog's timer, and the one timer left in this path: the process is
/// watched by `SIGCHLD` (see [`try_launch`]) and not by a clock at all.
///
/// It is a one-shot rather than a deadline the watch also has to respect, which
/// is what makes it safe to have two sources here -- see [`try_launch`] for the
/// ordering argument.
const EARLY_EXIT_CHECK: std::time::Duration = std::time::Duration::from_secs(3);

/// The dialog shown while the client starts.
///
/// Roblox takes a while to get from a launch to a window of its own, and until
/// now that gap was a toast and then nothing -- the launcher looked idle while
/// the most failure-prone part of the whole program was happening. This says
/// "working" for as long as that lasts.
///
/// **The progress bar is indeterminate on purpose.** Nothing here knows how far
/// along the client is: the shell holds a pid, not a progress channel, and a
/// bar that filled at a made-up rate would be inventing a measurement, which is
/// the one thing this project is strict about. A pulsing bar claims only that
/// something is still happening, which is true and is all that is known.
///
/// The icon is Cordial's own. It is deliberately the only customisable part and
/// it is customised by pointing at a file, never by shipping alternatives:
/// Roblox's own icons are their assets, and AGENTS.md rules out vendoring any
/// of them however small.
fn starting_dialog(parent: &gtk::Window, profile: &str, joining: bool) -> gtk::Window {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.set_margin_top(28);
    content.set_margin_bottom(24);
    content.set_margin_start(36);
    content.set_margin_end(36);

    let icon = gtk::Image::from_icon_name("io.github.luohoa97.Cordial");
    icon.set_pixel_size(72);
    content.append(&icon);

    let title = gtk::Label::new(Some(&match joining {
        true => format!("Starting Roblox on {profile}, joining the link"),
        false => format!("Starting Roblox on {profile}"),
    }));
    title.add_css_class("title-4");
    title.set_wrap(true);
    title.set_justify(gtk::Justification::Center);
    title.set_max_width_chars(32);
    content.append(&title);

    let bar = gtk::ProgressBar::new();
    bar.set_width_request(260);
    content.append(&bar);

    // 12 pulses a second is the shell's own animation and costs nothing; it is
    // not tied to anything the client is doing and must not be read as though
    // it were.
    let pulse = glib::timeout_add_local(Duration::from_millis(80), {
        let bar = bar.clone();
        move || {
            bar.pulse();
            glib::ControlFlow::Continue
        }
    });

    let dialog = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .deletable(false)
        .titlebar(&{
            let h = adw::HeaderBar::new();
            h.set_show_end_title_buttons(false);
            h.add_css_class("flat");
            h
        })
        .child(&content)
        .build();

    // Stop the pulse whenever the dialog goes, however it goes -- closed here
    // when the client is up, or closed by the early-exit path when it is not.
    // A timeout left running against a dropped widget is a warning per frame
    // for the rest of the session.
    let pulse = std::cell::RefCell::new(Some(pulse));
    dialog.connect_close_request(move |_| {
        if let Some(id) = pulse.borrow_mut().take() {
            id.remove();
        }
        glib::Propagation::Proceed
    });

    dialog.present();
    dialog
}



pub fn build(
    app: &adw::Application,
    config: Rc<RefCell<ShellConfig>>,
    config_path: Rc<PathBuf>,
) -> Shell {
    // T1: the chooser core paints before the plugin host is up. One entry, and
    // it starts the client — see `chooser.rs` on why the second one went.
    let source = chooser::CordialSource;

    // The toast overlay has to exist before the chooser, because the chooser's
    // activate handler reports through it.
    let toasts = adw::ToastOverlay::new();

    // Empty and hidden until the desktop hands over a link, which on most runs
    // is never; `AdwBanner` takes no space while it is not revealed.
    let join = PendingJoin::new();

    let chooser_widget = {
        let toasts = toasts.clone();
        let config = config.clone();
        let join = join.clone();
        chooser::build(&source, move |id| {
            let Some(window) = toasts.root().and_downcast::<gtk::Window>() else { return };
            match id {
                chooser::ROBLOX => activate_roblox(&window, &toasts, &config, &join),
                // Unreachable while there is one entry, and deliberately loud
                // rather than ignored: the moment plugin-contributed entries
                // exist, an id core does not know how to launch is a bug in the
                // registration path, and a silently dead row is how that bug
                // gets shipped.
                other => alert(&window, "Cordial does not know how to launch that", other),
            }
        })
    };

    // Directly above the Launch button, and that position is the whole argument:
    // the profile is a launch parameter — which of these do I start — rather
    // than an ambient identity, so it belongs beside the button it governs. It
    // was an avatar in the top-right corner first; see `profile_switcher.rs` for
    // why that was a browser convention borrowed into an application that is not
    // one.
    let switcher = profile_switcher::build(config.clone(), config_path.clone());
    let profile_row = switcher.group.clone();
    let profile_combo = switcher.row.clone();

    // Two controls, centred, and the empty space around them is the point.
    //
    // This was a stack of two `AdwPreferencesGroup`s pinned to the top of the
    // window, which left the bottom two thirds blank and read as a form that had
    // run out of fields. Nothing was added to fill it: the same two controls
    // sitting in the middle of the window is a composition rather than a
    // remainder, and the width clamp is tighter than the 480 the groups used
    // because a boxed list and a pill button stretched to a launcher's full
    // width look like a preferences page whatever is in them.
    let column = gtk::Box::new(gtk::Orientation::Vertical, 24);
    column.set_valign(gtk::Align::Center);
    column.append(&profile_row);
    column.append(&chooser_widget);

    let clamp = adw::Clamp::builder().maximum_size(360).child(&column).build();
    clamp.set_margin_top(24);
    clamp.set_margin_bottom(24);
    clamp.set_margin_start(24);
    clamp.set_margin_end(24);
    clamp.set_vexpand(true);

    // The banner sits above the centred column rather than inside it, so that a
    // queued join reads as a message about the window instead of a third
    // control in the composition. It expands nothing while it is hidden, which
    // is why the launcher looks the same as it always did on every run where no
    // link arrives.
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(join.banner());
    body.append(&clamp);
    toasts.set_child(Some(&body));

    // --- seam for the engine's surface -----------------------------------
    // This window is the same definition `cordial-runtime` builds to host the
    // engine's Wayland surface — see `host_window`, and ADR-011 on why there
    // must not be two. What differs between the two callers is only what goes
    // in the content slot: the chooser here, the engine's `wl_subsurface`
    // there. The two processes are still separate, so nothing is handed over
    // between them yet; what is shared today is the window, its header bar
    // and its theming.
    //
    // When the T3 handoff does exist, the shape is still a second child under
    // the toolbar view — a `gtk::Stack` page swapped in — rather than tearing
    // this window down and building another. ADR-002's open question about a
    // UI-plugin crash is why: the shell has to stay retained and hidden rather
    // than destroyed, so the window, header bar and theme survive the handoff
    // instead of flickering through a second window creation.
    // -----------------------------------------------------------------------

    // Whatever was last saved for the profile this shell is about to launch,
    // read before the window exists so a saved size reaches `HostWindow::new`
    // rather than a resize following on afterwards -- see `window_state.rs`
    // for where this is stored and why per profile. `profile::dir` refuses an
    // unusable name outright rather than sanitising it, and an unreadable name
    // here is the same as one with nothing saved: windowed, at the built-in
    // default below.
    let initial_profile = config.borrow().profile.clone();
    let initial_window =
        profile::dir(&initial_profile).ok().map(|d| window_state::load(&d, window_state::Which::Launcher)).unwrap_or_default();

    // 540x340 was 720x480 while the content was two preference groups pinned to
    // the top of it, which left the lower two thirds empty. This is a launcher's
    // size rather than a settings window's: wide enough that a development
    // build's title — `git describe` output, beside two header-bar buttons —
    // does not ellipsise, and tall enough that the centred column has room
    // around it without being adrift in it. Only the *default*, now doubly so:
    // it is also what a profile with nothing saved yet falls back to. Kept as
    // a pure function, the same reason `host_window.rs`'s own `fit_within` is
    // one, so the fallback is testable without a display.
    let (initial_width, initial_height) = initial_size(&initial_window);
    let host = HostWindow::new(&cordial_shell::host_window::title(), initial_width, initial_height, &toasts);

    // **The primary menu, rightmost, because that is where GNOME users look.**
    //
    // Packed before Settings so it ends up nearest the window controls: at the
    // end, first packed is outermost. The HIG puts app-level actions here --
    // the ones that are about Cordial rather than about the thing on screen --
    // and until now Cordial had none of them anywhere, including an About
    // dialog, which is the conventional home for the version and the licence.
    //
    // Settings keeps its own button rather than folding into this menu. It is
    // not an app-level action here: it is where the Roblox build, the profiles
    // and the plugins live, which is most of what anyone opens this launcher to
    // do. A frequently used primary action earns a button; About and the rest
    // do not.
    let primary_menu = gtk::gio::Menu::new();
    primary_menu.append(Some("_Report a Problem"), Some("win.settings::report"));
    primary_menu.append(Some("_About Cordial"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&primary_menu)
        .primary(true)
        .build();
    host.header().pack_end(&menu_button);

    let settings_button = gtk::Button::from_icon_name("preferences-system-symbolic");
    settings_button.set_tooltip_text(Some("Settings"));
    host.header().pack_end(&settings_button);

    let window = host.window().clone();
    // `HostWindow` is deliberately application-less — the runtime has no
    // `GApplication` — so the shell binary attaches its own here, which is
    // what makes the window keep `app` alive and quit with it.
    window.set_application(Some(app));

    // The other half of the joke, and the half that is actually seen: the task
    // switcher and the dock take the icon from here. `APP_ID` deliberately does
    // not change -- it is the single-instance key and has to match the desktop
    // file's own name -- so the brand rides on the window's icon rather than on
    // the application's identity. Both icons ship, so this name always
    // resolves; see `branding`, which also explains why nothing polls for the
    // date and why the repository never rebrands.
    window.set_icon_name(Some(cordial_shell::branding::current().icon()));

    // Immediately left of Settings, which is what packing it second at the end
    // produces, and always present. A control that comes and goes is hard to
    // find at the moment you want it and moves everything beside it when it
    // arrives — and this one answers "which Roblox build am I on", which is
    // worth a button whether or not anything newer exists.
    let update_button = updater::header_button(&window, config.clone());
    host.header().pack_end(&update_button);


    // An action rather than only a button handler, because the header bar is no
    // longer the only way in: a launch refused for a busy profile offers to
    // open settings, and that offer must land on the same window this button
    // opens. `AdwWindow` is not a `GtkApplicationWindow` and so carries no
    // action map of its own, hence the explicit group.
    //
    // It takes a string, which is the name of the page to open on and is empty
    // for "whichever libadwaita shows first". That parameter exists for the
    // photograph seam below: Settings has five tabs now and only the first one
    // can be seen without pressing something, which under ADR-011's Wayland is
    // not a thing an agent may do — see `open_on_start`. It goes through the
    // same action the button does rather than a second way in.
    // Read before `config` is moved into the closure below. The value is a
    // string in a config file, so it is copied out once here rather than
    // borrowed later from something that has gone.
    let fullscreen_accel = config.borrow().fullscreen_accel.trim().to_string();
    // Likewise: everything below that saves window state needs the `Rc`
    // itself, not merely a value read out of it once, and `config` stops being
    // usable the moment `settings_action`'s closure below takes it.
    let config_for_window_state = config.clone();
    // And again for `win.launch`, below. Same reason.
    let config_for_launch = config.clone();

    let settings_action =
        gtk::gio::SimpleAction::new("settings", Some(glib::VariantTy::STRING));
    let window_for_settings = window.clone();
    settings_action.connect_activate(move |_, page| {
        let settings = settings::build_preferences_window(
            &window_for_settings,
            config.clone(),
            config_path.clone(),
        );
        if let Some(name) = page.and_then(|p| p.str()).filter(|n| !n.is_empty()) {
            settings.set_visible_page_name(name);
        }
        // An `AdwDialog` is presented against a parent *widget*, not shown as
        // a window of its own. That is what lets it become a bottom sheet on a
        // narrow screen instead of a floating box too wide to fit.
        settings.present(Some(&window_for_settings));
    });
    // The same arrangement for the profile row: a launch refused because the
    // profile is busy has to be able to reach the control that chooses another
    // one. It is in this window rather than behind a button now, so the action
    // only has to move the focus there.
    let profile_action = gtk::gio::SimpleAction::new("profile", None);
    // **Open the list, do not merely focus the container.** This called
    // `grab_focus()` on the `PreferencesGroup`, which is a box around the row:
    // nothing visible happened, so pressing "Choose a Profile" on the
    // profile-busy dialog looked exactly like pressing "Cancel". Reported on
    // 2026-08-28 as the two buttons doing the same thing -- and from outside
    // the process they did, since the only difference was where keyboard focus
    // landed behind a dialog that was disappearing.
    //
    // Deferred to an idle callback because the usual caller is that dialog's
    // response handler, and the dialog is still tearing down when it fires; a
    // popover asked to open underneath a closing modal does not.
    let profile_combo_for_action = profile_combo.clone();
    profile_action.connect_activate(move |_, _| {
        let row = profile_combo_for_action.clone();
        row.grab_focus();
        glib::idle_add_local_once(move || {
            // Disambiguated: `WidgetExt` and `ListBoxRowExt` both offer an
            // `activate`, and the one that opens an `AdwComboRow`'s list is the
            // row's, which is what a keyboard Enter on it does.
            gtk::prelude::WidgetExt::activate(&row);
        });
    });

    // Fullscreen, on F11, because until now there was no way for a user to ask
    // for it at all. `HostWindow::set_fullscreen` has existed throughout and its
    // only callers were `looper.rs`'s CORDIAL_SCRIPT timeline and the probes
    // under examples/ -- instrumentation, reachable by a test and by nobody
    // playing the game. A window that can be fullscreened only by a script is
    // not a window that can be fullscreened.
    //
    // F11 rather than a header-bar button: the shell's chrome is what a
    // fullscreen player is trying to get rid of, so putting the control there
    // means it disappears at the moment it is needed to come back. The
    // compositor's own binding still works and is not replaced by this; this is
    // the one that survives being told the window is a game.
    let fullscreen_action = gtk::gio::SimpleAction::new("fullscreen", None);
    let window_for_fullscreen = window.clone();
    fullscreen_action.connect_activate(move |_, _| {
        let on = !window_for_fullscreen.is_fullscreen();
        if on {
            window_for_fullscreen.fullscreen();
        } else {
            window_for_fullscreen.unfullscreen();
        }
    });

    // Pressing Roblox, as an action.
    //
    // The photograph seam again, one step further than Settings: a crash page
    // cannot be seen without a launch that fails, and a launch cannot be
    // started without pressing something. AGENTS.md rules out synthesising the
    // press, and under ADR-011's Wayland there is nothing to send it to. So
    // this is the same call the chooser row makes -- `activate_roblox`, with
    // the same window, toasts, config and pending join -- reachable by name.
    // Not a second launch path: a seam that went somewhere else would prove
    // nothing about the button.
    let launch_action = gtk::gio::SimpleAction::new("launch", None);
    {
        let window = window.clone();
        let toasts = toasts.clone();
        let config = config_for_launch;
        let join = join.clone();
        launch_action.connect_activate(move |_, _| {
            activate_roblox(&window.clone().upcast(), &toasts, &config, &join);
        });
    }

    // **About, with the diagnostics block as its Troubleshooting section.**
    //
    // `AdwAboutDialog` carries a Troubleshooting page with a copy button and a
    // "Save as" built in, and `debug_info` is what fills it. That is the same
    // text `Settings -> Report a Problem` shows and `--diagnostics` prints, from
    // the one function, so the three can never drift.
    //
    // It is here because three separate reporters could not find the block at
    // all. One typed `flatpak run cordial --diagnostic` and got "Invalid id";
    // another ran `cordial --diagnostics`, got "command not found", and wrote
    // *that* into the diagnostics field of their report. Settings -> Report a
    // Problem is not where anyone looks for it. About -> Troubleshooting is.
    let about_action = gtk::gio::SimpleAction::new("about", None);
    {
        let window = window.clone();
        about_action.connect_activate(move |_, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name(cordial_shell::branding::current().name())
                .application_icon(cordial_shell::branding::current().icon())
                .version(cordial_shell::version::full())
                .developer_name("The Cordial contributors")
                .website("https://github.com/luohoa97/cordial")
                .issue_url("https://github.com/luohoa97/cordial/issues/new/choose")
                // `Gpl30`, not `Gpl30Only`: the manifest says
                // `GPL-3.0-or-later` and GTK spells that distinction in the
                // enum. Checked against LICENSE and Cargo.toml rather than
                // assumed -- a licence stated wrongly in an About dialog is
                // worse than one not stated at all, because it is believed.
                .license_type(gtk::License::Gpl30)
                .debug_info(crate::diagnostics::report())
                .debug_info_filename("cordial-diagnostics.txt")
                .build();
            dialog.present(Some(&window));
        });
    }

    let actions = gtk::gio::SimpleActionGroup::new();
    actions.add_action(&about_action);
    actions.add_action(&launch_action);
    actions.add_action(&settings_action);
    actions.add_action(&profile_action);
    actions.add_action(&fullscreen_action);
    window.insert_action_group("win", Some(&actions));

    // Start playing without a human pressing anything, for `just dev --play`.
    //
    // Deliberately the `win.launch` action rather than a second launch path or
    // a synthesised click: AGENTS.md rules out synthesising the press, ADR-011's
    // Wayland leaves nothing to send one to, and a separate path would prove
    // nothing about the button people actually use. This is why that action was
    // made reachable by name in the first place.
    //
    // A short delay rather than zero because the launch reads configuration and
    // may put a dialog up, and doing that before the window has been presented
    // gives a modal with nothing behind it. 400ms matches the deep-link path
    // just below, which had the same problem first.
    if std::env::var_os("CORDIAL_AUTOSTART").is_some() {
        let window = window.clone();
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            WidgetExt::activate_action(&window, "win.launch", None).ok();
        });
    }
    // Whatever the user configured, defaulting to F11 but not insisting on it —
    // see `ShellConfig::fullscreen_accel` for why a hardcoded F11 is unreachable
    // on some laptops. An empty string leaves the action registered and
    // unbound, which is the right state for somebody who would rather bind
    // GNOME's own `toggle-fullscreen` and have it work for every window.
    if let Some(app) = window.application() {
        use gtk::prelude::GtkApplicationExt;
        let accel = fullscreen_accel.clone();
        if accel.is_empty() {
            app.set_accels_for_action("win.fullscreen", &[]);
        } else {
            app.set_accels_for_action("win.fullscreen", &[accel.as_str()]);
        }
    }
    // The target before the name, and in that order deliberately: GTK's action
    // helper re-checks the pair every time either changes, and naming a
    // string-taking action while the target is still unset logs "can't be
    // activated due to parameter type mismatch" on every startup. The empty
    // string is "open on whatever page libadwaita shows first", which is what a
    // press of this button has always meant.
    settings_button.set_action_target_value(Some(&"".to_variant()));
    settings_button.set_action_name(Some("win.settings"));

    // Save the fullscreen state the moment it changes, against whichever
    // profile is selected *at that moment* -- read fresh from `config` in
    // every handler below rather than captured once, so that switching the
    // profile row and then pressing F11 saves against the row's new choice
    // and not the one this window opened with. `window_state.rs`'s own header
    // is the reasoning for why this happens unconditionally and why that is
    // safe for this window specifically -- it does not carry over to the
    // engine's own window without re-deriving it.
    {
        let config = config_for_window_state.clone();
        window.connect_fullscreened_notify(move |w| persist_fullscreen(&config, w));
    }
    {
        let config = config_for_window_state.clone();
        window.connect_maximized_notify(move |w| persist_maximised(&config, w));
    }

    // Size, debounced. GTK fires `notify::default-width`/`notify::default-height`
    // on every step of an interactive drag, and writing a file on every one of
    // those would mean tens of writes a second to a profile directory holding a
    // live session cookie next to it. `pending_size_save` is cancelled and
    // restarted on each notification, the same shape `starting_dialog`'s own
    // `pulse` timeout uses for "stop the old one before starting a new one",
    // so only the settled size after resizing stops is ever written.
    let pending_size_save: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
    let schedule_size_save: Rc<dyn Fn()> = {
        let config = config_for_window_state.clone();
        let window = window.clone();
        let pending = pending_size_save.clone();
        Rc::new(move || {
            if let Some(id) = pending.take() {
                id.remove();
            }
            let config = config.clone();
            let window = window.clone();
            let pending_inner = pending.clone();
            let id = glib::timeout_add_local_once(SIZE_SAVE_DEBOUNCE, move || {
                pending_inner.set(None);
                persist_window_size(&config, &window);
            });
            pending.set(Some(id));
        })
    };
    {
        let s = schedule_size_save.clone();
        window.connect_default_width_notify(move |_| s());
    }
    {
        let s = schedule_size_save.clone();
        window.connect_default_height_notify(move |_| s());
    }
    // Flushed on close rather than left to the debounce: quitting right after
    // a drag must not lose the last few pixels to a timer that never got to
    // fire. `Propagation::Proceed` because this only writes a file -- it must
    // never be what stands between the user and the window actually closing.
    {
        let config = config_for_window_state.clone();
        let pending = pending_size_save.clone();
        window.connect_close_request(move |w| {
            if let Some(id) = pending.take() {
                id.remove();
                persist_window_size(&config, w);
            }
            glib::Propagation::Proceed
        });
    }

    // Focus starts on the thing the window is for, so that a keyboard launches
    // with Return and nothing else. Left alone it lands on the profile row,
    // which is the first focusable widget and the one control here that is not
    // the point. GTK only *draws* a focus ring once a key has been pressed, so
    // this costs nothing visually on a window nobody types into.
    // Spelled out because `GtkWindowExt` and `RootExt` both offer `set_focus`
    // and neither is more obviously meant than the other.
    GtkWindowExt::set_focus(&window, Some(&chooser_widget));

    window.present();
    // After `present()`, not before: every other place this window is put into
    // or out of fullscreen (`fullscreen_action` above, `HostWindow::set_fullscreen`)
    // does it to an already-mapped window, and there is no precedent here for
    // asking an unmapped one. See `window_state.rs`'s header for why doing this
    // unconditionally is the right call for this window and must not be copied
    // uncritically onto the engine's own.
    // Before fullscreen, so that a window saved as both comes back as both and
    // leaving fullscreen drops it to maximised rather than to its floating
    // size -- which is what a user who maximised it and then fullscreened it
    // is expecting to get back.
    if initial_window.maximised {
        window.maximize();
    }
    if initial_window.fullscreen {
        window.fullscreen();
    }
    open_on_start(&window, &update_button);

    // This is the shell's own launcher window, not the engine's -- the two
    // are the same *definition* per ADR-011 but separate processes with
    // separate `HostWindow`s, and the one hosting the engine's subsurface is
    // built by `cordial-run`, out of reach from here. Watching this one is
    // what proves `refresh_watch` observes a real display correctly; the
    // callback is empty because there is nothing on this side of the process
    // boundary to tell -- see `refresh_watch.rs` for where the JNI wiring
    // belongs instead.
    refresh_watch::watch(&window, |_outputs| {});

    Shell { window, join }
}

/// How long to wait after the last `notify::default-width`/`default-height`
/// before writing the settled size to disk. Long enough that an interactive
/// drag -- which fires this on every frame the compositor delivers -- settles
/// well before it, short enough that quitting a couple of seconds after
/// letting go of the resize still saves it without needing the close-request
/// flush at all.
const SIZE_SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// This shell's built-in window size, used whenever a profile has nothing
/// saved yet -- the same 540x340 `window.rs` used before `window_state.rs`
/// existed at all.
const DEFAULT_WIDTH: i32 = 540;
const DEFAULT_HEIGHT: i32 = 340;

/// What size to open the window at, given what was saved for the profile it
/// is about to run. Pure and separate from the GTK call that uses it, the same
/// shape `host_window.rs`'s `fit_within` takes for the same reason: the
/// fallback is worth pinning with a test that does not need a display.
fn initial_size(state: &window_state::WindowState) -> (i32, i32) {
    (state.width.unwrap_or(DEFAULT_WIDTH), state.height.unwrap_or(DEFAULT_HEIGHT))
}

/// Save whether `window` is fullscreen right now, against whichever profile
/// `config` currently names.
///
/// Read-modify-write against the file rather than an in-memory cache, so that
/// switching the profile row and then resizing, or the reverse, never writes
/// one profile's geometry into another's record. The cost is a read on every
/// toggle; `window.json` is a few dozen bytes and this fires on a keypress, not
/// in a loop.
fn persist_fullscreen(config: &Rc<RefCell<ShellConfig>>, window: &adw::Window) {
    let name = config.borrow().profile.clone();
    let Ok(dir) = profile::dir(&name) else { return };
    let mut state = window_state::load(&dir, window_state::Which::Launcher);
    state.fullscreen = window.is_fullscreen();
    if let Err(e) = window_state::save(&dir, window_state::Which::Launcher, &state) {
        println!("  shell: could not save fullscreen state for {name:?}: {e}");
    }
}

/// Save the window's current default size, against whichever profile `config`
/// currently names -- unless it is fullscreen right now, in which case there
/// is no windowed size to record: GTK reports the fullscreen extent through
/// the same `default-width`/`default-height` properties while a fullscreen
/// transition is in flight, and writing that over the size someone actually
/// asked to remember would be recording the wrong number under the right
/// name, which is worse than not recording at all.
/// Maximised, saved the same way and for the same reason `persist_fullscreen`
/// is: GNOME's own list of what a window should remember is width, height,
/// maximised and fullscreen, and this was the one of the four that was missing.
fn persist_maximised(config: &Rc<RefCell<ShellConfig>>, window: &adw::Window) {
    let name = config.borrow().profile.clone();
    let Ok(dir) = profile::dir(&name) else { return };
    let mut state = window_state::load(&dir, window_state::Which::Launcher);
    state.maximised = window.is_maximized();
    if let Err(e) = window_state::save(&dir, window_state::Which::Launcher, &state) {
        println!("  shell: could not save maximised state for {name:?}: {e}");
    }
}

fn persist_window_size(config: &Rc<RefCell<ShellConfig>>, window: &adw::Window) {
    // **Maximised as well as fullscreen, and this is the detail people get
    // wrong.** The size worth remembering is the one the window returns to when
    // it is *not* maximised; writing the maximised extent here would restore a
    // floating window the size of the screen, which is how an application ends
    // up opening at 3440x1440 with a title bar in the corner. GTK keeps
    // `default-width`/`default-height` at the unmaximised size once the
    // transition has settled, but reports the covering extent through them
    // while one is in flight, and the debounce cannot tell the difference.
    if window.is_fullscreen() || window.is_maximized() {
        return;
    }
    let name = config.borrow().profile.clone();
    let Ok(dir) = profile::dir(&name) else { return };
    let mut state = window_state::load(&dir, window_state::Which::Launcher);
    state.width = Some(window.default_width());
    state.height = Some(window.default_height());
    if let Err(e) = window_state::save(&dir, window_state::Which::Launcher, &state) {
        println!("  shell: could not save window size for {name:?}: {e}");
    }
}

/// `CORDIAL_SHELL_PRESENT=settings,settings=updates,update,launch,maximise,fullscreen`
/// opens those windows at startup and puts the launcher into those states.
///
/// A test seam, and it exists because there is no other way to photograph these
/// windows. AGENTS.md forbids synthesising input at the compositor — it lands on
/// whatever has focus, which is the developer's session, and it has hijacked a
/// cursor here once — and Wayland has no window-targeted injection to fall back
/// on, so "click Settings and take a screenshot" is not an available sentence.
/// Asking the shell to open its own window is, and it goes through exactly the
/// same action and the same button handler a click does rather than a second
/// construction path that could differ from the real one.
///
/// **`settings=<page>` opens Settings on a named page**, which is the same
/// problem one level down: the window has five tabs and a photograph of it shows
/// one. The names are the `name` each `AdwPreferencesPage` is built with —
/// `roblox`, `updates`, `appearance`, `general`, `plugins` — and an unknown one
/// is libadwaita's warning to answer, not this function's, because a name that
/// silently fell back to the first page would produce a screenshot captioned as
/// something it is not.
///
/// Same shape as `CORDIAL_SHELL_RUN_SECONDS` above: an environment variable that
/// nothing sets in normal use, doing something a person could do by hand.
fn open_on_start(window: &adw::Window, update_button: &gtk::Button) {
    let Some(want) = std::env::var("CORDIAL_SHELL_PRESENT").ok().filter(|s| !s.is_empty()) else {
        return;
    };
    let window = window.clone();
    let update_button = update_button.clone();
    // After a beat, so the main window is mapped and photographable underneath
    // whatever this opens on top of it.
    glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
        for one in want.split(',').map(str::trim) {
            // `settings` and `settings=updates` are the same action with and
            // without a page, so they are split rather than matched separately.
            let (what, page) = one.split_once('=').unwrap_or((one, ""));
            match what {
                "settings" => {
                    let _ = window.activate_action("win.settings", Some(&page.to_variant()));
                }
                "update" => update_button.emit_clicked(),
                "launch" => {
                    let _ = window.activate_action("win.launch", None);
                }
                // Same seam, for the same reason, one property further: a
                // window-state round trip cannot be checked without putting the
                // window into a state, and maximising is not something an agent
                // may synthesise a click for.
                "maximise" => window.maximize(),
                "fullscreen" => {
                    let _ = window.activate_action("win.fullscreen", None);
                }
                other => println!("  shell: CORDIAL_SHELL_PRESENT does not know {other:?}"),
            }
        }
    });
}

/// The whole of what pressing Roblox does.
///
/// Split out so the instructions window's "check again" button can call exactly
/// the same thing rather than a second, similar-looking path — the failure that
/// arrangement produces is a retry button that succeeds where the row does not,
/// or the reverse, and neither is discoverable from the outside.
fn activate_roblox(
    window: &gtk::Window,
    toasts: &adw::ToastOverlay,
    config: &Rc<RefCell<ShellConfig>>,
    join: &PendingJoin,
) {
    // Root gets one warning before it costs an hour. See `root_warning`: no
    // session bus means no PipeWire, and the engine calls `abort()` at
    // experience start rather than playing silence -- which presents as a
    // crash, not as an audio problem. Not a refusal: on FreeBSD's linuxulator
    // there is no other user, so refusing would refuse the platform.
    if root_warning::running_as_root() {
        let window = window.clone();
        let toasts = toasts.clone();
        let config = config.clone();
        let join = join.clone();
        root_warning::confirm(&window.clone(), move || {
            launch_now(&window, &toasts, &config, &join);
        });
        return;
    }
    launch_now(window, toasts, config, join);
}

/// The launch itself, once anything that had to be asked has been.
fn launch_now(
    window: &gtk::Window,
    toasts: &adw::ToastOverlay,
    config: &Rc<RefCell<ShellConfig>>,
    join: &PendingJoin,
) {
    match try_launch(window, config, join) {
        Outcome::Started => {}
        Outcome::Failed(message) => alert(window, "Roblox could not start", &message),
        Outcome::ProfileBusy(name, holder) => {
            profile_busy(window, toasts, config, join, &name, holder)
        }
        Outcome::NoBuild => {
            let window = window.clone();
            let config = config.clone();
            let join = join.clone();
            instructions::present(&window.clone(), move || {
                // Returning whether it worked is what lets the instructions
                // window stay up while the user is still following them.
                matches!(try_launch(&window, &config, &join), Outcome::Started)
            });
        }
    }
}

enum Outcome {
    Started,
    NoBuild,
    /// The profile is held by another instance. Separated from `Failed` because
    /// it is not a fault and the answer is not "close this dialog": people
    /// reach it by double-clicking launch, and what they need is a way to run
    /// against a different profile.
    ///
    /// Carries the holder so the dialog can name it. Said "another window" once
    /// and that was the misleading part — the holder frequently has no window,
    /// which is exactly what makes this hard to recover from unaided.
    ProfileBusy(String, Option<profile::Holder>),
    Failed(String),
}

/// No `ToastOverlay` any more: the "Starting Roblox" toast this used to raise
/// is now the loading dialog, which says the same thing and stays up for as
/// long as it is true.
fn try_launch(
    window: &gtk::Window,
    config: &Rc<RefCell<ShellConfig>>,
    join: &PendingJoin,
) -> Outcome {
    let (roblox, profile_name) = {
        let config = config.borrow();
        (config.roblox.clone(), config.profile.clone())
    };

    let build = match install::locate(&roblox) {
        Ok(build) => build,
        Err(NotFound::NoBuild) => return Outcome::NoBuild,
        Err(NotFound::Unusable(message)) => return Outcome::Failed(message),
    };

    // ADR-012's claim, taken before the process exists so that a refusal
    // costs nothing. A second window on one profile is two processes writing
    // one cookie store; the message names the profile because "already open"
    // on its own does not tell anyone which one to close.
    let claim = match profile::acquire(&profile_name) {
        Ok(claim) => claim,
        Err(profile::Error::Busy(name, holder)) => return Outcome::ProfileBusy(name, holder),
        Err(e) => return Outcome::Failed(e.to_string()),
    };

    // Read rather than taken: everything above this can still refuse, and a
    // busy profile that also cost the user their link would be two failures for
    // one press. It is cleared below, once there is a process holding it.
    let url = join.peek();
    let instance = match launch::spawn(&build, claim, run_seconds_override(), url.as_deref()) {
        Ok(instance) => instance,
        Err(message) => return Outcome::Failed(message),
    };
    join.clear();

    let starting = starting_dialog(&window, &profile_name, url.is_some());

    // **`SIGCHLD`, not a clock.** This was `timeout_add_local` at 500 ms for
    // the whole session -- two wakeups a second, forever, to ask a question
    // whose answer is no for hours at a time. `child_watch_add_local` is GLib's
    // own `SIGCHLD` handling and fires once, when the process actually dies.
    //
    // **GLib owns the reap now, and it has to be the only one.** The child
    // watch calls `waitpid` itself; the `Instance::exited` this replaced called
    // `try_wait`, which is also a `waitpid`. Two of those and one loses with
    // `ECHILD` -- either the status the crash page exists to report is gone, or
    // nobody reaps and a zombie stays. So `Instance` no longer offers a wait at
    // all (see `launch::Instance::pid`), and the closure below holds the
    // `Instance` only for its command line and its captured output. Dropping a
    // `Child` does not reap, so GLib stays the sole waiter either way.
    //
    // **The ordering the single timer used to enforce is now enforced by
    // program order.** The old comment was right that two sources racing over
    // "does the dialog close before or after the crash page opens" would be
    // whatever GLib felt like -- so the crash page is not a second source. The
    // one-shot below can *only* close the dialog; the child watch closes the
    // dialog and then, in the same callback, decides about the crash page. The
    // two are therefore ordered by the statements in one function rather than
    // by source priority, and the shared `Cell` only has to make the close
    // idempotent. A `Cell` suffices for that because both callbacks run on the
    // GTK main context, on this thread, one at a time.
    let dialog_closed = Rc::new(Cell::new(false));
    {
        let starting = starting.clone();
        let dialog_closed = dialog_closed.clone();
        glib::timeout_add_local_once(EARLY_EXIT_CHECK, move || {
            if !dialog_closed.replace(true) {
                starting.close();
            }
        });
    }

    let window = window.clone();
    let pid = glib::Pid(instance.pid() as i32);
    glib::child_watch_add_local(pid, move |_, wait_status| {
        if !dialog_closed.replace(true) {
            starting.close();
        }
        // The launcher being gone is not a crash to report and there is nothing
        // left to be transient for. Under ADR-012 quitting the launcher while a
        // client runs is the ordinary case, so a closed launcher must not pop a
        // crash page on top of a desktop the user went back to. The poll this
        // replaces returned `Break` here; a child watch has no equivalent, and
        // it does not need one -- the source removes itself once the child has
        // exited, and until then the only thing this check costs is the branch.
        if !window.is_visible() {
            return;
        }
        // GLib hands back the raw `waitpid` status, which is exactly what
        // `ExitStatusExt::from_raw` takes, so signal deaths keep their signal
        // and `crash::is_crash` sees the same value `try_wait` used to give it.
        let status = std::process::ExitStatus::from_raw(wait_status);
        // Narrated either way, and deliberately without the client's output in
        // it: this line is how somebody reading a terminal learns which of the
        // two paths was taken, and the output belongs on the page and nowhere
        // else. `println!` here would be a second sink for text `launch.rs`
        // took care to keep in memory.
        if crash::is_crash(&status) {
            println!("  shell: the client {status}; showing the crash page");
            crash::present(&window, &status, &instance.command_line, &instance.recent_output());
        } else {
            println!("  shell: the client exited cleanly ({status}); no crash page");
        }
    });

    Outcome::Started
}

/// Shorten a session, for testing. `--run` is a hard timer in `cordial-run` and
/// the launcher's own default is a day, which is unhelpful when what is being
/// checked is that a launch happens at all.
fn run_seconds_override() -> Option<u64> {
    std::env::var("CORDIAL_SHELL_RUN_SECONDS").ok().and_then(|v| v.parse().ok())
}

/// The profile is already running, and here is what to do about it.
///
/// Deliberately not phrased as an error. ADR-012's lock refusing a second
/// instance is the design working, and the ordinary way to reach it is
/// double-clicking the launcher — so the dialog names the profile, says what is
/// true, and offers the only action that helps: choosing a different one.
///
/// It used to point at a text field in Settings. It now moves the focus to the
/// profile row above the Launch button, which already shows the profile this
/// dialog is about as unavailable.
/// The refusal a held profile earns, written so somebody can act on it.
///
/// **The old wording said "close the window that already has it" and that was
/// the actively misleading part.** The holder very often has no window: closing
/// the engine's window does not end `cordial-run` yet, and the launcher's
/// `--run` default is a day, so a client outlives the window that started it,
/// is reparented to `systemd --user`, and keeps the profile for the rest of its
/// timer. This developer met exactly that — a client 31 minutes into a 24-hour
/// timer holding `default` with nothing on screen to close — and the interface
/// told them to close a window that did not exist.
///
/// So the dialog names the process and offers to stop it. Someone who cannot
/// find a window is not confused; they are looking at a correct description of
/// a situation the previous text could not express.
fn profile_busy(
    parent: &gtk::Window,
    toasts: &adw::ToastOverlay,
    config: &Rc<RefCell<ShellConfig>>,
    join: &PendingJoin,
    name: &str,
    holder: Option<profile::Holder>,
) {
    let ours = holder.as_ref().filter(|h| h.is_cordial()).cloned();
    let body = busy_body(holder.as_ref());

    // `AlertDialog`, not `MessageDialog`: it draws inside the parent window
    // rather than opening a toplevel of its own. That is what libadwaita
    // replaced `MessageDialog` with, and `root_warning` already uses it. It
    // also removes the focus problem at the source rather than working around
    // it -- a separate toplevel handed focus to whatever was behind Cordial on
    // dismissal, so closing this meant alt-tabbing back to the window you were
    // already in. A dialog that never was a window cannot do that.
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Profile {name} is already in use"))
        .body(body)
        .build();
    // **"Cancel", not "Close".** This dialog can carry a "Close It and Launch"
    // response beside it, and two buttons both beginning with "Close" left the
    // dismissing one ambiguous about what it closed -- the dialog, or the other
    // client. Reported as the dialog having three buttons, two of which appear
    // to do the same thing. They never did: this one only dismisses.
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("profile", "Choose a Profile");
    if ours.is_some() {
        dialog.add_response("stop", "Close It and Launch");
        dialog.set_response_appearance("stop", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("stop"));
    } else {
        dialog.set_response_appearance("profile", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("profile"));
    }

    let parent_window = parent.clone();
    let toasts = toasts.clone();
    let config = config.clone();
    let join = join.clone();
    dialog.connect_response(None, move |_, response| {
        match response {
        // The switcher is the combo row above the launch button; activating the
        // window's action rather than building a second chooser here keeps one
        // construction site.
        "profile" => {
            let _ = parent_window.activate_action("win.profile", None);
        }
        "stop" => {
            if let Some(h) = ours.clone() {
                close_then_launch(
                    parent_window.clone(),
                    toasts.clone(),
                    config.clone(),
                    join.clone(),
                    h,
                );
            }
        }
        _ => {}
        }
    });
    dialog.present(Some(parent));
}

/// What the refusal says, given who is holding the profile.
///
/// Pulled out of the dialog and made pure so the wording can be pinned by
/// tests: a widget that has to be built and clicked to inspect is a widget
/// whose text drifts, and this one was wrong for months without anyone noticing.
fn busy_body(holder: Option<&profile::Holder>) -> String {
    // Short on purpose. This dialog is in the way of pressing play, and the
    // buttons under it already say what can be done -- "Close It and Launch"
    // is the whole answer for the common case. The long version explained the
    // storage model, why one client at a time, and where a stray client comes
    // from; all true, none of it changing which button to press. Someone who
    // wants that can find it on ADR-012.
    match holder {
        Some(h) if h.is_cordial() => {
            let started =
                h.running_for_text().map(|t| format!(", up {t}")).unwrap_or_default();
            format!("Another Cordial client has it open (process {}{started}).", h.pid)
        }
        // Kept longer than the others, because this is the one case Cordial
        // will not act on and the user has to go and find the process itself.
        Some(h) => format!(
            "Process {} has it open, and it is not a Cordial client:\n\n{}",
            h.pid, h.command
        ),
        None => "Something else has it open, and Cordial cannot tell what.".to_string(),
    }
}

/// Stop the client holding a profile, then launch once it is actually gone.
///
/// Waiting on [`profile::Holder::has_exited`] rather than sleeping a fixed
/// interval, because the thing being waited for is a Roblox client flushing
/// storage and there is no interval that is both short enough to feel like a
/// button and long enough to be true.
///
/// **Escalates instead of giving up.** The previous version sent one
/// `SIGTERM`, waited ten seconds, and if that was not enough it told the user
/// to press Roblox again — which is the bug this replaces: a real shutdown
/// here has been observed to take longer than that ten seconds, so the first
/// press signalled the client and gave up, and the second press only worked
/// because the shutdown the first press started had, by then, finished. See
/// [`escalation_for`] for the thresholds this now waits through and why each
/// one is what it is, and [`profile::Holder::force_stop`] for what happens if
/// even those run out.
fn close_then_launch(
    window: gtk::Window,
    toasts: adw::ToastOverlay,
    config: Rc<RefCell<ShellConfig>>,
    join: PendingJoin,
    holder: profile::Holder,
) {
    if let Err(message) = holder.ask_to_stop() {
        alert(&window, "Could not close the other client", &message);
        return;
    }

    // Sampled now, before any waiting, so the warning at `STOP_WARN` has a
    // baseline to compare against rather than only the instant of the
    // warning itself — see `stuck_diagnosis` for why one sample alone cannot
    // tell a slow shutdown from a stopped one.
    let started_cpu = holder.cpu_ticks();
    let waited = Cell::new(Duration::ZERO);
    let stage = Cell::new(Escalation::KeepWaiting);

    glib::timeout_add_local(STOP_POLL, move || {
        if holder.has_exited() {
            activate_roblox(&window, &toasts, &config, &join);
            return glib::ControlFlow::Break;
        }
        waited.set(waited.get() + STOP_POLL);
        let now = escalation_for(waited.get());

        // Each tier's action fires once, on the tick where the stage first
        // advances into it — `stage` is what makes that idempotent against a
        // timer that keeps calling this closure every `STOP_POLL` for as long
        // as the tier lasts.
        if now != stage.get() {
            stage.set(now);
            match now {
                Escalation::KeepWaiting => {}
                Escalation::Warn => {
                    let diagnosis = stuck_diagnosis(started_cpu, holder.cpu_ticks(), holder.wchan().as_deref());
                    println!(
                        "  shell: process {} still stopping after {}s: {diagnosis}",
                        holder.pid,
                        waited.get().as_secs()
                    );
                    toasts.add_toast(adw::Toast::new(&format!(
                        "Still closing the other Roblox client (process {}). {diagnosis}",
                        holder.pid
                    )));
                }
                Escalation::Kill => {
                    println!(
                        "  shell: process {} has not stopped after {}s; forcing it closed",
                        holder.pid,
                        waited.get().as_secs()
                    );
                    if let Err(message) = holder.force_stop() {
                        alert(&window, "Could not close the other client", &message);
                        return glib::ControlFlow::Break;
                    }
                    toasts.add_toast(adw::Toast::new(&format!(
                        "Process {} did not respond; closing it and continuing.",
                        holder.pid
                    )));
                }
                Escalation::GiveUp => {
                    // `SIGKILL` cannot be caught, deferred or blocked, so a
                    // process that is still here despite it is not merely
                    // slow — it is stuck in the kernel (uninterruptible I/O,
                    // state `D`) in a way nothing in user space can end. That
                    // is rare enough, and different enough from every other
                    // branch here, that it is worth its own honest sentence
                    // rather than folding it into the wording above.
                    alert(
                        &window,
                        "The other client will not close",
                        &format!(
                            "Process {} did not stop even after being forced to close. This is \
                             unusual — it is likely stuck waiting on the kernel rather than on \
                             Roblox. Wait a moment and press Roblox again, or investigate the \
                             process yourself (its pid is {}).",
                            holder.pid, holder.pid
                        ),
                    );
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

/// What to do about a client that has been asked to stop and has not yet,
/// given how long ago that was asked.
///
/// Pure and separate from the timer that drives it, the same shape
/// `busy_body` and `initial_size` take for the same reason: a threshold that
/// can only be checked by starting a real client and waiting is a threshold
/// nobody re-verifies, and this one has already been wrong once — the value
/// this replaces, `STOP_GIVE_UP: Duration::from_secs(10)`, was reached by a
/// real shutdown on this developer's own machine, which is the bug report
/// this function exists to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escalation {
    /// Nothing worth doing or saying yet.
    KeepWaiting,
    /// Worth telling the user something, and worth naming what the wait looks
    /// like — see [`stuck_diagnosis`] — but not yet worth acting on: this is
    /// still within reach of an ordinary graceful shutdown.
    Warn,
    /// Long enough that it is no longer being treated as ordinary. `SIGKILL`,
    /// once, and keep waiting for the kernel to actually reap it.
    Kill,
    /// `SIGKILL` was sent and the process is still here. Nothing left to
    /// escalate to; say so.
    GiveUp,
}

/// Past which point, since `SIGTERM` was sent, a client that has not stopped
/// is worth mentioning to the user rather than waiting on in silence.
///
/// The exact value of the constant this replaced: `STOP_GIVE_UP` used to be
/// ten seconds and be the *whole* budget, and this bug report is a real
/// shutdown that outlasted it. Kept as the point past which staying silent
/// stops being honest, rather than discarded — ten seconds genuinely is past
/// what a quick shutdown here takes, so it is still the right moment to say
/// something. It is simply no longer the moment to give up.
const STOP_WARN: Duration = Duration::from_secs(10);

/// Past which point a client that has not stopped is treated as unresponsive
/// rather than slow, and is sent `SIGKILL`.
///
/// Three times [`STOP_WARN`], and grounded in the one number this project
/// actually knows about a graceful shutdown's slowest scheduled step:
/// `secrets::CALL_TIMEOUT` gives a keyring write up to five seconds before it
/// gives up, because that call runs on the looper thread during a save. Even
/// a shutdown that hits a wedged keyring, waits out that timeout, and then
/// has to flush Roblox's own storage on top of it is not this slow twice
/// over. Past this point the client is not doing that; it is not doing
/// anything.
const STOP_KILL: Duration = Duration::from_secs(30);

/// Past which point, since `SIGKILL` was sent, a client that is somehow still
/// here is given up on rather than waited on further.
///
/// A runnable or ordinarily-sleeping process cannot survive `SIGKILL` for
/// more than a scheduling tick, so five seconds of grace after
/// [`STOP_KILL`] is not a race against the kernel — it exists only for the one
/// state that *can* survive a kill signal, uninterruptible I/O (`D` in
/// `ps`), which is rare and which nothing in user space can shorten. Waiting
/// longer would not change the answer; it would only make the dialog that
/// finally appears feel more like a hang than the honest report it is meant
/// to be.
const STOP_GIVE_UP: Duration = Duration::from_secs(35);

fn escalation_for(elapsed: Duration) -> Escalation {
    if elapsed >= STOP_GIVE_UP {
        Escalation::GiveUp
    } else if elapsed >= STOP_KILL {
        Escalation::Kill
    } else if elapsed >= STOP_WARN {
        Escalation::Warn
    } else {
        Escalation::KeepWaiting
    }
}

/// What to say about a client that is still running past [`STOP_WARN`],
/// given two samples of [`profile::Holder::cpu_ticks`] taken across the wait
/// and the kernel function its main thread is blocked in when that can be
/// read at all.
///
/// This is the highest-value part of the whole change and the part worth
/// being honest about the limits of. It cannot name a source line the way a
/// debugger attached to the process could — AGENTS.md's own account of the
/// AB-BA deadlock in `AudioDevice::close` found that with `lldb`, not with
/// this. What it *can* do without asking a user to install a debugger is the
/// distinction AGENTS.md says a backtrace alone cannot make either: "a
/// spinning pump and a blocked one produce identical backtraces... always
/// quote the process's CPU beside the stack." Two free reads of `/proc` are
/// enough for that half of the diagnosis, which is real information a user
/// can act on — "it's still doing something, wait" against "it's not doing
/// anything, this is probably stuck" — even without the other half.
fn stuck_diagnosis(before: Option<u64>, after: Option<u64>, wchan: Option<&str>) -> String {
    let activity = match (before, after) {
        (Some(b), Some(a)) if a > b => {
            "It has used CPU time since being asked to close, so it is doing something rather \
             than stuck."
                .to_string()
        }
        (Some(_), Some(_)) => {
            "It has used no CPU time since being asked to close.".to_string()
        }
        _ => "Its CPU use could not be read.".to_string(),
    };
    match wchan {
        Some(w) => format!("{activity} The kernel reports it waiting in {w}."),
        None => activity,
    }
}

/// How often to look for the old client having gone.
const STOP_POLL: Duration = Duration::from_millis(250);

/// Say something went wrong, in a dialog the user has to acknowledge.
///
/// A toast would be wrong for these: every one of them needs an action from the
/// user, and a message that fades after four seconds is barely better than no
/// message. The toast is for the good case.
pub fn alert(parent: &gtk::Window, heading: &str, body: &str) {
    // In-window, for the reason `profile_busy` gives: a toplevel dialog gives
    // focus away when it closes.
    let dialog = adw::AlertDialog::builder().heading(heading).body(body).build();
    dialog.add_response("ok", "Close");
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn holder(command: &str) -> profile::Holder {
        profile::Holder {
            pid: 649889,
            command: command.into(),
            running_for: Some(Duration::from_secs(31 * 60)),
        }
    }

    #[test]
    fn a_profile_with_nothing_saved_opens_at_the_built_in_default() {
        let state = window_state::WindowState::default();
        assert_eq!(initial_size(&state), (DEFAULT_WIDTH, DEFAULT_HEIGHT));
    }

    #[test]
    fn a_profile_with_a_saved_size_opens_at_that_size() {
        let state = window_state::WindowState { fullscreen: false, maximised: false, width: Some(1024), height: Some(768) };
        assert_eq!(initial_size(&state), (1024, 768));
    }

    #[test]
    fn a_profile_with_only_one_dimension_saved_falls_back_for_the_other() {
        // Should not happen from this file's own writer, which always sets
        // both together, but a hand-edited file could carry just one -- and
        // the fallback per field is simpler and no worse than refusing the
        // whole saved size over half of it.
        let state = window_state::WindowState { fullscreen: false, maximised: false, width: Some(1024), height: None };
        assert_eq!(initial_size(&state), (1024, DEFAULT_HEIGHT));
    }

    #[test]
    fn the_no_window_case_is_explained_rather_than_left_a_mystery() {
        // What must survive is the identification: which process, and how long
        // it has been up, because that is what tells somebody whether it is the
        // window in front of them or a leftover. The paragraph explaining that
        // a missing window is expected has gone -- the "Close It and Launch"
        // button makes the point without needing a window to be found at all.
        let body = busy_body(Some(&holder("/app/bin/cordial-run --profile default")));
        assert!(body.contains("process 649889"), "{body}");
        assert!(body.contains("31 minutes"), "{body}");
        assert!(!body.contains("close the window that already has it"), "{body}");
    }

    #[test]
    fn a_stranger_holding_the_lock_is_named_but_not_offered_up_for_killing() {
        // is_cordial gates the "Close It and Launch" response, so the body for
        // a process Cordial did not start must not imply it can be closed from
        // here. It says what has the file open and stops.
        let body = busy_body(Some(&holder("/usr/bin/grep -r something")));
        // The naming is the load-bearing part and it stays. The sentence about
        // not closing what Cordial did not start went with the rest of the
        // prose: the dialog simply does not offer a "Close It" button in this
        // case, which says the same thing in the place people actually look.
        assert!(body.contains("not a Cordial client"), "{body}");
        assert!(body.contains("/usr/bin/grep"), "{body}");
    }

    #[test]
    fn the_waiting_link_is_shown_escaped_and_short() {
        // Both hazards in one line of text. The `&` is not hypothetical —
        // Roblox's own links join their fields with one — and an `AdwBanner`
        // fed raw markup drops the label entirely, so the queued join would
        // become a blank grey bar: the exact "it ignored my click" this banner
        // exists to prevent, wearing the banner as a disguise.
        // The banner no longer shows the payload at all -- see
        // `deep_link::summarise`, which used to truncate to 64 characters and
        // put 24 characters of a one-time auth ticket on screen. So the first
        // thing to assert is absence.
        let line = banner_line("roblox-player://placeId=1818&launchData=<x>");
        assert!(!line.contains("placeId"), "{line}");
        assert!(!line.contains("launchData"), "{line}");
        assert!(line.contains("roblox-player:"), "{line}");
        assert!(line.contains("press Roblox"), "{line}");

        // The escaping stays even though nothing reaching it should now need
        // escaping. It is one call, and the failure it prevents is total: an
        // `AdwBanner` fed raw markup drops the label entirely, so the queued
        // join becomes a blank grey bar -- the exact "it ignored my click" this
        // banner exists to prevent, wearing the banner as a disguise. Belt and
        // braces on a label built from attacker-influenced input is the correct
        // amount of paranoia.
        assert!(!banner_line("roblox-player://<b>x</b>").contains("<b>"));

        // And it cannot stretch the window, whatever length Roblox's payload
        // happens to be this year.
        let long = banner_line(&format!("roblox-player://{}", "a".repeat(4000)));
        assert!(long.chars().count() < 120, "{}", long.chars().count());
    }

    #[test]
    fn an_unidentifiable_holder_says_so_instead_of_inventing_one() {
        // `holder_of` returns None for "could not tell", never for "nobody
        // holds it" -- the flock already proved somebody does. The body has to
        // preserve that distinction or it becomes a message claiming the
        // profile is both taken and free.
        let body = busy_body(None);
        assert!(body.contains("cannot tell what"), "{body}");
        assert!(!body.contains("process 649889"), "{body}");
    }

    #[test]
    fn a_quick_shutdown_is_never_warned_about() {
        // The ordinary case, and the one that must stay silent: most closes
        // finish in well under STOP_WARN, and a dialog or toast popping up for
        // every routine "close it and launch" would train users to ignore it.
        assert_eq!(escalation_for(Duration::ZERO), Escalation::KeepWaiting);
        assert_eq!(escalation_for(Duration::from_secs(9)), Escalation::KeepWaiting);
    }

    #[test]
    fn the_bug_reports_own_ten_seconds_now_only_warns() {
        // This is the regression test for the actual report: ten seconds was
        // the whole budget before, and a real shutdown reached it. Now it must
        // be a status update, not a give-up.
        assert_eq!(escalation_for(Duration::from_secs(10)), Escalation::Warn);
        assert_eq!(escalation_for(Duration::from_secs(29)), Escalation::Warn);
    }

    #[test]
    fn only_a_long_silence_is_forced_closed() {
        assert_eq!(escalation_for(Duration::from_secs(30)), Escalation::Kill);
        assert_eq!(escalation_for(Duration::from_secs(34)), Escalation::Kill);
    }

    #[test]
    fn giving_up_is_reserved_for_surviving_sigkill() {
        assert_eq!(escalation_for(Duration::from_secs(35)), Escalation::GiveUp);
        assert_eq!(escalation_for(Duration::from_secs(600)), Escalation::GiveUp);
    }

    #[test]
    fn the_thresholds_are_in_the_order_the_state_machine_assumes() {
        // `close_then_launch` relies on `escalation_for` being monotonic in
        // `elapsed` -- warn, then kill, then give up, never out of order --
        // and that is a property of the three constants, not of the function
        // reading them. Pinned directly so a future edit to one constant
        // cannot silently invert the sequence.
        assert!(STOP_WARN < STOP_KILL, "warning must come before forcing a close");
        assert!(STOP_KILL < STOP_GIVE_UP, "forcing a close must come before giving up on it");
    }

    #[test]
    fn cpu_used_since_asking_reads_as_still_working() {
        let message = stuck_diagnosis(Some(1000), Some(1050), None);
        assert!(message.contains("doing something"), "{message}");
        assert!(!message.contains("no CPU"), "{message}");
    }

    #[test]
    fn no_cpu_used_since_asking_reads_as_likely_stuck() {
        let message = stuck_diagnosis(Some(1000), Some(1000), None);
        assert!(message.contains("no CPU"), "{message}");
    }

    #[test]
    fn an_unreadable_cpu_sample_says_so_rather_than_guessing() {
        // `/proc/<pid>/stat` can vanish out from under a read on a process
        // that exits mid-poll, or be unreadable across a mount namespace, and
        // this must not be read as either "using CPU" or "not using CPU" --
        // both are claims about a number the caller does not actually have.
        let message = stuck_diagnosis(None, None, None);
        assert!(message.contains("could not be read"), "{message}");
        assert!(!message.contains("doing something") && !message.contains("no CPU"), "{message}");
    }

    #[test]
    fn a_known_wait_channel_is_named_alongside_the_cpu_reading() {
        // The two readings are independent -- a process can be burning CPU in
        // one thread while its main thread waits in a syscall -- so both are
        // worth saying when both are known, not one in place of the other.
        let message = stuck_diagnosis(Some(1000), Some(1000), Some("futex_wait_queue_me"));
        assert!(message.contains("no CPU"), "{message}");
        assert!(message.contains("futex_wait_queue_me"), "{message}");
    }

    #[test]
    fn no_wait_channel_known_leaves_the_sentence_about_cpu_alone() {
        // A missing wchan must not read as "waiting in nothing" -- that would
        // be inventing a location the kernel did not actually give.
        let message = stuck_diagnosis(Some(1000), Some(1050), None);
        assert!(!message.contains("waiting in"), "{message}");
    }
}
