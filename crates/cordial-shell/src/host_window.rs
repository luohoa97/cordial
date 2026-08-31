//! The one window Cordial has: an `AdwWindow` carrying an `AdwToolbarView`,
//! a header bar, and a content slot.
//!
//! [ADR-002](../../../docs/adr/ADR-002-core-shell-and-ui-handoff.md) gives core
//! a shell — window, chooser, an escape hatch — and
//! [ADR-011](../../../docs/adr/ADR-011-wayland-and-libadwaita.md) says of that
//! shell and the engine's host window that they "are the same window", because
//! "building the engine's host window as a bare Wayland surface would mean
//! building the shell twice, and the second one would have to inherit the theme
//! anyway". This module is where that sentence stops being an intention: the
//! shell binary fills the content slot with the chooser, and `cordial-runtime`
//! fills it with the engine's Wayland subsurface. One window definition, two
//! callers.
//!
//! It stays deliberately thin. Anything that grows — settings, themes,
//! plugin-contributed views — belongs to the UI plugin, not here.

use gdk4_wayland::prelude::*;
use libadwaita as adw;
use libadwaita::glib;
use libadwaita::gtk;
use libadwaita::prelude::*;
use std::ffi::c_void;
use std::time::{Duration, Instant};

/// The `xdg_toplevel` app_id this window carries, and X11's `WM_CLASS` before
/// it. Must keep matching `StartupWMClass` in
/// `packaging/io.github.luohoa97.Cordial.desktop` for the reasons in ADR-009 — GNOME
/// Shell, the screen-cast portal's window picker and every capture tool match a
/// window to its desktop entry through this string, and a drift shows up as a
/// missing icon rather than as an error. Pinned by
/// `tests::app_id_matches_the_desktop_entry`.
pub const APP_ID: &str = "Cordial";

/// What the header bar says.
///
/// It used to name the graphics backend and the name it gave was "OpenGL ES",
/// which is false: the engine reaches its landing page through Vulkan on both
/// backends — 547, 548 and 550 `vkQueuePresentKHR` calls over three
/// consecutive 25-second runs, with every GLES counter at zero in the same
/// runs. A title bar is a poor place to report something that can change at
/// run time and an even poorer place to report it wrongly, so it now reports
/// nothing about graphics at all; `CORDIAL_COUNT_GL=1` answers the question
/// the backend suffix was trying to answer, and answers it with counts.
/// The version is what `git describe` says, not what `Cargo.toml` says — see
/// `build.rs`. On a tag that is the tag alone; off one it carries the distance
/// and the hash so a bug report identifies a commit rather than a range of
/// dozens; and with uncommitted changes it ends in `-dirty`, which is the case
/// that matters most here. A binary built from a working tree several agents
/// were editing looked exactly like a committed one, and an afternoon went into
/// a regression nobody could attribute to a tree.
pub fn title() -> String {
    // The name rather than a literal, so the twice-a-year joke reaches the one
    // place a user actually reads it. See `branding`: decided once, never
    // polled, and never applied to anything in the repository.
    format!("{} {}", crate::branding::current().name(), crate::version::full())
}

/// How much of a monitor to leave for whatever else is on it.
///
/// Wayland has no way to ask for a work area — a panel is just another client,
/// so `gdk_monitor_get_geometry` is the whole of what GDK can tell anyone and
/// there is no `get_workarea` to pair with it. The space a desktop shell
/// reserves therefore has to be allowed for rather than read. GNOME's top bar
/// is 37 logical pixels at scale 1; this is roughly twice that, so a window
/// clamped by [`fit_within`] still has somewhere to sit rather than being
/// placed with its bottom edge past the end of the screen.
const MONITOR_ALLOWANCE: i32 = 96;

/// Clamp a requested window size to something that fits on one monitor.
///
/// Kept pure and separate from the GDK lookups so the arithmetic is testable
/// without a display. The bug it exists for: on a dual-head desktop measuring
/// 5360x1440 in total — a 3440x1440 monitor with a 1920x1200 one beside it —
/// the window ran off the edge of the first screen, because nothing in this
/// file had ever asked how big a screen was. Sizes here are logical pixels,
/// which is what both `gdk_monitor_get_geometry` and
/// `gtk_window_set_default_size` speak, so no scale factor enters into it.
fn fit_within(requested: (i32, i32), monitor: (i32, i32)) -> (i32, i32) {
    // The floors matter more than they look: a monitor smaller than the
    // allowance would otherwise produce a zero or negative size, and GTK
    // treats that as "no default size at all" rather than as an error.
    let max_w = (monitor.0 - MONITOR_ALLOWANCE).max(320);
    let max_h = (monitor.1 - MONITOR_ALLOWANCE).max(240);
    (requested.0.min(max_w), requested.1.min(max_h))
}

/// The geometry of every monitor GDK currently lists.
fn monitor_geometries() -> Vec<(i32, i32)> {
    let Some(display) = gtk::gdk::Display::default() else {
        return Vec::new();
    };
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i))
        .filter_map(|m| m.downcast::<gtk::gdk::Monitor>().ok())
        .map(|m| {
            let g = m.geometry();
            (g.width(), g.height())
        })
        .collect()
}

/// The smallest monitor attached, which is the only safe guess before the
/// window exists.
///
/// Wayland does not let a client choose, or even learn in advance, which
/// output its toplevel will be mapped on — that is the compositor's decision
/// and it is communicated after the fact, through `wl_surface.enter`. So the
/// size passed to `gtk_window_set_default_size` has to fit *whichever* monitor
/// the window lands on, and the smallest one is the only bound that does.
///
/// A second pass was tried and dropped: once the window is mapped,
/// `gdk_display_get_monitor_at_surface` says which output it really landed on,
/// which would allow a tighter clamp. It buys nothing — the build-time bound
/// already fits every monitor — and it costs a second `set_default_size` on a
/// window the engine is about to take its geometry from. `content_rect` is
/// where the subsurface's position, every pointer coordinate and the IME's
/// cursor rectangle come from, so it is not somewhere to perturb for a bound
/// that is already sufficient.
///
/// **Measured, on a 3440x1440 monitor beside a 1920x1200 one.** Asking for
/// 5000x1300: without this clamp the same tree yields a 3440x1301 window —
/// the whole width of the first screen — and the binary built from the
/// commit before it yields 5000x1300, which is the reported bleed. With it,
/// 1824x1058, twice each. The default 1280x720 is unaffected either way.
fn smallest_monitor() -> Option<(i32, i32)> {
    monitor_geometries().into_iter().min_by_key(|(w, h)| (*w as i64) * (*h as i64))
}

/// Bring GTK up for a process that is hosting the engine's Wayland surface.
///
/// Two things here are not the defaults and both were paid for.
///
/// The backend is forced to Wayland because the engine's surface has to be a
/// subsurface of a *Wayland* surface or it cannot be a subsurface at all. That
/// is a requirement of the caller, not a preference.
///
/// It takes both a call and an environment variable, which is worth spelling
/// out because the obvious half does not work. This developer's session — an
/// ordinary GNOME Wayland one — exports `GDK_BACKEND=x11`. With
/// `gdk_set_allowed_backends("wayland")` alone and that variable set, GTK 4.22
/// opens *no display at all* and `gtk_init_check` returns false with nothing
/// printed: under `GDK_DEBUG=misc` the trace reads `Skipping x11 backend` — so
/// the allowed-backends call was honoured — and then never says a word about
/// wayland, which the environment variable had already excluded. Two filters,
/// and their intersection was empty. The symptom is `Failed to initialize GTK`
/// and no window, so anyone who hits it will not guess.
///
/// The variable is only overwritten when it is set to something else, so on a
/// session that does not export it nothing here touches the environment. It is
/// still a process-global write with the engine's threads already running,
/// which is why it is conditional rather than unconditional.
///
/// `glib::set_prgname` because a window with no `GApplication` takes its
/// `xdg_toplevel.app_id` from the program name, which would otherwise be
/// `cordial-run`. See [`APP_ID`].
pub fn init_wayland() -> Result<(), String> {
    if std::env::var("GDK_BACKEND").is_ok_and(|v| v != "wayland") {
        // SAFETY: `g_setenv` is not thread-safe against a concurrent
        // `getenv`, which is why the standard library marks its equivalent
        // unsafe. This runs on the thread that is about to initialise GTK,
        // before any GTK or GDK call, and the engine's own threads do not read
        // this variable — nothing in `libroblox.so` has heard of GDK.
        unsafe { glib::setenv("GDK_BACKEND", "wayland", true) }
            .map_err(|e| format!("could not force GDK_BACKEND=wayland: {e}"))?;
    }
    gtk::gdk::set_allowed_backends("wayland");
    glib::set_prgname(Some(APP_ID));
    adw::init().map_err(|e| format!("libadwaita would not initialise: {e}"))?;
    unmute_waylands_own_errors();
    Ok(())
}

