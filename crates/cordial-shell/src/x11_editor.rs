//! The text editor on the X11 backend: a small GTK window laid over the
//! focused Roblox TextBox, holding a real `gtk::Text` that owns the text.
//!
//! [ADR-062](../../../docs/adr/ADR-062-the-x11-editor-is-an-override-redirect-window.md)
//! is the decision and its alternatives. The shape of it:
//!
//! * The engine's window is a bare Xlib one, created before GTK exists
//!   (`cordial-runtime`'s `android/window.rs`). It stays that. Nothing here
//!   reparents it, which is what ADR-024 priced as the expensive way and what
//!   would have moved X keyboard focus off the one connection that reads the
//!   game's keys.
//! * The editor is a *separate* GTK toplevel made **override-redirect** after
//!   it is realised, positioned over the box with `XMoveResizeWindow` and
//!   given keyboard focus with `XSetInputFocus`. Override-redirect because
//!   a window manager would otherwise place, decorate, animate and decide the
//!   focus of it. An undecorated transient window did take focus and the
//!   position asked for under openbox, xfwm4 and icewm in a prototype, so this
//!   is not because managers cannot do it; it is that a managed window leaves
//!   the answer to each manager, and the ones not tried (tiling, compositing
//!   desktops) are where it differs. Override-redirect takes the question
//!   away. Measured under openbox, icewm and xfwm4 (with and without its
//!   compositor) on a private Xvfb, and under sway's Xwayland; not under KWin,
//!   Mutter, a tiling manager or gamescope.
//! * A window of its own means a window nobody composites for us. On a
//!   compositing desktop the editor is ARGB and transparent over the box, as
//!   on Wayland. On a bare X server a transparent window is painted *black*
//!   (measured), so there it is opaque, in a colour read from the pixels it is
//!   about to cover or, failing that, chosen to contrast with the text.
//!
//! **Keyboard focus is the part that is not obvious.** While a box is focused
//! the editor holds X input focus, so the engine's own connection hears no
//! keys at all, which is what makes "the widget owns the text" true here and
//! why `dispatch_key` has nothing to guard against. Three things move focus
//! away from it and each is handled in [`X11Editor::tick`]: a focus-follows-
//! mouse window manager handing it back to the engine when the pointer crosses
//! the edge (taken straight back), the user switching to another application
//! (the editor hides, or it would float over that application), and the engine
//! window being unmapped.
//!
//! Every Xlib symbol is looked up with `dlsym` against the process, never
//! linked: the library half of this crate is built on hosts with no X headers
//! and must still link, and these are symbols GTK already pulled in.

use crate::editor::{EditorCore, EDITOR_SHEET};
use crate::host_window::TextOverlay;
use libadwaita::glib;
use libadwaita::glib::translate::IntoGlib;
use libadwaita::gtk;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::ffi::{c_int, c_long, c_uint, c_ulong, c_void, CStr};

type Display = *mut c_void;
type Window = c_ulong;

/// What the runtime does with a key the editor receives that the *engine*
/// should also hear (Escape, F11, the recording hotkey): `(keyval, keycode,
/// state, down)`, returning whether it consumed the key.
pub type KeyForward = Box<dyn Fn(u32, u32, u32, bool) -> bool>;

const NONE_WINDOW: Window = 0;
const POINTER_ROOT: Window = 1;
const REVERT_TO_PARENT: c_int = 2;
const CURRENT_TIME: c_ulong = 0;
const CW_OVERRIDE_REDIRECT: c_ulong = 1 << 9;
const IS_VIEWABLE: c_int = 2;
const ALL_PLANES: c_ulong = !0;
const Z_PIXMAP: c_int = 2;

/// Xlib's `XSetWindowAttributes`.
#[repr(C)]
#[derive(Default)]
struct XSetWindowAttributes {
    background_pixmap: c_ulong,
    background_pixel: c_ulong,
    border_pixmap: c_ulong,
    border_pixel: c_ulong,
    bit_gravity: c_int,
    win_gravity: c_int,
    backing_store: c_int,
    backing_planes: c_ulong,
    backing_pixel: c_ulong,
    save_under: c_int,
    event_mask: c_long,
    do_not_propagate_mask: c_long,
    override_redirect: c_int,
    colormap: c_ulong,
    cursor: c_ulong,
}

/// Xlib's `XWindowAttributes`; only `map_state` is read.
#[repr(C)]
struct XWindowAttributes {
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    border_width: c_int,
    depth: c_int,
    visual: *mut c_void,
    root: c_ulong,
    class: c_int,
    bit_gravity: c_int,
    win_gravity: c_int,
    backing_store: c_int,
    backing_planes: c_ulong,
    backing_pixel: c_ulong,
    save_under: c_int,
    colormap: c_ulong,
    map_installed: c_int,
    map_state: c_int,
    all_event_masks: c_long,
    your_event_mask: c_long,
    do_not_propagate_mask: c_long,
    override_redirect: c_int,
    screen: *mut c_void,
}

/// The Xlib and GDK entry points, resolved once.
struct X {
    move_resize: unsafe extern "C" fn(Display, Window, c_int, c_int, c_uint, c_uint) -> c_int,
    raise: unsafe extern "C" fn(Display, Window) -> c_int,
    set_input_focus: unsafe extern "C" fn(Display, Window, c_int, c_ulong) -> c_int,
    get_input_focus: unsafe extern "C" fn(Display, *mut Window, *mut c_int) -> c_int,
    translate: unsafe extern "C" fn(Display, Window, Window, c_int, c_int, *mut c_int, *mut c_int, *mut Window) -> c_int,
    change_attributes: unsafe extern "C" fn(Display, Window, c_ulong, *mut XSetWindowAttributes) -> c_int,
    get_attributes: unsafe extern "C" fn(Display, Window, *mut XWindowAttributes) -> c_int,
    query_tree: unsafe extern "C" fn(Display, Window, *mut Window, *mut Window, *mut *mut Window, *mut c_uint) -> c_int,
    free: unsafe extern "C" fn(*mut c_void) -> c_int,
    get_image: unsafe extern "C" fn(Display, c_ulong, c_int, c_int, c_uint, c_uint, c_ulong, c_int) -> *mut c_void,
    get_pixel: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_ulong,
    destroy_image: unsafe extern "C" fn(*mut c_void) -> c_int,
    flush: unsafe extern "C" fn(Display) -> c_int,
    default_root: unsafe extern "C" fn(Display) -> Window,
    connection_number: unsafe extern "C" fn(Display) -> c_int,
    surface_xid: unsafe extern "C" fn(*mut c_void) -> c_ulong,
    display_xdisplay: unsafe extern "C" fn(*mut c_void) -> Display,
    trap_push: unsafe extern "C" fn(*mut c_void),
    trap_pop: unsafe extern "C" fn(*mut c_void) -> c_int,
}

fn sym(name: &CStr) -> Result<*mut c_void, String> {
    // SAFETY: `RTLD_DEFAULT` with a NUL-terminated name is the documented form.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    if p.is_null() {
        Err(format!("{} is not in this process", name.to_string_lossy()))
    } else {
        Ok(p)
    }
}

/// One symbol, as the function-pointer type the caller names.
fn load<T: Copy>(name: &CStr) -> Result<T, String> {
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    let p = sym(name)?;
    // SAFETY: `T` is an `unsafe extern "C" fn` type spelled with the symbol's own
    // signature in `X` above, which is the same size as a data pointer on every
    // platform Linux Cordial builds for (asserted just above).
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
}

impl X {
    fn load() -> Result<Self, String> {
        Ok(Self {
            move_resize: load(c"XMoveResizeWindow")?,
            raise: load(c"XRaiseWindow")?,
            set_input_focus: load(c"XSetInputFocus")?,
            get_input_focus: load(c"XGetInputFocus")?,
            translate: load(c"XTranslateCoordinates")?,
            change_attributes: load(c"XChangeWindowAttributes")?,
            get_attributes: load(c"XGetWindowAttributes")?,
            query_tree: load(c"XQueryTree")?,
            free: load(c"XFree")?,
            get_image: load(c"XGetImage")?,
            get_pixel: load(c"XGetPixel")?,
            destroy_image: load(c"XDestroyImage")?,
            flush: load(c"XFlush")?,
            default_root: load(c"XDefaultRootWindow")?,
            connection_number: load(c"XConnectionNumber")?,
            surface_xid: load(c"gdk_x11_surface_get_xid")?,
            display_xdisplay: load(c"gdk_x11_display_get_xdisplay")?,
            trap_push: load(c"gdk_x11_display_error_trap_push")?,
            trap_pop: load(c"gdk_x11_display_error_trap_pop")?,
        })
    }
}