/// Let libwayland's fatal messages reach the terminal again.
///
/// A session was lost with this as its entire epitaph:
///
/// ```text
/// Gdk-Message: 14:10:43.968: Error 71 (Protocol error) dispatching to Wayland display.
/// ```
///
/// GDK prints that and calls `_exit(1)`. It names an errno and nothing else,
/// and 71 is `EPROTO` — the compositor rejected something the client sent.
/// libwayland *does* say which object and why, but GTK4 calls
/// `wl_log_set_handler_client` with a handler that logs at
/// `G_LOG_LEVEL_DEBUG`, and debug is dropped unless `G_MESSAGES_DEBUG` names
/// the domain. So the one line that answers the question is discarded by
/// default, roughly 50ms before the process dies.
///
/// Measured, by binding a global name mutter never advertised. Without this,
/// the whole of the output is the `Gdk-Message` above. With
/// `G_MESSAGES_DEBUG=all`, and now with this:
///
/// ```text
/// wl_registry#107: error 0: global wl_compositor (999999) is unavailable
/// ```
///
/// Installing a `Gdk`-domain handler rather than setting `G_MESSAGES_DEBUG`
/// keeps the other ~122 debug lines GDK emits per launch (portal settings,
/// mostly) out of the way; a handler registered here is called whatever
/// `G_MESSAGES_DEBUG` says, because that filter lives in GLib's *default*
/// handler and this replaces it for one domain.
///
/// The substring test is the weak part and is deliberately small. These are
/// the shapes libwayland uses when a connection is finished: `<interface>#<id>:
/// error <code>: <reason>` for a compositor-sent `wl_display.error`, and
/// `interface '<name>' has no event <n>` for an opcode past the end of one of
/// the hand-written tables in `cordial_runtime::android::wayland`. Missing a
/// third shape costs a diagnostic, not correctness — everything still goes to
/// GDK's own handler as well.
fn unmute_waylands_own_errors() {
    glib::log_set_handler(
        Some("Gdk"),
        glib::LogLevels::LEVEL_DEBUG,
        false,
        false,
        |_domain, _level, message| {
            if message.contains(": error ") || message.contains("has no event") {
                eprintln!("[wayland] {message}");
            }
        },
    );
}

/// A built, not-yet-presented shell window.
///
/// Holds GTK objects, which are `Rc`-refcounted and must only ever be touched
/// from the thread that ran [`init_wayland`]. Nothing in this type is
/// `Send`/`Sync` and it must not be made so; the runtime keeps its copy behind
/// a wrapper whose own comment names the same rule.
pub struct HostWindow {
    window: adw::Window,
    header: adw::HeaderBar,
    toolbar: adw::ToolbarView,
    /// The widget the content occupies. Its allocation — not the window's — is
    /// what the engine's subsurface is sized and positioned from, so that the
    /// header bar's height never has to be assumed anywhere.
    content: gtk::Widget,
    text_layer: gtk::Fixed,
    /// The editor itself: a real editable GTK widget, not a picture of one.
    ///
    /// It was a `GtkLabel` mirroring a buffer Cordial owned, with a caret
    /// Cordial drew. That worked and was miserable to use -- no selection, no
    /// click-to-position, no double-click-to-word, a caret that did not blink,
    /// and every one of those would have had to be written by hand. Reported as
    /// "the text field is so weird ... our own handling is ultra awkward".
    ///
    /// A `gtk::Text` is the bare editable widget behind `GtkEntry`, without the
    /// frame and background an entry draws, which is what suits sitting
    /// invisibly on top of a box Roblox has already drawn.
    editor: gtk::Text,
    /// Font size and colours for the current box. Reloaded when a box takes
    /// focus rather than styled per keystroke -- see [`HostWindow::set_text_overlay`].
    editor_css: gtk::CssProvider,
    /// Where the editor is in surface coordinates, so the input-region punch
    /// can hand that one rectangle back to GTK. `None` when nothing is focused.
    editor_rect: std::cell::Cell<Option<(i32, i32, i32, i32)>>,
    /// Set while Cordial is seeding the widget from the engine's text, so the
    /// change signal does not echo Cordial's own write straight back at it.
    editor_seeding: std::rc::Rc<std::cell::Cell<bool>>,
    /// What to tell when the user edits. Installed by the runtime, which owns
    /// the push to `syncTextboxTextAndCursorPosition2`.
    editor_changed: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(&str, i32)>>>>,
    /// The font family the engine draws with, once the runtime has found it in
    /// the APK. `None` until then, and `None` for ever if it is not there --
    /// see [`HostWindow::set_editor_font_family`].
    editor_font_family: std::cell::RefCell<Option<String>>,
    /// Whether the canvas is currently lowered. Remembered for the CSS class;
    /// the input region no longer depends on it, see [`HostWindow::set_canvas_cutout`].
    canvas_see_through: std::cell::Cell<bool>,
    /// The canvas rectangle in surface coordinates, as last given to
    /// [`HostWindow::set_canvas_cutout`]. Remembered so the input region can be
    /// rebuilt when the *editor* moves, without waiting for the next geometry
    /// sync to supply the canvas again.
    canvas_rect: std::cell::Cell<Option<(i32, i32, i32, i32)>>,
    /// Whether a web-view dialog is up, so [`input_region`] stops punching the
    /// canvas out of what GTK will accept clicks for.
    ///
    /// Separate from `canvas_see_through`, which both the editor and a dialog
    /// set: those two want opposite answers here. See `input_region`.
    dialog_up: std::cell::Cell<bool>,
}

/// The Android editor rectangle Roblox asks the platform to paint over its
/// surface while a TextBox has focus.
pub struct TextOverlay<'a> {
    pub text: &'a str,
    pub caret_chars: i32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Device pixels, the same units as `x`/`y`/`width`/`height` -- but unlike
    /// those, this is not necessarily the engine's own number verbatim. The
    /// runtime multiplies it by the resolved font's `fromRbxFontRatio` before
    /// it reaches here, when a per-box font was resolved; see
    /// `android::editor_font`'s note on that field for why.
    pub font_size: f32,
    pub text_color: u32,
    pub password: bool,
    /// The family this particular box is drawn in, when the runtime managed to
    /// resolve one out of the APK's own font table.
    ///
    /// Per box rather than per process because a game restyles individual
    /// TextBoxes: the login form and an in-experience chat entry are different
    /// fonts in the same session. `None` falls back to the process-wide family
    /// [`HostWindow::set_editor_font_family`] installed, which is Roblox's UI
    /// font and right for everything the game did not touch.
    pub font_family: Option<&'a str>,
    /// OpenType/CSS weight -- 400 regular, 700 bold -- meaningful only when
    /// `font_family` is `Some`. Roblox's own UI uses four weights of one
    /// family, so a family without a weight collapses Medium, Bold and
    /// ExtraBold into Regular.
    pub font_weight: i32,
    pub font_italic: bool,
    /// True when the engine gave no geometry and Cordial placed the editor
    /// itself. Such an editor is not sitting on the box, so it has to carry its
    /// own chrome to be legible -- see the CSS class below.
    pub fallback: bool,
    /// Roblox's `Enum.TextXAlignment`: `Left` = 0, `Center` = 1, `Right` = 2.
    /// These are Roblox's own published scripting-API ordinals -- not
    /// something read out of a binary -- and they line up with what
    /// `native/android_classes.cpp`'s `CordialTextBoxInfo::x_alignment`
    /// carries. See [`gtk_xalign`] for where they turn into a GTK property.
    pub x_alignment: i32,
    /// Roblox's `Enum.TextYAlignment`: `Top` = 0, `Center` = 1, `Bottom` = 2.
    /// See [`vertical_placement`] -- `gtk::Text` has no equivalent property of
    /// its own, so this is applied by resizing the widget rather than by
    /// setting an attribute.
    pub y_alignment: i32,
}

/// GTK's `Editable::set_alignment` fraction for Roblox's `xAlignment`: `0.0`
/// is flush left, `1.0` flush right, `0.5` centred -- the same three points a
/// `TextXAlignment` names, just spelled as a fraction instead of an enum.
///
/// **Confirmed, not inferred.** Every box this project has focused before
/// today read `xAlignment=Left` here, and nothing anywhere applied it -- a
/// TextBox actually styled `Center` or `Right` would draw the engine's own
/// text centred or flush right and Cordial's editor flush left underneath it,
/// which is one plausible reading of "the text box isn't centred". Which slot
/// carries the value, and that it is `xAlignment` and not `font` (the two
/// were genuinely ambiguous from Cordial's own two-box capture -- both read
/// `0` on both boxes), is settled by mocktail's `NativeTextBoxInfo`
/// constructor, `src/jnivm/jnivm.cc:4016-4024` (Apache-2.0): its varargs
/// reader lists the six int arguments in declared order as `xAlignment,
/// yAlignment, textColor, font, textInputType, returnKeyType`, which is a
/// fact about Roblox's platform API rather than about mocktail's own
/// implementation. The ordinals themselves are Roblox's published
/// `Enum.TextXAlignment`, not read out of mocktail at all.
fn gtk_xalign(x_alignment: i32) -> f32 {
    match x_alignment {
        2 => 1.0,
        1 => 0.5,
        // `0` (Left) and anything this build has never seen: Roblox's own
        // default, and the one value this project has actually observed.
        _ => 0.0,
    }
}

/// Where to draw a box's one line of text vertically, given Roblox's
/// `yAlignment`, as `(y, height)` to hand `gtk::Fixed::move_` and
/// `set_size_request` in place of the box's own `y`/`height`.
///
/// **`gtk::Text` has no vertical-alignment property**, because it is a
/// single-line widget and Pango simply centres whatever it is given within
/// the height it is allocated -- which is exactly why `Center` needed no code
/// at all: every box `cordial_textbox` has ever reported reads `yAlignment=1`
/// (Centre), and the measured caret centre already lands within 0.5px of the
/// box's own centre (`docs/NEXT.md`, 2026-08-30) by doing nothing. `Top` and
/// `Bottom` are approximated by shrinking the widget to its own natural line
/// height and anchoring that at the box's edge instead of letting it fill the
/// box and centre itself.
///
/// `natural_h` comes from `gtk::Widget::measure`, taken after the font
/// attributes are set, rather than from a guessed line-height multiple of the
/// font size -- there was nothing to measure a fudge factor against, since no
/// box this project has focused has ever used anything but `Center`. Treat
/// the `Top`/`Bottom` branches as **`UNVERIFIED`** end to end for that reason;
/// [`vertical_placement_tests`] covers the arithmetic, not a live box.
fn vertical_placement(y_alignment: i32, box_y: i32, box_h: i32, natural_h: i32) -> (i32, i32) {
    let h = natural_h.clamp(1, box_h.max(1));
    match y_alignment {
        0 => (box_y, h),
        2 => (box_y + (box_h - h).max(0), h),
        // `1` (Centre), and anything this build has never seen: unchanged
        // from before this function existed.
        _ => (box_y, box_h),
    }
}

/// Pango's `Weight` from an OpenType weight number.
///
/// A match rather than `Weight::__Unknown`, which is `doc(hidden)` and would be
/// reaching behind the binding. The ladder is Pango's own constants, and the
/// nearest-below rule means a font declaring some off-ladder weight still asks
/// for the closest thing Pango has a name for instead of silently becoming
/// Normal.
fn pango_weight(open_type: i32) -> gtk::pango::Weight {
    use gtk::pango::Weight;
    match open_type {
        ..=149 => Weight::Thin,
        150..=249 => Weight::Ultralight,
        250..=324 => Weight::Light,
        325..=364 => Weight::Semilight,
        365..=389 => Weight::Book,
        390..=449 => Weight::Normal,
        450..=549 => Weight::Medium,
        550..=649 => Weight::Semibold,
        650..=749 => Weight::Bold,
        750..=849 => Weight::Ultrabold,
        850..=949 => Weight::Heavy,
        _ => Weight::Ultraheavy,
    }
}

impl HostWindow {
    /// Build the window with an empty canvas in the content slot.
    ///
    /// `width`/`height` are the *content* size the caller wants; the header bar
    /// is added on top of that, so the engine gets the resolution it asked for
    /// rather than that minus a titlebar.
    pub fn with_canvas(title: &str, width: i32, height: i32) -> Self {
        // A `GtkDrawingArea` with no draw function paints nothing at all, so
        // what shows through is the themed window background — which is
        // exactly what ADR-011 asks for behind the canvas ("the desktop's own
        // background colour, following light and dark mode, rather than a
        // flash of white"), with no CSS of Cordial's own involved.
        let canvas = gtk::DrawingArea::new();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        let host = Self::new(title, width, height, &canvas);
        host.window.add_css_class("cordial-engine-host");
        // **The window itself is not transparent, and must not be.**
        //
        // It was, briefly, and the result was a window nobody could see at all
        // -- not the canvas, not the header bar, not the background. Reported
        // as "its just an invisible window, i cant see it, alt tab its just
        // invisible, but its there".
        //
        // A transparent toplevel is only safe while the engine's subsurface is
        // both painting and stacked above it, and neither holds reliably today:
        // the canvas is lowered whenever a web-view dialog or the text overlay
        // is up, and there is an open bug where the engine presents one frame
        // and stops. In either state the desktop shows through and there is
        // nothing left to click, read, or even find. The drawing area alone
        // stays transparent, which is all the engine needs.
        //
        // The header bar takes the desktop's own colours -- that part of
        // 3d67e59 is worth keeping, and is what stopped the bar being drawn on
        // a transparent custom background. Its *sizing* is not: the same commit
        // shrank it to a 30px min-height with 24px controls against a
        // libadwaita default nearer 47px, which read as "the x in the title bar
        // looks off and the titlebar looks short". That is now opt-in through
        // the Appearance page rather than the only option.
        let compact = matches!(std::env::var("CORDIAL_TITLE_BAR").as_deref(), Ok("compact"));
        // The see-through state is a class rather than the default, and that
        // distinction is the whole lesson of 5a295e3. A permanently transparent
        // toplevel shows the desktop whenever the engine is not painting, which
        // is a window nobody can find. Transparent *only while the engine is
        // deliberately lowered* is safe, because in that state the engine is by
        // definition the thing painting the canvas.
        let mut sheet = String::from(
            ".cordial-engine-host drawingarea { background-color: transparent; } \
             .cordial-engine-host headerbar { \
                 background-color: @headerbar_bg_color; \
                 color: @headerbar_fg_color; \
                 background-image: none; \
             } \
             .cordial-engine-host.cordial-canvas-below, \
             .cordial-engine-host.cordial-canvas-below toolbarview, \
             .cordial-engine-host.cordial-canvas-below overlay { \
                 background-color: transparent; \
                 background-image: none; \
             } \
             .cordial-editor { \
                 background: none; \
                 background-image: none; \
                 border: none; \
                 box-shadow: none; \
                 outline: none; \
                 padding: 0; \
                 margin: 0; \
                 min-height: 0; \
                 min-width: 0; \
             } \
             .cordial-text-fallback { \
                 background-color: rgba(28, 28, 30, 0.94); \
                 color: #ffffff; \
                 border: 1px solid rgba(255, 255, 255, 0.25); \
                 border-radius: 8px; \
                 padding: 6px 10px; \
             }",
        );
        if compact {
            sheet.push_str(
                " .cordial-engine-host headerbar { min-height: 30px; padding: 0 6px; } \
                 .cordial-engine-host headerbar windowcontrols button { \
                     min-width: 24px; min-height: 24px; padding: 0; margin: 2px; \
                 }",
            );
        }
        let css = gtk::CssProvider::new();
        css.load_from_string(&sheet);
        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(&host.window),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        // The editor's own sheet, empty until a box takes focus and reloaded
        // with that box's font size and colours each time one does. Separate
        // from the sheet above because that one is written once and this one is
        // rewritten, and reparsing the whole stylesheet to restyle one widget
        // would restyle the header bar too.
        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(&host.window),
            &host.editor_css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        host
    }

    pub fn new(title: &str, width: i32, height: i32, content: &impl IsA<gtk::Widget>) -> Self {
        let header = adw::HeaderBar::new();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(content));
        let text_layer = gtk::Fixed::new();
        // **Off unless an editor is up, and this is not a tidiness setting.**
        // A `GtkFixed` is picked over its whole allocation, so a targetable one
        // spanning the content would swallow every pointer event GTK receives.
        // It is switched on for exactly as long as there is something in it to
        // click, which is also exactly as long as the input region has a hole
        // punched for that something.
        text_layer.set_can_target(false);
        text_layer.set_hexpand(true);
        text_layer.set_vexpand(true);

        let editor = gtk::Text::new();
        editor.set_visible(false);
        editor.add_css_class("cordial-editor");
        // The engine has already drawn the box; the editor supplies the text
        // and the caret and nothing else. `gtk::Text` is the bare widget from
        // inside a `GtkEntry` and draws no frame of its own -- there is
        // deliberately no `set_has_frame` here, and reaching for one resolves
        // to `ButtonExt`'s and fails to compile, which is a confusing way to
        // be told the widget was already the right choice. The background and
        // padding an entry would draw are turned off in CSS instead.
        editor.set_overflow(gtk::Overflow::Hidden);
        // **Keep the canvas's cursor, not a text widget's.** GTK gives text
        // widgets an I-beam, and the editor sits on a box the engine drew, so
        // hovering the box swapped the pointer for a GTK one and broke the
        // illusion that the box belongs to Roblox -- reported as "in sober ...
        // its like its part of roblox, but in ours we replace the cursor with
        // some gtk cursor when you hover over it". Nothing else in the canvas
        // sets a cursor, so the default arrow is what the rest of the surface
        // shows and what this must match.
        editor.set_cursor_from_name(Some("default"));

        let editor_css = gtk::CssProvider::new();
        let editor_seeding = std::rc::Rc::new(std::cell::Cell::new(false));
        let editor_changed: std::rc::Rc<
            std::cell::RefCell<Option<Box<dyn Fn(&str, i32)>>>,
        > = std::rc::Rc::new(std::cell::RefCell::new(None));
        {
            // One closure behind an `Rc`, shared by both signals: the text and
            // the caret are one fact as far as the engine is concerned --
            // `syncTextboxTextAndCursorPosition2` takes them together -- and
            // reporting them from two independent handlers would send the
            // engine a caret for text it had not been told about yet.
            let seeding = editor_seeding.clone();
            let sink = editor_changed.clone();
            let widget = editor.clone();
            let notify: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(move || {
                if seeding.get() {
                    return;
                }
                if let Some(cb) = sink.borrow().as_ref() {
                    cb(widget.text().as_str(), widget.position());
                }
            });
            let on_text = notify.clone();
            editor.connect_changed(move |_| on_text());
            editor.connect_notify_local(Some("cursor-position"), move |_, _| notify());
        }
        text_layer.put(&editor, 0.0, 0.0);