/// Bring GTK up on the X11 backend, for a process whose engine window is a bare
/// Xlib one.
///
/// Both halves of the backend choice, for the reason `host_window::init_wayland`
/// spells out: `GDK_BACKEND` and the allowed-backends call are two filters and
/// their intersection can be empty, which is a GTK that opens no display and
/// says nothing. A session that exports `GDK_BACKEND=wayland` and runs Cordial
/// with `CORDIAL_X11=1` is exactly that.
///
/// `GSK_RENDERER=cairo` unless the user chose one: the editor is a few hundred
/// pixels of text, a GL renderer would open a second EGL context beside the
/// engine's for it (ADR-047 is the history of how that goes), and cairo is
/// what worked on every X server this was run against.
pub fn init() -> Result<(), String> {
    if gtk::is_initialized() {
        return Ok(());
    }
    // SAFETY: before any GTK or GDK call, on the thread that is about to
    // initialise it; nothing in the engine reads these.
    unsafe {
        if std::env::var("GDK_BACKEND").is_ok_and(|v| v != "x11") {
            glib::setenv("GDK_BACKEND", "x11", true).map_err(|e| format!("could not force GDK_BACKEND=x11: {e}"))?;
        }
        if std::env::var_os("GSK_RENDERER").is_none() {
            let _ = glib::setenv("GSK_RENDERER", "cairo", false);
        }
    }
    gtk::gdk::set_allowed_backends("x11");
    // See `host_window::adopt_user_locale_keeping_c_numbers`.
    gtk::disable_setlocale();
    gtk::init().map_err(|e| format!("GTK would not initialise on X11: {e}"))?;
    let display = gtk::gdk::Display::default().ok_or("GTK has no default display")?;
    if display.type_().name() != "GdkX11Display" {
        return Err(format!("GTK opened a {} where an X11 display was needed", display.type_().name()));
    }
    Ok(())
}

/// What the runtime last asked the editor to show.
struct Wanted {
    /// The rectangle the editor occupies, in the engine window's pixels.
    rect: (i32, i32, i32, i32),
    text_color: u32,
}

struct State {
    wanted: Option<Wanted>,
    /// Whether the window is mapped, as far as this struct asked it to be.
    shown: bool,
    /// Hidden because focus went to another application, or the engine window
    /// is not viewable. Cleared once the engine has focus again.
    suspended: bool,
    /// The X window GTK made, and whether it has been made override-redirect.
    xid: Window,
    or_applied_to: Window,
    /// The root-relative rectangle last sent to the server.
    placed: Option<(i32, i32, i32, i32)>,
    /// An opaque background for a server with no compositor: sampled once per
    /// box, because later reads would see the editor's own pixels.
    background: Option<u32>,
    /// Number of times X focus was taken back from the engine, for the log.
    refocused: u32,
}

pub struct X11Editor {
    window: gtk::Window,
    core: EditorCore,
    x: X,
    display: Display,
    gdk_display: gtk::gdk::Display,
    engine: Window,
    composited: bool,
    background: gtk::CssProvider,
    st: RefCell<State>,
    key_forward: std::rc::Rc<RefCell<Option<KeyForward>>>,
    scale: Cell<i32>,
}

impl X11Editor {
    /// Build the (hidden) editor for the engine window `engine`. [`init`] must
    /// have succeeded on this thread first.
    pub fn new(engine: c_ulong) -> Result<Self, String> {
        let x = X::load()?;
        let gdk_display = gtk::gdk::Display::default().ok_or("no GDK display")?;
        // SAFETY: a live `GdkDisplay` pointer and GDK's own accessor for it.
        let display = unsafe { (x.display_xdisplay)(gdk_display.as_ptr() as *mut c_void) };
        if display.is_null() {
            return Err("GDK's display has no Xlib Display".into());
        }
        let core = EditorCore::new();
        let composited = gdk_display.is_composited() && gdk_display.is_rgba();
        gtk::style_context_add_provider_for_display(&gdk_display, &sheet_provider(), gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        gtk::style_context_add_provider_for_display(
            &gdk_display,
            core.colour_provider(),
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        let background = gtk::CssProvider::new();
        background.load_from_string(&window_sheet(None));
        gtk::style_context_add_provider_for_display(&gdk_display, &background, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);

        let window = gtk::Window::builder().decorated(false).title("Cordial text editor").build();
        window.add_css_class("cordial-x11-editor");
        window.set_child(Some(core.widget()));
        window.set_default_size(1, 1);

        let key_forward: std::rc::Rc<RefCell<Option<KeyForward>>> = std::rc::Rc::new(RefCell::new(None));
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let sink = key_forward.clone();
            keys.connect_key_pressed(move |_, keyval, keycode, state| {
                forward(&sink, keyval.into_glib(), keycode, state.bits(), true)
            });
            let sink = key_forward.clone();
            keys.connect_key_released(move |_, keyval, keycode, state| {
                let _ = forward(&sink, keyval.into_glib(), keycode, state.bits(), false);
            });
        }
        window.add_controller(keys);

        let editor = Self {
            window,
            core,
            x,
            display,
            gdk_display,
            engine,
            composited,
            background,
            st: RefCell::new(State {
                wanted: None,
                shown: false,
                suspended: false,
                xid: 0,
                or_applied_to: 0,
                placed: None,
                background: None,
                refocused: 0,
            }),
            key_forward,
            scale: Cell::new(1),
        };
        eprintln!(
            "[x11-editor] ready: compositing {} ({}), engine window {engine:#x}",
            if composited { "on" } else { "off" },
            if composited { "the editor is transparent over the box" } else { "the editor is opaque over the box" },
        );
        Ok(editor)
    }

    pub fn set_font_family(&self, family: Option<String>) {
        self.core.set_font_family(family);
    }

    pub fn connect_changed<F: Fn(&str, i32) + 'static>(&self, f: F) {
        self.core.connect_changed(f);
    }