        overlay.add_overlay(&text_layer);
        toolbar.set_content(Some(&overlay));

        // Clamped before the window exists, because a default size larger than
        // the screen is not something the compositor will correct for you: it
        // maps the toplevel at the size asked for and the far edge simply ends
        // up past the end of the monitor. On a dual-head desktop that reads as
        // the window bleeding onto the second screen, which is how this was
        // first reported.
        let (w, h) = match smallest_monitor() {
            Some(monitor) => fit_within((width, height + header_height_hint()), monitor),
            None => (width, height + header_height_hint()),
        };

        let window = adw::Window::builder()
            .title(title)
            .default_width(w)
            .default_height(h)
            .content(&toolbar)
            .build();

        // Hide the header bar while fullscreen, and put it back afterwards.
        //
        // GTK does not do this for you and is right not to: it cannot know
        // whether a given toolbar is chrome the user wants gone or part of the
        // application. For a game it is chrome. Without this, fullscreening
        // leaves the title bar sitting across the top of the picture, which is
        // what "fullscreen still shows the titlebar" means and is not a
        // compositor problem — the window really is fullscreen, the bar is
        // simply still inside it.
        //
        // `set_reveal_top_bars` rather than hiding the header directly, because
        // ToolbarView owns the space it occupies: hiding the child leaves the
        // gap it was sitting in.
        {
            let toolbar_for_fs = toolbar.clone();
            window.connect_fullscreened_notify(move |w| {
                toolbar_for_fs.set_reveal_top_bars(!w.is_fullscreen());
            });
        }