    pub fn connect_activate<F: Fn() + 'static>(&self, f: F) {
        self.core.connect_activate(f);
    }

    /// Install the callback for keys the engine must also hear.
    pub fn connect_key_forward(&self, f: KeyForward) {
        *self.key_forward.borrow_mut() = Some(f);
    }

    pub fn paste_at_caret(&self, text: &str) -> usize {
        if self.core.multiline_active() {
            self.core.multiline_paste_at_caret(text)
        } else {
            self.core.paste_at_caret(text)
        }
    }

    pub fn replace_text(&self, text: &str) -> usize {
        if self.core.multiline_active() {
            self.core.multiline_replace_text(text)
        } else {
            self.core.replace_text(text)
        }
    }

    /// The Xlib connection GDK holds, for the runtime's looper to watch so a
    /// keystroke does not wait for the next 50 ms pump.
    pub fn connection_fd(&self) -> c_int {
        // SAFETY: `display` is GDK's live Display.
        unsafe { (self.x.connection_number)(self.display) }
    }

    /// Whether the editor is on screen and holds the keyboard: the condition
    /// under which the engine's own key path must leave the text alone.
    pub fn owns_keyboard(&self) -> bool {
        let st = self.st.borrow();
        st.shown && st.xid != 0 && self.focus_holder() == st.xid
    }

    /// Show, move or hide the editor for the focused box. `None` is a box that
    /// lost focus.
    pub fn set_text_overlay(&self, overlay: Option<TextOverlay<'_>>) {
        let Some(overlay) = overlay else {
            self.hide(false);
            self.core.clear();
            self.st.borrow_mut().wanted = None;
            return;
        };
        let scale = self.window.scale_factor().max(1);
        self.scale.set(scale);
        self.core.set_scale(scale);
        let rect = self.core.apply(&overlay);
        self.st.borrow_mut().wanted = Some(Wanted { rect, text_color: overlay.text_color });
        self.place();
    }

    /// Run GTK's pending work and keep the window, its stacking and its focus
    /// where they should be. Called every pump.
    pub fn tick(&self) {
        let ctx = glib::MainContext::default();
        for _ in 0..32 {
            if !ctx.iteration(false) {
                break;
            }
        }
        if self.st.borrow().wanted.is_some() {
            self.place();
        }
    }

    // ------------------------------------------------------------ internals

    fn trapped<T>(&self, f: impl FnOnce() -> T) -> T {
        let dpy = self.gdk_display.as_ptr() as *mut c_void;
        // SAFETY: GDK's documented pair; the trap keeps an error from a window
        // that vanished mid-call from reaching the default handler, which exits.
        unsafe { (self.x.trap_push)(dpy) };
        let r = f();
        // SAFETY: Xlib/GDK call on GDK's own live display, from the thread that owns it; the window ids are ones this struct created or was given.
        unsafe { (self.x.trap_pop)(dpy) };
        r
    }

    fn map_state(&self, w: Window) -> c_int {
        self.trapped(|| {
            let mut a = std::mem::MaybeUninit::<XWindowAttributes>::zeroed();
            // SAFETY: `a` is a zeroed attributes struct of Xlib's layout.
            let ok = unsafe { (self.x.get_attributes)(self.display, w, a.as_mut_ptr()) };
            if ok == 0 {
                return 0;
            }
            // SAFETY: Xlib/GDK call on GDK's own live display, from the thread that owns it; the window ids are ones this struct created or was given.
            unsafe { a.assume_init() }.map_state
        })
    }

    fn focus_holder(&self) -> Window {
        self.trapped(|| {
            let mut w: Window = 0;
            let mut revert: c_int = 0;
            // SAFETY: valid out-pointers.
            unsafe { (self.x.get_input_focus)(self.display, &mut w, &mut revert) };
            w
        })
    }

    /// The engine window's top-left in root coordinates, or `None` if it is
    /// gone or not viewable.
    fn engine_origin(&self) -> Option<(i32, i32)> {
        if self.map_state(self.engine) != IS_VIEWABLE {
            return None;
        }
        self.trapped(|| {
            let (mut rx, mut ry, mut child) = (0, 0, 0);
            // SAFETY: valid out-pointers; the root is the display's own.
            let ok = unsafe {
                let root = (self.x.default_root)(self.display);
                (self.x.translate)(self.display, self.engine, root, 0, 0, &mut rx, &mut ry, &mut child)
            };
            (ok != 0).then_some((rx, ry))
        })
    }

    fn take_focus(&self, xid: Window) {
        self.trapped(|| {
            // SAFETY: the window is viewable, checked by the caller.
            unsafe {
                (self.x.set_input_focus)(self.display, xid, REVERT_TO_PARENT, CURRENT_TIME);
                (self.x.flush)(self.display);
            }
        });
    }

    /// Make GTK's window override-redirect, once per X window it creates.
    ///
    /// Done between `realize` and the first map: attributes of a mapped window
    /// cannot be changed to this, and a window the window manager has already
    /// seen is its to manage.
    fn ensure_override_redirect(&self) -> Option<Window> {
        WidgetExt::realize(&self.window);
        let surface = self.window.surface()?;
        // SAFETY: a live GdkSurface on an X11 display.
        let xid = unsafe { (self.x.surface_xid)(surface.as_ptr() as *mut c_void) };
        if xid == 0 {
            return None;
        }
        let mut st = self.st.borrow_mut();
        st.xid = xid;
        if st.or_applied_to != xid {
            let mut attrs = XSetWindowAttributes { override_redirect: 1, ..Default::default() };
            self.trapped(|| {
                // SAFETY: `attrs` is Xlib's struct and the window exists.
                unsafe {
                    (self.x.change_attributes)(self.display, xid, CW_OVERRIDE_REDIRECT, &mut attrs);
                    (self.x.flush)(self.display);
                }
            });
            st.or_applied_to = xid;
        }
        Some(xid)
    }

    /// Read the pixel the editor is about to cover, for a server with nothing
    /// to composite it with. `None` if the read fails, which on some drivers
    /// it does for a window that is presented through DRI3.
    fn sample_background(&self, x: i32, y: i32) -> Option<u32> {
        self.trapped(|| {
            // SAFETY: a 1x1 read inside the engine window, before the editor covers it.
            unsafe {
                let img = (self.x.get_image)(self.display, self.engine, x, y, 1, 1, ALL_PLANES, Z_PIXMAP);
                if img.is_null() {
                    return None;
                }
                let px = (self.x.get_pixel)(img, 0, 0) as u32;
                (self.x.destroy_image)(img);
                Some(px & 0x00ff_ffff)
            }
        })
    }

    /// Put the window where the box is, showing it first if it is not up.
    fn place(&self) {
        let (rect, text_color) = match self.st.borrow().wanted.as_ref() {
            Some(w) => (w.rect, w.text_color),
            None => return,
        };
        let Some((ox, oy)) = self.engine_origin() else {
            // Unmapped or minimised engine: nothing to sit on, and an
            // override-redirect window would stay put over whatever is there.
            self.suspend();
            return;
        };
        let holder = self.focus_holder();
        {
            let mut st = self.st.borrow_mut();
            if st.suspended {
                if holder == self.engine {
                    st.suspended = false;
                } else {
                    return;
                }
            }
        }

        let (x, y, w, h) = rect;
        let root_rect = (ox + x, oy + y, w.max(1), h.max(1));
        let shown = self.st.borrow().shown;
        if !shown {
            if !self.composited && self.st.borrow().background.is_none() {
                let sampled = self.sample_background(x + (w / 2).min(3), y + h / 2);
                let colour = opaque_background(text_color, sampled);
                self.st.borrow_mut().background = Some(colour);
                self.background.load_from_string(&window_sheet(Some(colour)));
            }
            let Some(xid) = self.ensure_override_redirect() else { return };
            let s = self.scale.get();
            self.window.set_default_size((root_rect.2 / s).max(1), (root_rect.3 / s).max(1));
            self.move_resize(xid, root_rect);
            self.window.present();
            self.st.borrow_mut().shown = true;
        }
        let xid = self.st.borrow().xid;
        if self.st.borrow().placed != Some(root_rect) {
            self.move_resize(xid, root_rect);
        }

        // Stacking: a window manager restacks the engine window on fullscreen
        // and on focus, and an override-redirect window it never heard of ends
        // up beneath it. Raise only when something is above us.
        if !self.is_topmost(xid) {
            // SAFETY: Xlib/GDK call on GDK's own live display, from the thread that owns it; the window ids are ones this struct created or was given.
            self.trapped(|| unsafe {
                (self.x.raise)(self.display, xid);
                (self.x.flush)(self.display);
            });
        }

        // Focus, once the server says the window is mapped (an XSetInputFocus
        // on an unviewable window is a BadMatch).
        if self.map_state(xid) == IS_VIEWABLE && holder != xid {
            if holder == self.engine || holder == NONE_WINDOW || holder == POINTER_ROOT {
                self.take_focus(xid);
                let mut st = self.st.borrow_mut();
                st.refocused += 1;
                if st.refocused <= 3 {
                    eprintln!("[x11-editor] took keyboard focus from {holder:#x} for the box");
                }
            } else {
                // Somebody else's window has focus: the user left. Hide rather
                // than float over it.
                self.suspend();
            }
        }
    }

    fn move_resize(&self, xid: Window, r: (i32, i32, i32, i32)) {
        // SAFETY: Xlib/GDK call on GDK's own live display, from the thread that owns it; the window ids are ones this struct created or was given.
        self.trapped(|| unsafe {
            (self.x.move_resize)(self.display, xid, r.0, r.1, r.2 as c_uint, r.3 as c_uint);
            (self.x.flush)(self.display);
        });
        self.st.borrow_mut().placed = Some(r);
    }

    fn is_topmost(&self, xid: Window) -> bool {
        self.trapped(|| {
            let (mut root, mut parent) = (0, 0);
            let mut children: *mut Window = std::ptr::null_mut();
            let mut n: c_uint = 0;
            // SAFETY: valid out-pointers; the array is freed with XFree.
            unsafe {
                let r = (self.x.default_root)(self.display);
                if (self.x.query_tree)(self.display, r, &mut root, &mut parent, &mut children, &mut n) == 0 {
                    return true;
                }
                let top = if n > 0 { *children.add(n as usize - 1) } else { 0 };
                if !children.is_null() {
                    (self.x.free)(children as *mut c_void);
                }
                top == xid
            }
        })
    }

    /// Hide because the user went elsewhere, and come back on its own when the
    /// engine window has focus again.
    fn suspend(&self) {
        let was = {
            let mut st = self.st.borrow_mut();
            let was = st.shown;
            st.suspended = true;
            was
        };
        if was {
            eprintln!("[x11-editor] hidden: focus is not on the game window");
            self.hide(true);
        }
    }

    fn hide(&self, keep_wanted: bool) {
        let (shown, xid) = {
            let st = self.st.borrow();
            (st.shown, st.xid)
        };
        if shown {
            // Hand focus back first, and only if we hold it: a box that blurs
            // while another application has focus must not steal it.
            if self.focus_holder() == xid && self.map_state(self.engine) == IS_VIEWABLE {
                self.take_focus(self.engine);
            }
            self.window.set_visible(false);
        }
        let mut st = self.st.borrow_mut();
        st.shown = false;
        st.placed = None;
        if !keep_wanted {
            st.suspended = false;
            st.background = None;
        }
    }
}