        HostWindow {
            window,
            header,
            toolbar,
            content: content.as_ref().clone(),
            text_layer,
            editor,
            editor_css,
            editor_rect: std::cell::Cell::new(None),
            canvas_see_through: std::cell::Cell::new(false),
            canvas_rect: std::cell::Cell::new(None),
            dialog_up: std::cell::Cell::new(false),
            editor_seeding,
            editor_changed,
            editor_font_family: std::cell::RefCell::new(None),
        }
    }

    pub fn window(&self) -> &adw::Window {
        &self.window
    }

    pub fn header(&self) -> &adw::HeaderBar {
        &self.header
    }

    pub fn toolbar(&self) -> &adw::ToolbarView {
        &self.toolbar
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Name the font family the editor should draw with.
    ///
    /// The runtime supplies this because it is the side that can reach the
    /// APK; this window only knows it wants to match whatever the engine uses.
    /// `None` leaves Pango to pick, which is the desktop UI font and visibly
    /// not what the engine drew -- acceptable as a fallback, never as the
    /// intent.
    pub fn set_editor_font_family(&self, family: Option<String>) {
        *self.editor_font_family.borrow_mut() = family;
    }

    /// Install a callback for edits the user makes in the editor.
    ///
    /// The runtime owns the push to the engine, because it owns the JNI handle
    /// and the decision about `pass_text` versus
    /// `syncTextboxTextAndCursorPosition2`. This window owns the widget and
    /// knows nothing about either.
    ///
    /// Text and caret arrive together and never separately, because the engine
    /// is told them together and a caret reported for text the engine has not
    /// seen yet is a caret past the end of the string it holds.
    pub fn connect_editor_changed<F: Fn(&str, i32) + 'static>(&self, f: F) {
        *self.editor_changed.borrow_mut() = Some(Box::new(f));
    }

    /// Place (or hide) the desktop equivalent of Android's transparent
    /// `EditText` over the engine canvas. The caller controls subsurface
    /// stacking; this method only updates GTK's parent surface.
    ///
    /// **The widget owns the text; this seeds it.** Android's own `EditText` is
    /// the authoritative buffer and syncs to the engine, and that is the shape
    /// being imitated. So the text passed in is written only when it differs
    /// from what the widget already holds -- a blind `set_text` on every pump
    /// tick would reset the selection, fight the user's caret, and echo back
    /// through [`Self::connect_editor_changed`] as though they had typed it.
    pub fn set_text_overlay(&self, overlay: Option<TextOverlay<'_>>) {
        let Some(overlay) = overlay else {
            self.editor.set_visible(false);
            self.text_layer.set_can_target(false);
            self.editor_rect.set(None);
            // Seeded, not typed: clearing on blur must not reach the engine as
            // an edit that empties the box the user just finished filling in.
            self.editor_seeding.set(true);
            self.editor.set_text("");
            self.editor_seeding.set(false);
            self.refresh_input_region();
            self.window.queue_draw();
            return;
        };

        let (x, y) = (overlay.x.round() as i32, overlay.y.round() as i32);
        let (w, h) = (
            overlay.width.max(1.0).round() as i32,
            overlay.height.max(1.0).round() as i32,
        );

        // Colours and size, as CSS rather than Pango attributes, because the
        // caret is the reason. GTK paints it from the CSS `caret-color`, which
        // defaults to the themed text colour and takes no notice of a Pango
        // foreground override -- so an attribute-styled editor on Roblox's
        // light search field drew dark text with the theme's white caret,
        // invisible on exactly the field it was in.
        let rgb = format!(
            "#{:02x}{:02x}{:02x}",
            (overlay.text_color >> 16) & 0xff,
            (overlay.text_color >> 8) & 0xff,
            overlay.text_color & 0xff
        );
        // **Colour here, size deliberately not.** A CSS `font-size` is scaled
        // by the desktop's text-scaling factor before it reaches Pango, so on
        // any setup with font scaling turned up -- which is most laptops -- the
        // editor drew visibly larger than the text Roblox draws in the same box
        // when it is not focused. Reported as "text gets too big when
        // selected", and "selected" is "focused", because the editor only
        // exists while the box has focus.
        //
        // The size is set below as an absolute Pango attribute instead, which
        // is in the same units as the rest of this spec.
        self.editor_css
            .load_from_string(&format!(".cordial-editor {{ color: {rgb}; caret-color: {rgb}; }}"));

        // `new_size_absolute`, not `new`. `AttrSize::new` takes points and Pango
        // converts them through the context's resolution; the absolute form takes
        // device units and skips that conversion entirely. The engine's
        // `fontSize` is in the same space as the `x`, `y`, `width` and `height`
        // beside it in the same struct, and those are placed straight into the
        // widget tree without a conversion -- so the size must not get a *DPI*
        // conversion either, or the text is the only part of the spec drawn in
        // different units from the box it goes in. It may already carry a
        // *font-specific* correction by the time it gets here -- see this
        // field's own doc comment -- and that is a different adjustment for a
        // different reason: it is reconciling Roblox's own two text stacks
        // with each other, not converting between Pango's and the engine's.
        let attrs = gtk::pango::AttrList::new();
        attrs.insert(gtk::pango::AttrSize::new_size_absolute(
            (overlay.font_size.max(1.0) * gtk::pango::SCALE as f32).round() as i32,
        ));
        // **The family matters as much as the size, and was missing.** Setting
        // only the size left Pango drawing the desktop's UI font over a string
        // the engine had drawn in its own, so focusing a box changed the shape
        // and weight of every character while the height stayed right. With
        // the engine's own family registered the two renderings agree.
        //
        // The box's own family wins over the process-wide one, because a game
        // may restyle a single TextBox and the login form in the same session.
        // The process-wide family stays as the fallback and is what still
        // works when the APK's font table cannot be read at all -- losing the
        // family entirely would put the desktop UI font back, which is the
        // original bug rather than a smaller version of it.
        //
        // Weight and style ride with the family and only with the family. A
        // weight applied over Pango's own choice of face would restyle the
        // desktop font, which is a second wrong answer rather than a partial
        // right one.
        let per_box = overlay.font_family;
        let process_wide = self.editor_font_family.borrow();
        if let Some(family) = per_box.or(process_wide.as_deref()) {
            attrs.insert(gtk::pango::AttrString::new_family(family));
        }
        if per_box.is_some() {
            attrs.insert(gtk::pango::AttrInt::new_weight(pango_weight(overlay.font_weight)));
            attrs.insert(gtk::pango::AttrInt::new_style(if overlay.font_italic {
                gtk::pango::Style::Italic
            } else {
                gtk::pango::Style::Normal
            }));
        }
        drop(process_wide);
        self.editor.set_attributes(Some(&attrs));

        // Horizontal alignment. Cheap and idempotent, so it is set on every
        // call rather than only when it changes -- the same editor widget is
        // reused across boxes, and a `Right`-aligned box followed by a
        // `Left`-aligned one has to put this back same as the family and the
        // input purpose below do.
        self.editor.set_alignment(gtk_xalign(overlay.x_alignment));

        // GTK's own masking rather than a string of bullets: the widget then
        // holds the real text, so the caret lands between real characters and
        // a paste or a selection means what it says. Substituting the bullets
        // ourselves would have made the buffer a lie the moment anything
        // measured it.
        self.editor.set_visibility(!overlay.password);

        // **And the purpose, which masking does not imply.** GTK's own
        // documentation for `set_visibility` says so directly: "you probably
        // want to set input-purpose to password or pin to inform input
        // methods about the purpose of this widget, *in addition to* setting
        // visibility to false."
        //
        // It matters because an input method decides what to offer from the
        // purpose, not from whether glyphs are drawn. Left unset, a composing
        // IME is entitled to show a candidate window containing what is being
        // typed into a masked field, and to keep it in its own learning
        // history. Neither is visible from this side, which is why the field
        // looked fine without it.
        //
        // Set on both branches, not just the password one, because the same
        // editor widget is reused for every box the engine focuses -- a
        // password box followed by a chat box has to put this back.
        self.editor.set_input_purpose(if overlay.password {
            gtk::InputPurpose::Password
        } else {
            gtk::InputPurpose::FreeForm
        });

        // **There is deliberately no clipboard guard here, and that is not an
        // oversight.** An audit reported that selecting a masked password and
        // pressing Ctrl+C copies it in clear, which would be worth fixing and
        // is not true: the same GTK documentation says the invisible char
        // "will also appear that way when the text in the widget is copied to
        // the clipboard". GTK masks the copy as well as the display.
        //
        // Written down because the fix for the claim is a one-liner that looks
        // obviously correct -- disable `clipboard.copy` for password fields --
        // and would cost a real password field its copy behaviour to defend
        // against nothing.

        if self.editor.text() != overlay.text {
            self.editor_seeding.set(true);
            self.editor.set_text(overlay.text);
            self.editor.set_position(overlay.caret_chars);
            self.editor_seeding.set(false);
        }

        // Vertical alignment. Measured after the attributes above are set,
        // because the natural height this asks for is the *drawn* font's line
        // height, not the engine's `fontSize` before the per-font ratio
        // correction -- see `TextOverlay::font_size`'s own comment for why
        // those differ. `Center` (`y_alignment == 1`, the only value this
        // project has ever measured) leaves `y`/`h` exactly as they arrived,
        // so this changes nothing for every box checked in docs/NEXT.md.
        let (_, natural_h, _, _) = self.editor.measure(gtk::Orientation::Vertical, -1);
        let (y, h) = vertical_placement(overlay.y_alignment, y, h, natural_h);

        self.editor.set_size_request(w, h);
        self.text_layer.move_(&self.editor, x as f64, y as f64);

        // **A bare editor is invisible half the time.** With the engine's own
        // spec it sits on the box and takes the box's colour, which is right.
        // With a synthesised placement it floats over whatever happens to be
        // there -- and Roblox's own search dropdown is white, so white text on
        // it renders nothing at all. Reported as "I don't think it's even
        // drawing"; it was drawing, in white, on white.
        if overlay.fallback {
            self.editor.add_css_class("cordial-text-fallback");
        } else {
            self.editor.remove_css_class("cordial-text-fallback");
        }

        self.editor.set_visible(true);
        self.text_layer.set_can_target(true);
        self.editor_rect.set(Some((x, y, w, h)));
        self.refresh_input_region();
        if !self.editor.has_focus() {
            // **`grab_focus_without_selecting`, and the difference is not
            // cosmetic.** GTK's `gtk-entry-select-on-focus` is on by default,
            // so a plain `grab_focus` on a text widget selects its entire
            // contents. The editor then comes up with everything selected and
            // the very next keystroke *replaces* the lot -- so focusing a box
            // that already had text in it and typing one character threw the
            // rest away. Reported as two separate symptoms that turned out to
            // be one bug: "text gets too big when selected" (that is the
            // selection highlight, on all of it) and "when you deselect and
            // select it again, it clears everything once you type".
            //
            // Measured: a sign-in username field holding `abcdefZ`, one real
            // keystroke through the compositor, and the engine afterwards
            // reported the box as one byte long.
            self.editor.grab_focus_without_selecting();
        }
        // **Nothing here touches the selection, and that is deliberate.**
        // There used to be a "collapse any stray selection" line, added to
        // belt-and-brace the select-on-focus fix above. It destroyed every
        // selection the user made: selecting text moves the cursor, moving the
        // cursor notifies, notifying pushes to the engine, pushing repaints the
        // overlay, and the repaint ran the collapse. Ctrl+A appeared to do
        // nothing and dragging across text would not highlight it.
        // `grab_focus_without_selecting` already handles the only case that
        // needed handling. The widget owns its selection.
        self.window.queue_draw();
    }

    /// The `wl_display` GTK opened, as a raw pointer.
    ///
    /// Everything Cordial does natively — the engine's own `wl_surface`, the
    /// `wl_subsurface` that parents it here, Mesa's Vulkan WSI and its EGL
    /// winsys — has to be on *this* connection and no other. Wayland object
    /// ids are scoped to the connection that made them, so a second connection
    /// would produce buffers that can never be attached to this surface. That
    /// is why this is exposed rather than the runtime opening its own.
    pub fn wl_display(&self) -> Option<*mut c_void> {
        let display = WidgetExt::display(&self.window).downcast::<gdk4_wayland::WaylandDisplay>().ok()?;
        display.wl_display_raw().map(std::ptr::NonNull::as_ptr)
    }

    /// GDK's own `wl_pointer` for the default seat, borrowed as a raw pointer.
    ///
    /// A Wayland client may create more than one `wl_pointer` from one seat,
    /// but GDK's is the object that owns the desktop cursor for this GTK
    /// window. Pointer constraints attached to a second object can receive a
    /// `locked` event without constraining GDK's cursor, which leaves the host
    /// pointer free to cross the window edge while the engine's cursor stays
    /// centred. The runtime borrows this object for its relative-pointer and
    /// constraint requests; ownership and destruction remain with GDK.
    pub fn wl_pointer(&self) -> Option<*mut c_void> {
        let display = WidgetExt::display(&self.window);
        let pointer = display.default_seat()?.pointer()?;
        let pointer = pointer.downcast::<gdk4_wayland::WaylandDevice>().ok()?;
        pointer.wl_pointer_raw().map(std::ptr::NonNull::as_ptr)
    }

    /// The toplevel's own `wl_surface` — the parent the engine's surface is
    /// made a subsurface of. `None` until the window has been presented and
    /// GTK has realised it, which is what [`Self::wait_until_mapped`] waits
    /// for.
    pub fn wl_surface(&self) -> Option<*mut c_void> {
        let surface = self.window.surface()?;
        let surface = surface.downcast::<gdk4_wayland::WaylandSurface>().ok()?;
        surface.wl_surface_raw().map(std::ptr::NonNull::as_ptr)
    }

    /// Make the window see-through so a lowered engine subsurface is visible,
    /// or opaque again when it is raised back.
    ///
    /// Paired with `set_engine_stacking`, and only meaningful together: GTK
    /// paints the window's own background across the canvas area, so declaring
    /// the region non-opaque is not enough on its own -- the pixels are still
    /// there. Measured: with the region punched but the background opaque, a
    /// lowered engine stays completely hidden.
    ///
    /// Deliberately not the default state. A toplevel that is always
    /// transparent shows the desktop through itself the moment the engine stops
    /// painting, which is the invisible window 5a295e3 had to fix. Tying it to
    /// the lowered state means it is only ever transparent while something is
    /// known to be painting underneath.
    /// A web-view dialog opened or closed.
    ///
    /// Rewrites the input region, because a modal wants the whole window and
    /// the ordinary layout gives the canvas rectangle to the engine. See
    /// [`input_region`] for why this is a different answer from the editor's.
    pub fn set_dialog_up(&self, up: bool) {
        if self.dialog_up.replace(up) == up {
            return;
        }
        self.refresh_input_region();
    }

    pub fn set_canvas_see_through(&self, on: bool) {
        if on {
            // **Every layer, not just the window.** Making the toplevel
            // transparent is not enough: `AdwToolbarView` sits between the
            // window and the canvas and paints `@window_bg_color` of its own,
            // so a lowered canvas stayed hidden behind a flat sheet of
            // #222226 while the engine went on presenting at sixty frames a
            // second. Measured in an experience with the chat box focused --
            // the whole window that colour, `presents` climbing 39138 to
            // 39321, and the game reappearing the instant the canvas was
            // raised again.
            //
            // fb67d71 measured the single-selector version working, and it
            // does on the pages where something else in the tree happens to be
            // transparent already. In an experience it is not.
            self.window.add_css_class("cordial-canvas-below");
        } else {
            self.window.remove_css_class("cordial-canvas-below");
        }
        self.canvas_see_through.set(on);
        // Both regions, now, rather than waiting for a geometry sync that may
        // never come -- see `refresh_opaque_region`.
        self.refresh_opaque_region();
        self.refresh_input_region();
    }

    /// Rebuild and apply the input region.
    ///
    /// Called from both the things it depends on -- the canvas moving and the
    /// editor moving -- because they change at different moments and the wrong
    /// combination is silent in both directions. A canvas with no hole punched
    /// for the editor swallows every click on the text field; a hole with no
    /// editor in it drops clicks meant for the game.
    fn refresh_input_region(&self) {
        let Some(surface) = self.window.surface() else { return };
        let Some(canvas) = self.canvas_rect.get().or_else(|| self.content_rect()) else { return };
        let region = input_region(
            (surface.width(), surface.height()),
            canvas,
            self.editor_rect.get(),
            self.dialog_up.get(),
        );
        surface.set_input_region(Some(&region));
    }

    /// Tell the compositor that the canvas rectangle is neither opaque nor
    /// ours to click, so a lowered engine subsurface shows through it.
    ///
    /// **This is what stops the game going black while a text overlay or a web
    /// view is up.** Those lower the engine beneath the parent so GTK can draw
    /// on top; with an opaque parent the compositor is then entitled to skip
    /// painting the engine entirely, and the whole canvas goes flat. Punching
    /// the rectangle out of the opaque region is the compositor's cue that
    /// there is something underneath worth compositing.
    ///
    /// The input region is punched to match, for the same reason and in the
    /// same call: whatever is visible there should also be clickable there.
    /// Without it the parent swallows clicks aimed at a lowered canvas --
    /// measured, with an explicit region two synthetic clicks produced four
    /// `nativePassMouseButton` calls into the engine and without it zero.
    ///
    /// Called from `sync_canvas_geometry`, which already runs on every
    /// allocation change. That is deliberate: GTK recomputes its own regions
    /// when the window resizes, so a region set once at startup would be
    /// silently reverted by the first resize, and the canvas would stop taking
    /// clicks with nothing to show why.
    /// Tell the compositor which part of the parent surface is the engine's.
    ///
    /// **This is the only place the input region is written, and that is the
    /// point.** There used to be a second writer -- the editor's hole was
    /// punched by `set_text_overlay` and this function, called from
    /// `sync_canvas_geometry` on every pump tick, immediately overwrote it with
    /// a region that knew nothing about the editor. The hole existed for a few
    /// milliseconds at a time, roughly twenty times a second, so a click into
    /// the text field essentially never landed. Reported as "you cant click to
    /// move the caret ... you cant drag to select text", and it survived a
    /// first fix that corrected the hole's coordinates while leaving the
    /// overwrite in place.
    ///
    /// The opaque region is *not* the same shape and must not be: it says what
    /// GTK paints solidly, and the editor is drawn over the canvas with a
    /// transparent background, so it stays part of the cut-out.
    /// Record where the engine's canvas is, then rewrite both regions.
    pub fn set_canvas_cutout(&self, x: i32, y: i32, w: i32, h: i32) {
        if w <= 0 || h <= 0 {
            return;
        }
        self.canvas_rect.set(Some((x, y, w, h)));
        self.refresh_opaque_region();
        self.refresh_input_region();
    }

    /// Tell the compositor which part of the parent surface it may skip.
    ///
    /// **Called from the two things it depends on, and that is the whole bug
    /// this function exists to fix.** It used to live inside
    /// `set_canvas_cutout`, which runs off `sync_canvas_geometry` -- so it was
    /// rewritten only when the *geometry* changed. Lowering the canvas does not
    /// change the geometry. In an experience, where the window has been the
    /// same size for minutes, the region therefore kept the value it was given
    /// while the canvas was still on top: fully opaque. The compositor was told
    /// the parent is opaque and skipped compositing the subsurface underneath,
    /// so the window was a flat sheet of #222226 no matter what GTK painted.
    ///
    /// That is why every attempt to fix this with CSS failed. Measured: with
    /// the toplevel *and every descendant* forced transparent, the window was
    /// still that exact colour, while the engine presented at sixty frames a
    /// second and the game reappeared the instant the canvas was raised. It
    /// was never a painting problem.
    ///
    /// On the landing page it happened to work, because moving between pages
    /// resizes things often enough that a geometry sync lands soon after the
    /// lower. That is luck, and it is why this looked state-dependent.
    fn refresh_opaque_region(&self) {
        let Some(surface) = self.window.surface() else { return };
        let (sw, sh) = (surface.width(), surface.height());
        if sw <= 0 || sh <= 0 {
            return;
        }

        // Nothing is opaque while the canvas is lowered: the toplevel is
        // transparent over it so the subsurface can show through, and claiming
        // otherwise is what hid it.
        if self.canvas_see_through.get() {
            #[allow(deprecated)]
            surface.set_opaque_region(None);
            return;
        }

        let Some((x, y, w, h)) = self.canvas_rect.get() else { return };

        // The window, not the surface. A GTK surface carries the client-side
        // decoration shadow -- a translucent margin around a floating window --
        // and declaring that opaque means the compositor does not repaint what
        // is behind it, so the window drags a stale halo around with it.
        // Measured under sway with the window floated: surface 1636x911
        // against window 1596x871, forty pixels in each axis.
        let (dx, dy) = self.window.surface_transform();
        let (ww, wh) = (self.window.width(), self.window.height());
        let (ox, oy, ow, oh) = if ww > 0 && wh > 0 {
            (dx.round() as i32, dy.round() as i32, ww, wh)
        } else {
            (0, 0, sw, sh)
        };

        let opaque =
            gtk::cairo::Region::create_rectangle(&gtk::cairo::RectangleInt::new(ox, oy, ow, oh));
        if opaque.subtract_rectangle(&gtk::cairo::RectangleInt::new(x, y, w, h)).is_err() {
            return;
        }
        // Deprecated since GDK 4.16, which computes its own from the render
        // tree -- and its own answer is "the whole surface", because the
        // drawing area being transparent is not something it can see through
        // to a subsurface with.
        #[allow(deprecated)]
        surface.set_opaque_region(Some(&opaque));
    }

    /// Whether the compositor currently considers this window focused.
    ///
    /// `xdg_toplevel`'s `activated` state, which GDK reports as
    /// `GdkToplevelState::FOCUSED`. `None` means there is no toplevel yet, on
    /// the same footing as [`Self::visible`].
    pub fn focused(&self) -> Option<bool> {
        let surface = self.window.surface()?;
        let toplevel = surface.downcast::<gtk::gdk::Toplevel>().ok()?;
        Some(toplevel.state().contains(gtk::gdk::ToplevelState::FOCUSED))
    }

    /// Whether the compositor still considers this window visible.
    ///
    /// `None` means there is no toplevel to ask yet, which is not `Some(false)`
    /// — the caller throttles on this, and treating "not realised" as "not
    /// visible" would throttle a window during its own bring-up.
    ///
    /// `SUSPENDED` is the interesting half and it is a real protocol answer
    /// rather than a guess: `xdg_toplevel`'s `suspended` state, added in
    /// xdg-shell version 6, is the compositor saying the surface's content is
    /// not visible and the client may stop drawing it. GDK surfaces it as
    /// `GdkToplevelState::SUSPENDED`. `MINIMIZED` is included because a
    /// minimised window is not visible either and a compositor is free to
    /// report only that.
    ///
    /// **Measured on this desktop rather than taken from the protocol text** —
    /// see `looper::pump`'s `minimise` script action and the report that used
    /// it. What a compositor does for a *covered* window is its own choice and
    /// is not established here.
    pub fn visible(&self) -> Option<bool> {
        let surface = self.window.surface()?;
        let toplevel = surface.downcast::<gtk::gdk::Toplevel>().ok()?;
        let hidden = gtk::gdk::ToplevelState::MINIMIZED | gtk::gdk::ToplevelState::SUSPENDED;
        Some(!toplevel.state().intersects(hidden))
    }

    /// The toplevel's whole state, for the instrumented run that established
    /// what [`Self::visible`] can actually see. Not used by anything else.
    pub fn toplevel_state(&self) -> Option<gtk::gdk::ToplevelState> {
        let surface = self.window.surface()?;
        Some(surface.downcast::<gtk::gdk::Toplevel>().ok()?.state())
    }

    /// Minimise or restore the window from code, for the same reason
    /// [`Self::set_fullscreen`] exists and under the same rule: the visibility
    /// signal cannot be tested by clicking the real control, because every
    /// route that would do the clicking injects at the compositor and lands on
    /// the developer's session.
    pub fn set_minimised(&self, on: bool) {
        if on {
            self.window.minimize();
        } else {
            self.window.present();
        }
    }

    /// Where the content slot sits inside the toplevel's surface, in surface
    /// coordinates: `(x, y, width, height)`.
    ///
    /// The offset matters and is not the widget's own allocation. A libadwaita
    /// window draws its drop shadow and resize border *inside* its
    /// `wl_surface`, so the content starts some way in from the surface's
    /// origin; `gtk_native_get_surface_transform` is that inset, and
    /// `wl_subsurface.set_position` is expressed in the parent's surface
    /// coordinates. Adding the two is the difference between the engine
    /// landing under the header bar and landing under the shadow.
    pub fn content_rect(&self) -> Option<(i32, i32, i32, i32)> {
        let bounds = self.content.compute_bounds(&self.window)?;
        let (dx, dy) = self.window.surface_transform();
        let w = bounds.width().round() as i32;
        let h = bounds.height().round() as i32;
        if w <= 0 || h <= 0 {
            return None;
        }
        Some(((bounds.x() as f64 + dx).round() as i32, (bounds.y() as f64 + dy).round() as i32, w, h))
    }

    /// Run whatever GTK has queued, without blocking.
    ///
    /// Bounded rather than "until nothing is pending": GTK's frame clock can
    /// keep a main context permanently ready while an animation runs, and this
    /// is called from inside the engine's own message pump, which must return.
    pub fn pump(&self) {
        let ctx = glib::MainContext::default();
        for _ in 0..32 {
            if !ctx.iteration(false) {
                break;
            }
        }
    }

    /// Iterate until the window actually exists on the compositor and has been
    /// laid out, or give up.
    ///
    /// Both conditions are needed before a subsurface can be created against
    /// it: `wl_surface` is null until GTK realises the surface, and the content
    /// allocation is zero until the first layout pass, which is what says how
    /// big the engine's surface should be.
    pub fn wait_until_mapped(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.wl_surface().is_some() && self.content_rect().is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("GTK never mapped the window (no wl_surface, or no content allocation)".into());
            }
            self.pump();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Ask GTK to repaint and therefore to commit the toplevel.
    ///
    /// `wl_subsurface.set_position` is double-buffered *on the parent*: it
    /// does nothing at all until the parent surface is committed, and GTK only
    /// commits when it has drawn. Moving the engine's surface without this
    /// leaves it at its old position until something unrelated happens to
    /// repaint the window, which reads as a stuck or torn canvas.
    /// Repaint the toplevel now, rather than on GTK's next frame.
    ///
    /// Exists for one caller: restacking the engine's subsurface. Lowering it
    /// and turning the background transparent have to reach the compositor in
    /// the same breath, and they do not by default -- a CSS class change is
    /// honoured on GTK's next frame while the restack goes out immediately, so
    /// the compositor gets "the canvas is underneath now" together with a
    /// parent buffer that is still opaque. That is one frame of the engine
    /// hidden behind GTK's background, reported as "once you press it, roblox
    /// disappears for a frame then reappears".
    ///
    /// Best effort, and it does not pretend otherwise: GTK renders on its
    /// frame clock and nothing here can force a frame the compositor has not
    /// asked for. Queuing the draw and then running the main context gives it
    /// the opportunity, which is enough in practice and is strictly better
    /// than not trying.
    pub fn repaint_now(&self) {
        self.window.queue_draw();
        self.pump();
    }

    pub fn queue_commit(&self) {
        self.window.queue_draw();
    }

    /// Fullscreen the window from code, so the configure path can be exercised
    /// without a click.
    ///
    /// **This said "TEMPORARY INSTRUMENTATION -- not for commit" for several
    /// releases while being committed**, on the one window definition ADR-011
    /// makes shared between the shell and the runtime. It is not temporary, and
    /// a doc comment lying about its own status costs more than no comment: the
    /// next person to read it either deletes something load-bearing or learns
    /// to disregard the markers that do mean it.
    ///
    /// Why it cannot be replaced by clicking the real control. Fullscreening is
    /// how `dispatch_configure` and the swapchain recreate behind it get
    /// exercised, and a test cannot press Cordial's own fullscreen button:
    /// every compositor-level injection route — `XTestFake*`, `ydotool`,
    /// `wlr-virtual-keyboard`, the RemoteDesktop portal — lands on whatever has
    /// focus, which is the developer's session, and has already hijacked their
    /// cursor once mid-session. ADR-011 is Wayland, which has no
    /// window-targeted injection to fall back on. Asking GTK directly is what
    /// remains.
    ///
    /// Reached through `android::wayland::instr_set_fullscreen`, from
    /// `looper::pump`'s `CORDIAL_SCRIPT` timeline and from the probes under
    /// `crates/cordial-runtime/examples`.
    pub fn set_fullscreen(&self, on: bool) {
        if on {
            self.window.fullscreen();
        } else {
            self.window.unfullscreen();
        }
    }
}

/// The parent surface's input region: everything except the canvas, plus the
/// editor's rectangle handed back.
///
/// Pure, and separated out so it can be tested, because the bug it had was
/// invisible from every direction. `content` and `editor` arrive in two
/// different coordinate spaces -- `content` is already in surface coordinates,
/// `editor` is in the content area's, which is what `gtk::Fixed::move_` and the
/// engine's own `NativeTextBoxInfo` both use. Unioning the editor in without
/// offsetting it drew the editor in exactly the right place and punched the
/// hole for it a header bar's height too high, so the text was visible and
/// unclickable, and nothing about either the code or the screen said why.
///
/// `surface` is the whole surface including any CSD shadow, and is widened to
/// cover the content if a configure has left it briefly smaller -- a region
/// that does not reach the canvas would clip the hole rather than the chrome.
fn input_region(
    surface: (i32, i32),
    content: (i32, i32, i32, i32),
    editor: Option<(i32, i32, i32, i32)>,
    modal: bool,
) -> gtk::cairo::Region {
    let (cx, cy, cw, ch) = content;
    let w = surface.0.max(cx + cw);
    let h = surface.1.max(cy + ch);
    let region = gtk::cairo::Region::create_rectangle(&gtk::cairo::RectangleInt::new(0, 0, w, h));
    // **A web-view dialog wants the whole window, so the hole is not punched
    // at all.** An `AdwDialog` draws inside this toplevel and is centred over
    // the canvas, so with the usual cut-out its buttons sit in the rectangle
    // that belongs to the engine -- and every click on them goes to the
    // subsurface instead of to GTK. Reported as "I cant click on the webview's
    // items".
    //
    // Not the same answer as the editor below, and the difference is the point.
    // The editor is a widget on one TextBox and the rest of the canvas is still
    // the game's, so it punches itself back in as a rectangle. A dialog is
    // modal: nothing behind it should take a click while it is up, which is
    // also exactly what `wayland::dialog_in_front` already enforces on the
    // forwarding side. Claiming the whole surface says the same thing to the
    // compositor.
    //
    // **The other way round was tried first and was much worse.** 07564e2 gave
    // the *canvas* an empty input region instead, which does not hand the click
    // to the parent -- it says no surface here wants it, and with this hole
    // already punched in the parent nothing else claimed it either, so clicks
    // fell through Cordial's window and raised whatever was behind. Reverted in
    // 73c74eb.
    if modal {
        return region;
    }
    // Subtract rather than build up from the chrome: the header bar is not the
    // only thing outside the canvas, and enumerating the rest by hand would go
    // stale the first time the layout changes.
    let _ = region.subtract_rectangle(&gtk::cairo::RectangleInt::new(cx, cy, cw, ch));
    if let Some((ex, ey, ew, eh)) = editor {
        // Into surface coordinates. See the doc comment: this `+ cx, + cy` is
        // the entire fix for "you cant click on a specific part of it to move
        // the caret".
        region
            .union_rectangle(&gtk::cairo::RectangleInt::new(cx + ex, cy + ey, ew, eh))
            .ok();
    }
    region
}