fn forward(sink: &std::rc::Rc<RefCell<Option<KeyForward>>>, keyval: u32, keycode: u32, state: u32, down: bool) -> glib::Propagation {
    match sink.borrow().as_ref() {
        Some(cb) if cb(keyval, keycode, state, down) => glib::Propagation::Stop,
        _ => glib::Propagation::Proceed,
    }
}

fn sheet_provider() -> gtk::CssProvider {
    let p = gtk::CssProvider::new();
    p.load_from_string(EDITOR_SHEET);
    p
}

/// The window's own background. Transparent where something composites it;
/// where nothing does, a transparent window is painted black (measured on a
/// bare Xvfb), which is invisible text on a light box.
fn window_sheet(opaque: Option<u32>) -> String {
    match opaque {
        None => ".cordial-x11-editor, .cordial-x11-editor.background { background: transparent; box-shadow: none; }".into(),
        Some(rgb) => format!(
            ".cordial-x11-editor, .cordial-x11-editor.background {{ background: #{rgb:06x}; box-shadow: none; }}"
        ),
    }
}

/// An opaque colour for the editor when nothing composites it.
///
/// The pixel the box showed if one could be read and it contrasts with the
/// text; otherwise whichever of near-black and near-white the text is not.
/// `INFERRED` as a match for the box: it is right where the box is one flat
/// colour, which Roblox's own fields are, and a gradient or an image behind
/// the field would show as a flat patch.
pub fn opaque_background(text_color: u32, sampled: Option<u32>) -> u32 {
    let text = luma(text_color);
    if let Some(s) = sampled {
        if (luma(s) - text).abs() >= 0.25 {
            return s;
        }
    }
    if text > 0.5 {
        0x232527
    } else {
        0xf7f7f8
    }
}