/// A first guess at the header bar's height, used only to pick the window's
/// initial size so the *content* comes out at the requested resolution. The
/// real height is read back from the widget tree once there is a layout — see
/// [`HostWindow::content_rect`] — so being a few pixels out here costs a
/// resize at startup, not a wrong canvas forever.
fn header_height_hint() -> i32 {
    47
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header bar is what makes the two coordinate spaces differ, so the
    /// numbers here are a real layout: a 1280x720 surface whose content starts
    /// 47px down, and an editor the engine placed 10px into that content.
    fn layout() -> ((i32, i32), (i32, i32, i32, i32), (i32, i32, i32, i32)) {
        ((1280, 720), (0, 47, 1280, 673), (332, 10, 592, 36))
    }

    /// fontconfig's weight scale is not Pango's, and the conversion is where
    /// that gets got wrong: Regular is 80 to fontconfig and 400 to Pango, Bold
    /// 200 and 700. Every number below is one `FcWeightToOpenType` produced
    /// from a font Roblox actually ships, read with `FcFreeTypeQuery` on
    /// 2026-08-27, so this asserts on the real inputs rather than on a ladder.
    #[test]
    fn shipped_weights_reach_the_pango_face_they_name() {
        use gtk::pango::Weight;
        // BuilderSans-Regular.otf, Arimo-Regular.ttf and 36 others.
        assert_eq!(pango_weight(400), Weight::Normal);
        // BuilderSans-Medium.otf, Montserrat-Medium.ttf.
        assert_eq!(pango_weight(500), Weight::Medium);
        // SourceSansPro-Semibold.ttf.
        assert_eq!(pango_weight(600), Weight::Semibold);
        // BuilderSans-Bold.otf, Arimo-Bold.ttf, ComicNeue-Angular-Bold.ttf.
        assert_eq!(pango_weight(700), Weight::Bold);
        // BuilderSans-ExtraBold.otf and Montserrat-Black.ttf, both of which
        // fontconfig reads as 210 and converts to 900 -- so ExtraBold arrives
        // as Heavy, which is the face that exists rather than the name.
        assert_eq!(pango_weight(900), Weight::Heavy);
    }

    /// A weight fontconfig failed to report must not become Thin. It arrives
    /// as 400 from `query_face`, and 400 has to be Normal for that fallback to
    /// mean anything.
    #[test]
    fn an_unknown_weight_falls_back_to_normal_not_thin() {
        assert_eq!(pango_weight(400), gtk::pango::Weight::Normal);
        assert_ne!(pango_weight(400), gtk::pango::Weight::Thin);
    }

    /// Roblox's three `TextXAlignment` ordinals, both ways: the value this
    /// project has actually measured (`Left`, on every box so far) must not
    /// move, and `Center`/`Right` must land on the fractions `Editable`
    /// documents -- `0.5` and `1.0` -- rather than on whatever the match arm
    /// order happens to produce.
    #[test]
    fn xalign_maps_every_ordinal_and_defaults_left() {
        assert_eq!(gtk_xalign(0), 0.0, "Left, the only value ever measured");
        assert_eq!(gtk_xalign(1), 0.5, "Center");
        assert_eq!(gtk_xalign(2), 1.0, "Right");
        // A build that renumbers the enum, or a slot this project has the
        // wrong one for, must draw left rather than somewhere arbitrary.
        assert_eq!(gtk_xalign(99), 0.0);
    }

    /// `vertical_placement`'s `Center` arm is the load-bearing one: it is the
    /// only value ever measured, and it must return `y`/`h` byte-identical to
    /// what was passed in, because that is what made the 2026-08-30 caret
    /// measurement in docs/NEXT.md come out within 0.5px with no code here at
    /// all. `Top` and `Bottom` are asserted against the arithmetic only --
    /// UNVERIFIED against a real box, as the function's own doc says.
    #[test]
    fn vertical_placement_centre_is_untouched_top_and_bottom_anchor() {
        assert_eq!(
            vertical_placement(1, 10, 22, 13),
            (10, 22),
            "Centre must pass y/h through unchanged -- this is the measured case"
        );
        assert_eq!(
            vertical_placement(0, 10, 22, 13),
            (10, 13),
            "Top anchors the natural height at the box's own top"
        );
        assert_eq!(
            vertical_placement(2, 10, 22, 13),
            (19, 13),
            "Bottom anchors it at the box's own bottom: 10 + (22 - 13)"
        );
        // An unrecognised ordinal must be as inert as Centre, not stretch or
        // shrink the widget to something nobody asked for.
        assert_eq!(vertical_placement(99, 10, 22, 13), (10, 22));
    }

    /// A natural height taller than the box (a font too big for its own line
    /// box, which is exactly what `fromRbxFontRatio` exists to prevent but
    /// should not be trusted blindly) must clamp to the box rather than draw
    /// outside it or invert the `Bottom` offset into a negative height.
    #[test]
    fn vertical_placement_clamps_a_natural_height_taller_than_the_box() {
        assert_eq!(vertical_placement(0, 10, 22, 40), (10, 22));
        assert_eq!(vertical_placement(2, 10, 22, 40), (10, 22));
    }

    /// **While a dialog is up the whole window is ours to click.**
    ///
    /// The `AdwDialog` is drawn inside this toplevel and centred over the
    /// canvas, so with the ordinary cut-out its buttons sit in the rectangle
    /// that belongs to the engine and every click on them misses GTK. This is
    /// the assertion that "I cant click on the webview's items" is fixed.
    #[test]
    fn a_dialog_takes_the_whole_window_rather_than_a_hole() {
        let (surface, content, editor) = layout();
        let region = input_region(surface, content, Some(editor), true);
        let (cx, cy, cw, ch) = content;
        // The middle of the canvas, which is where a centred dialog's buttons
        // land, and which is emphatically not clickable without this.
        assert!(region.contains_point(cx + cw / 2, cy + ch / 2));
        assert!(region.contains_point(cx, cy));
        assert!(region.contains_point(1, 1), "the chrome stays ours too");
    }

    /// And the moment it closes, the canvas goes back to the engine.
    ///
    /// The failure this catches is a modal that leaves the region claimed:
    /// every click afterwards would be swallowed by GTK and the game would
    /// stop responding to the mouse entirely, which is far worse than the bug
    /// being fixed and would look nothing like it.
    #[test]
    fn closing_a_dialog_gives_the_canvas_back() {
        let (surface, content, editor) = layout();
        let region = input_region(surface, content, Some(editor), false);
        let (cx, cy, cw, ch) = content;
        assert!(!region.contains_point(cx + cw / 2, cy + ch / 2));
    }

    #[test]
    fn the_canvas_is_not_ours_to_click() {
        let (surface, content, _) = layout();
        let r = input_region(surface, content, None, false);
        assert!(r.contains_point(10, 10), "the header bar still takes input");
        assert!(!r.contains_point(640, 400), "the canvas does not");
    }

    #[test]
    fn the_editor_is_clickable_where_it_is_drawn() {
        // The regression test for the coordinate-space bug. The editor draws at
        // content-relative (332, 10), which on screen is (332, 57). Before the
        // fix the hole was punched at (332, 10) -- inside the header bar -- and
        // every click on the visible field went past it to the engine.
        let (surface, content, editor) = layout();
        let (cx, cy, _, _) = content;
        let (ex, ey, ew, eh) = editor;
        let r = input_region(surface, content, Some(editor), false);

        for (dx, dy, what) in [(1, 1, "top left"), (ew / 2, eh / 2, "middle"), (ew - 2, eh - 2, "bottom right")] {
            assert!(
                r.contains_point(cx + ex + dx, cy + ey + dy),
                "the {what} of the editor must be clickable, at surface ({}, {})",
                cx + ex + dx,
                cy + ey + dy
            );
        }
        // **Why the bug was silent, in one assertion.** The unoffset position
        // is `(332, 10)`, which is inside the header bar -- and the header bar
        // is *supposed* to take input, so unioning the editor there changed
        // nothing observable, while the field itself stayed subtracted. There
        // was no wrong-looking region to notice: just a rectangle added where
        // one already was, and a field that quietly did not respond.
        assert!(
            r.contains_point(ex + 2, ey + 2),
            "the unoffset position is header bar, which is why adding it there was a no-op"
        );
        assert!(!r.contains_point(cx + ex - 4, cy + ey + eh / 2), "just left of the editor is canvas");
        assert!(!r.contains_point(cx + ex + ew + 4, cy + ey + eh / 2), "just right of it is canvas");
    }

    #[test]
    fn a_surface_smaller_than_its_content_still_covers_the_canvas() {
        // A configure can leave the surface briefly behind the allocation. A
        // region that stopped at the stale width would clip the subtraction and
        // leave part of the canvas taking input.
        let (_, content, _) = layout();
        let r = input_region((320, 200), content, None, false);
        assert!(!r.contains_point(1200, 700), "still excluded, not merely off the end");
        assert!(r.contains_point(10, 10));
    }

    #[test]
    fn app_id_matches_the_desktop_entry() {
        // Moved here from `cordial-runtime`'s Wayland backend, which used to
        // set the app_id itself through `xdg_toplevel.set_app_id`. GTK owns
        // the toplevel now, so this constant is what reaches the wire, and the
        // test has to live beside it or it pins nothing. ADR-009 is why the
        // two must agree.
        let desktop = include_str!("../../../packaging/io.github.luohoa97.Cordial.desktop");
        let declared = desktop
            .lines()
            .find_map(|l| l.strip_prefix("StartupWMClass="))
            .expect("desktop entry declares StartupWMClass");
        assert_eq!(declared.trim(), APP_ID);
    }

    #[test]
    fn the_title_does_not_name_a_graphics_backend() {
        // It used to say "(OpenGL ES)" and the engine renders through Vulkan,
        // measured over three runs. A title bar that reports the wrong backend
        // is worse than one that reports none, and this is here so that a
        // future suffix has to be justified rather than pasted back.
        let t = title();
        // Whichever face today wears -- the version has to follow the name
        // either way, and on two days a year the name is not "Cordial".
        let name = crate::branding::current().name();
        assert!(t.starts_with(&format!("{name} ")), "{t}");
        for backend in ["OpenGL", "GLES", "Vulkan"] {
            assert!(!t.contains(backend), "{t} names a graphics backend");
        }
    }

    #[test]
    fn a_window_is_clamped_to_the_monitor_it_opens_on() {
        // The reported bug, in numbers: a 3440x1440 monitor with a 1920x1200
        // one beside it, a 5360x1440 union, and a window sized against nothing
        // at all. Whatever is asked for, the result has to fit the screen it
        // lands on, not the sum of the screens.
        let (w, h) = fit_within((5360, 1440), (1920, 1200));
        assert!(w <= 1920 && h <= 1200, "{w}x{h}");
        let (w, h) = fit_within((3440, 1440), (3440, 1440));
        assert!(w <= 3440 && h <= 1440, "{w}x{h}");
    }

    #[test]
    fn a_window_that_already_fits_is_left_exactly_as_asked() {
        // The clamp must not become a resize of every window. 1280x720 plus a
        // header bar is the runtime's default and has to survive untouched, or
        // the engine renders at a resolution nobody asked for.
        assert_eq!(fit_within((1280, 767), (3440, 1440)), (1280, 767));
    }

    #[test]
    fn a_monitor_smaller_than_the_allowance_still_yields_a_usable_size() {
        // Subtracting the panel allowance from a tiny monitor would otherwise
        // produce a zero or negative default size, which GTK reads as "no
        // default size" rather than as an error — a silently ignored clamp.
        let (w, h) = fit_within((1280, 767), (64, 48));
        assert!(w > 0 && h > 0, "{w}x{h}");
    }
}