fn luma(rgb: u32) -> f32 {
    let (r, g, b) = (((rgb >> 16) & 0xff) as f32, ((rgb >> 8) & 0xff) as f32, (rgb & 0xff) as f32);
    (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sampled_box_colour_is_used_when_the_text_contrasts_with_it() {
        assert_eq!(opaque_background(0xd5d5dd, Some(0x393b3d)), 0x393b3d);
        assert_eq!(opaque_background(0x191919, Some(0xffffff)), 0xffffff);
    }

    /// A pixel that is the text's own colour is the glyph edge or a border, not
    /// the box: using it would make the text vanish.
    #[test]
    fn a_sample_that_does_not_contrast_with_the_text_is_ignored() {
        assert_eq!(opaque_background(0xd5d5dd, Some(0xe0e0e0)), 0x232527);
        assert_eq!(opaque_background(0x191919, Some(0x202020)), 0xf7f7f8);
    }

    #[test]
    fn with_no_sample_the_background_is_chosen_against_the_text() {
        assert_eq!(opaque_background(0xffffff, None), 0x232527, "light text gets a dark field");
        assert_eq!(opaque_background(0x000000, None), 0xf7f7f8, "dark text gets a light one");
    }

    #[test]
    fn the_window_sheet_is_transparent_only_when_nothing_is_asked_to_be_opaque() {
        assert!(window_sheet(None).contains("transparent"));
        assert!(window_sheet(Some(0x232527)).contains("#232527"));
    }
}
