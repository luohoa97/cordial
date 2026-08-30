//! A host window, and `ANativeWindow_*` over it.
//!
//! Android hands the engine an `ANativeWindow` and it renders into that. On
//! Linux the equivalent is a window-system surface, so this creates one and
//! implements the ten `ANativeWindow_*` entry points Roblox imports against it.
//!
//! X11 is loaded with `dlopen` rather than linked. Cordial has to run its loader
//! and asset tests on machines with no display at all — CI, containers, a remote
//! shell — and a link-time dependency would make the whole binary refuse to
//! start there. Loading late means "no window" is a runtime condition the caller
//! can handle, which is what it actually is.
//!
//! Wayland is the better long-term target and Roblox's Android build has no
//! opinion either way. X11 first because `eglCreateWindowSurface` takes an
//! `xcb_window_t`/`Window` directly, whereas Wayland needs an `wl_egl_window`
//! and a surface role — more moving parts for the same first frame.

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Android pixel formats, from `android/native_window.h`.
pub const WINDOW_FORMAT_RGBA_8888: i32 = 1;

/// KeyPressMask | KeyReleaseMask | ButtonPressMask | ButtonReleaseMask |
/// PointerMotionMask | ExposureMask | StructureNotifyMask | FocusChangeMask,
/// from X.h.
/// ExposureMask is what makes a damaged window (uncovered, restored,
/// redirected through a compositor) generate `Expose`, which
/// `pump_input_events` turns into `onSurfaceRedrawNeededNative` — without it
/// the window never asked to be told, and a damaged window just stayed
/// damaged until the engine's own next frame.
///
/// Module-level rather than local to `open()` so the redraw wiring can be
/// checked (`EXPOSURE_MASK` bit present) without a live X server.
/// StructureNotifyMask (0x20000) is included so `ConfigureNotify` arrives when
/// the window is resized. Without it Cordial never learned its own window had
/// changed size: the engine kept rendering at the size it was told at startup
/// while X cleared the window to its background colour, which is the black
/// flash on every resize.
const INPUT_EVENT_MASK: c_long =
    0x1 | 0x2 | 0x4 | 0x8 | 0x40 | 0x8000 | 0x20000 | 0x200000;

type Display = *mut c_void;
type Window = c_ulong;

struct Xlib {
    open_display: unsafe extern "C" fn(*const c_char) -> Display,
    default_root_window: unsafe extern "C" fn(Display) -> Window,
    create_simple_window: unsafe extern "C" fn(
        Display, Window, c_int, c_int, u32, u32, u32, c_ulong, c_ulong,
    ) -> Window,
    map_window: unsafe extern "C" fn(Display, Window) -> c_int,
    set_wm_normal_hints: unsafe extern "C" fn(Display, Window, *mut XSizeHints),
    set_class_hint: unsafe extern "C" fn(Display, Window, *mut XClassHint) -> c_int,
    set_wm_hints: unsafe extern "C" fn(Display, Window, *mut XWMHints) -> c_int,
    move_window: unsafe extern "C" fn(Display, Window, c_int, c_int) -> c_int,
    intern_atom: unsafe extern "C" fn(Display, *const c_char, c_int) -> c_ulong,
    send_event: unsafe extern "C" fn(Display, Window, c_int, c_long, *mut c_void) -> c_int,
    sync: unsafe extern "C" fn(Display, c_int) -> c_int,
    store_name: unsafe extern "C" fn(Display, Window, *const c_char) -> c_int,
    flush: unsafe extern "C" fn(Display) -> c_int,
    destroy_window: unsafe extern "C" fn(Display, Window) -> c_int,
    // ---- input, added for keyboard/mouse delivery ----
    select_input: unsafe extern "C" fn(Display, Window, c_long),
    connection_number: unsafe extern "C" fn(Display) -> c_int,
    pending: unsafe extern "C" fn(Display) -> c_int,
    next_event: unsafe extern "C" fn(Display, *mut c_void) -> c_int,

    grab_pointer: unsafe extern "C" fn(
        Display, Window, c_int, c_uint, c_int, c_int, Window, c_ulong, c_ulong,
    ) -> c_int,
    ungrab_pointer: unsafe extern "C" fn(Display, c_ulong) -> c_int,
    warp_pointer: unsafe extern "C" fn(
        Display, Window, Window, c_int, c_int, c_uint, c_uint, c_int, c_int,
    ) -> c_int,
    query_pointer: unsafe extern "C" fn(
        Display, Window,
        *mut Window, *mut Window,
        *mut c_int, *mut c_int,
        *mut c_int, *mut c_int,
        *mut c_uint,
    ) -> c_int,

    /// `XLookupString` doubles as the keysym lookup and the ASCII/Latin-1 text
    /// lookup, and — unlike `XKeycodeToKeysym` — takes the event's `state` into
    /// account, so Shift and the rest of the modifier state do not have to be
    /// reimplemented by hand.
    lookup_string:
        unsafe extern "C" fn(*mut c_void, *mut c_char, c_int, *mut c_ulong, *mut c_void) -> c_int,
    // ---- cursor, so the host pointer does not double the engine's own ----
    create_bitmap_from_data:
        unsafe extern "C" fn(Display, Window, *const c_char, c_uint, c_uint) -> c_ulong,
    create_pixmap_cursor: unsafe extern "C" fn(
        Display, c_ulong, c_ulong, *mut XColor, *mut XColor, c_uint, c_uint,
    ) -> c_ulong,
    define_cursor: unsafe extern "C" fn(Display, Window, c_ulong) -> c_int,
    free_pixmap: unsafe extern "C" fn(Display, c_ulong) -> c_int,
}

/// `XColor`. Only the pixel/RGB prefix is read by `XCreatePixmapCursor`, but the
/// whole struct has to be the right size because Xlib writes through the pointer.
#[repr(C)]
struct XColor {
    pixel: c_ulong,
    red: u16,
    green: u16,
    blue: u16,
    flags: c_char,
    pad: c_char,
}

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 2;

impl Xlib {
    fn load() -> Result<Self, String> {
        // SAFETY: a literal soname; the handle is never closed.
        let lib = unsafe { dlopen(c"libX11.so.6".as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            return Err("libX11.so.6 is not available".into());
        }
        macro_rules! sym {
            ($name:literal) => {{
                let name = CString::new($name).unwrap();
                // SAFETY: the handle is open and the names are Xlib's documented
                // exports, so the signatures are the ones declared above.
                let p = unsafe { dlsym(lib, name.as_ptr()) };
                if p.is_null() {
                    return Err(format!("libX11 has no {}", $name));
                }
                unsafe { std::mem::transmute(p) }
            }};
        }
        Ok(Xlib {
            open_display: sym!("XOpenDisplay"),
            default_root_window: sym!("XDefaultRootWindow"),
            create_simple_window: sym!("XCreateSimpleWindow"),
            map_window: sym!("XMapWindow"),
            store_name: sym!("XStoreName"),
            flush: sym!("XFlush"),
            destroy_window: sym!("XDestroyWindow"),
            select_input: sym!("XSelectInput"),
            set_wm_normal_hints: sym!("XSetWMNormalHints"),
            set_class_hint: sym!("XSetClassHint"),
            set_wm_hints: sym!("XSetWMHints"),
            move_window: sym!("XMoveWindow"),
            intern_atom: sym!("XInternAtom"),
            send_event: sym!("XSendEvent"),
            sync: sym!("XSync"),
            connection_number: sym!("XConnectionNumber"),
            pending: sym!("XPending"),
            next_event: sym!("XNextEvent"),
            grab_pointer: sym!("XGrabPointer"),
            ungrab_pointer: sym!("XUngrabPointer"),
            warp_pointer: sym!("XWarpPointer"),
            query_pointer: sym!("XQueryPointer"),
            lookup_string: sym!("XLookupString"),
            create_bitmap_from_data: sym!("XCreateBitmapFromData"),
            create_pixmap_cursor: sym!("XCreatePixmapCursor"),
            define_cursor: sym!("XDefineCursor"),
            free_pixmap: sym!("XFreePixmap"),
        })
    }
}

/// A mapped host window and the Android-side state the engine queries about it.
pub struct HostWindow {
    xlib: Xlib,
    display: Display,
    window: Window,
    /// `XConnectionNumber(display)` — the socket Xlib reads the wire protocol
    /// from. Polling this with a zero timeout is what lets input delivery avoid
    /// ever calling into Xlib when there is nothing queued, which is what keeps
    /// it from blocking the render loop (see `pump_input_events`, below).
    conn_fd: c_int,
    /// Dimensions the engine asked for via `ANativeWindow_setBuffersGeometry`,
    /// which override the window's own size in every query. Android reports the
    /// buffer geometry, not the surface geometry, and the engine sizes its
    /// framebuffers from the answer.
    buffers: Mutex<Geometry>,
    input: Mutex<InputState>,
    pointer_lock: Mutex<PointerLockState>,
    fullscreen: AtomicBool,
}

/// Buttons and timing carried across calls to `pump_input_events`, the way a
/// real `InputDevice` accumulates gesture state between individual X11 events.
struct InputState {
    /// Android `MotionEvent.BUTTON_*` bits currently held down.
    buttons: i32,
    /// `uptimeMillis()` of the button that started the current gesture — reset
    /// to the current time whenever `buttons` goes from zero to non-zero, and
    /// left alone until it goes back to zero. Android's own `downTime` has this
    /// exact meaning: constant across a MOVE/UP sequence, not per-event.
    down_time_ms: i64,
    clock: std::time::Instant,
}

/// X11 pointer capture state.
///
/// X11 has no Wayland relative-pointer protocol in this backend, so the first
/// implementation uses XGrabPointer + XWarpPointer and derives dx/dy from
/// MotionNotify events around a fixed centre.
struct PointerLockState {
    locked: bool,
    suppressed: bool,
    ignore_next_warp: bool,
    centre: (i32, i32),
    saved_root: Option<(i32, i32)>,
}

impl PointerLockState {
    fn new() -> Self {
        Self {
            locked: false,
            suppressed: false,
            ignore_next_warp: false,
            centre: (0, 0),
            saved_root: None,
        }
    }
}

#[derive(Clone, Copy)]
struct Geometry {
    width: i32,
    height: i32,
    format: i32,
}

// The window lives for the process and X11 calls are serialised by the caller.
unsafe impl Send for HostWindow {}
unsafe impl Sync for HostWindow {}

static WINDOW: OnceLock<HostWindow> = OnceLock::new();


/// `XSizeHints`. Only the leading fields matter here, but the struct has to be
/// the full size Xlib expects or `XSetWMNormalHints` reads past the end.
#[repr(C)]
struct XSizeHints {
    flags: c_long,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    min_width: c_int,
    min_height: c_int,
    max_width: c_int,
    max_height: c_int,
    width_inc: c_int,
    height_inc: c_int,
    min_aspect_x: c_int,
    min_aspect_y: c_int,
    max_aspect_x: c_int,
    max_aspect_y: c_int,
    base_width: c_int,
    base_height: c_int,
    win_gravity: c_int,
}

/// `WM_CLASS`, whose second element must match `StartupWMClass` in
/// `packaging/io.github.luohoa97.Cordial.desktop`. A mismatch is invisible in normal
/// use and shows up as an unnamed window in OBS and portal capture pickers, and
/// as a second unbranded taskbar entry. See ADR-009.
const WM_RES_NAME: &str = "cordial";
const WM_RES_CLASS: &str = "Cordial";

#[repr(C)]
struct XClassHint {
    res_name: *mut c_char,
    res_class: *mut c_char,
}

#[repr(C)]
struct XWMHints {
    flags: c_long,
    input: c_int,
    initial_state: c_int,
    icon_pixmap: c_ulong,
    icon_window: Window,
    icon_x: c_int,
    icon_y: c_int,
    icon_mask: c_ulong,
    window_group: c_ulong,
}

/// Where to put the window, in root coordinates.
///
/// A window created at 0,0 lands on the primary monitor, which is not where
/// anyone wants a game window if they kept a second screen for exactly this.
/// `CORDIAL_MONITOR=<n>` centres the window on the nth monitor reported by
/// Xinerama (0 is the first); `CORDIAL_WINDOW_POS=<x>,<y>` overrides with
/// explicit top-left coordinates and wins if both are set.
///
/// Centring rather than pinning to the monitor's corner, because a monitor
/// origin is not a sensible place for a window — on a layout like
/// `0,0 3440x1440` beside `3440,240 1920x1200`, the corner is where the bezel
/// is.
///
/// Xinerama rather than RandR because the query is one call with no resource
/// management, and every multi-head X server that supports RandR also answers
/// Xinerama. Returns (0, 0) when nothing is configured or the query fails, which
/// is exactly the previous behaviour.
struct Placement {
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    fullscreen: bool,
    /// Which monitor was asked for, for `_NET_WM_FULLSCREEN_MONITORS`. A window
    /// manager fullscreens onto whichever monitor it thinks the window is on,
    /// and it does not have to agree with where the window was put — so naming
    /// the monitor explicitly is the only reliable way to say which screen.
    monitor: Option<c_long>,
}

fn placement(win_w: c_int, win_h: c_int) -> Placement {
    let fullscreen = std::env::var_os("CORDIAL_FULLSCREEN").is_some();
    let mut p = Placement { x: 0, y: 0, width: win_w, height: win_h, fullscreen, monitor: None };

    if let Ok(pos) = std::env::var("CORDIAL_WINDOW_POS") {
        let mut parts = pos.split(',').map(str::trim);
        if let (Some(Ok(x)), Some(Ok(y))) = (
            parts.next().map(str::parse::<c_int>),
            parts.next().map(str::parse::<c_int>),
        ) {
            p.x = x;
            p.y = y;
            return p;
        }
        eprintln!("[android] CORDIAL_WINDOW_POS={pos:?} is not <x>,<y>; ignoring");
    }

    let Ok(want) = std::env::var("CORDIAL_MONITOR") else {
        return p;
    };
    let Ok(want) = want.trim().parse::<usize>() else {
        eprintln!("[android] CORDIAL_MONITOR must be a number; ignoring");
        return p;
    };

    #[repr(C)]
    struct XineramaScreenInfo {
        screen_number: c_int,
        x_org: i16,
        y_org: i16,
        width: i16,
        height: i16,
    }

    const RTLD_NOW: c_int = 2;
    // SAFETY: dlopen/dlsym with literal names; every result is null-checked.
    unsafe {
        let lib = dlopen(c"libXinerama.so.1".as_ptr(), RTLD_NOW);
        if lib.is_null() {
            eprintln!("[android] CORDIAL_MONITOR needs libXinerama; ignoring");
            return p;
        }
        let query = dlsym(lib, c"XineramaQueryScreens".as_ptr());
        if query.is_null() {
            return p;
        }
        let query: unsafe extern "C" fn(Display, *mut c_int) -> *mut XineramaScreenInfo =
            std::mem::transmute(query);
        // The caller already has a display open; re-opening here would be a
        // second connection for one query, so this runs against the same one.
        let d = CURRENT_DISPLAY.load(std::sync::atomic::Ordering::Relaxed);
        if d == 0 {
            return p;
        }
        let mut n: c_int = 0;
        let screens = query(d as Display, &mut n);
        if screens.is_null() || n <= 0 {
            return p;
        }
        let list = std::slice::from_raw_parts(screens, n as usize);
        let m = match list.get(want) {
            Some(m) => m,
            None => {
                eprintln!(
                    "[android] CORDIAL_MONITOR={want} but only {n} monitor(s); using the first"
                );
                &list[0]
            }
        };
        p.monitor = Some(want.min(n as usize - 1) as c_long);
        if p.fullscreen {
            // Cover the monitor exactly. The window manager fullscreens onto
            // whichever monitor the window occupies, so filling it first is
            // what pins fullscreen to the requested screen rather than the
            // primary one.
            p.x = m.x_org as c_int;
            p.y = m.y_org as c_int;
            p.width = m.width as c_int;
            p.height = m.height as c_int;
        } else {
            // Clamped at the origin so an oversized window still starts
            // on-screen rather than off the top-left of its monitor.
            p.x = m.x_org as c_int + ((m.width as c_int - win_w) / 2).max(0);
            p.y = m.y_org as c_int + ((m.height as c_int - win_h) / 2).max(0);
        }
        p
    }
}

/// The open display, so `window_origin` can query monitors on the same
/// connection rather than opening a second one for a single call.
static CURRENT_DISPLAY: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Open a window. Fails cleanly when there is no display, which is a normal
/// condition rather than an error — the loader and asset paths do not need one.
pub fn open(width: u32, height: u32, title: &str) -> Result<&'static HostWindow, String> {
    if let Some(w) = WINDOW.get() {
        return Ok(w);
    }
    let xlib = Xlib::load()?;

    // SAFETY: a null display name means $DISPLAY, per Xlib's contract.
    let display = unsafe { (xlib.open_display)(std::ptr::null()) };
    if display.is_null() {
        return Err("no X display (is DISPLAY set?)".into());
    }

    // SAFETY: `display` is open; the geometry and border/background pixels are
    // plain values.
    CURRENT_DISPLAY.store(display as usize, std::sync::atomic::Ordering::Relaxed);
    let place = placement(width as c_int, height as c_int);
    // Reported always, not behind a trace flag: "the window opened on the wrong
    // screen" is a user-visible complaint, and this line is what separates
    // "Cordial computed the wrong position" from "the window manager ignored
    // the one it was given".
    println!(
        "[android] window placement: {}x{} at {},{}{}",
        place.width, place.height, place.x, place.y,
        if place.fullscreen { " (fullscreen)" } else { "" }
    );
    let (ox, oy) = (place.x, place.y);
    // Fullscreen resizes the surface as well as the window: the engine sizes
    // its framebuffers from what `geometry()` reports, so a window covering a
    // 1920x1200 monitor while the surface still says 1280x720 would render a
    // corner of the screen.
    let (width, height) = (place.width as u32, place.height as u32);

    let (window, conn_fd) = unsafe {
        let root = (xlib.default_root_window)(display);
        let w = (xlib.create_simple_window)(display, root, ox, oy, width, height, 0, 0, 0);
        // XStoreName sets WM_NAME, which is XA_STRING — Latin-1, not UTF-8.
        // An em dash here renders as mojibake, so the title is kept ASCII
        // rather than encoded twice for a window caption.
        let ascii: String = title
            .chars()
            .map(|c| if c.is_ascii() { c } else { '-' })
            .collect();
        let name = CString::new(ascii).unwrap_or_default();
        (xlib.store_name)(display, w, name.as_ptr());

        // Without WM hints a window manager is free to place this wherever it
        // likes and to decide it does not take keyboard focus. Both were
        // happening: the window landed on the primary monitor whatever
        // `CORDIAL_MONITOR` said, and key events went elsewhere while mouse
        // events still arrived, because ButtonPress is delivered by pointer
        // position but KeyPress follows the focus.
        //
        // USPosition rather than PPosition: it means "the user asked for this
        // position", which window managers honour where they routinely override
        // a mere program preference.
        let mut hints = XSizeHints {
            flags: 1 << 0, // USPosition
            x: ox,
            y: oy,
            width: width as c_int,
            height: height as c_int,
            min_width: 0, min_height: 0, max_width: 0, max_height: 0,
            width_inc: 0, height_inc: 0,
            min_aspect_x: 0, min_aspect_y: 0, max_aspect_x: 0, max_aspect_y: 0,
            base_width: 0, base_height: 0, win_gravity: 0,
        };
        (xlib.set_wm_normal_hints)(display, w, &mut hints);

        // InputHint | StateHint, asking to be given the keyboard.
        let mut wm = XWMHints {
            flags: (1 << 0) | (1 << 1),
            input: 1,
            initial_state: 1, // NormalState
            icon_pixmap: 0, icon_window: 0, icon_x: 0, icon_y: 0,
            icon_mask: 0, window_group: 0,
        };
        (xlib.set_wm_hints)(display, w, &mut wm);

        // WM_CLASS, so the window is addressable by rule in a tiling or
        // scripted setup rather than only by title. It is also how a capture
        // tool and the desktop entry resolve the window to the application
        // (ADR-009), which is why the class is a constant with a test against
        // the .desktop rather than a literal here.
        let res_name = CString::new(WM_RES_NAME).unwrap_or_default();
        let res_class = CString::new(WM_RES_CLASS).unwrap_or_default();
        let mut class = XClassHint {
            res_name: res_name.as_ptr() as *mut c_char,
            res_class: res_class.as_ptr() as *mut c_char,
        };
        (xlib.set_class_hint)(display, w, &mut class);

        // Hide the host pointer over this window.
        //
        // Roblox draws its own cursor, so the X11 one sits alongside it and the
        // client shows two. Cordial cannot suppress the engine's — that would be
        // reaching into its rendering — so the host's is the one that goes.
        //
        // `XDefineCursor` is scoped to this window: the pointer is invisible
        // while it is over Cordial and completely untouched everywhere else on
        // the desktop. That matters more than it sounds. The global alternatives
        // (`XFixesHideCursor`, grabbing the pointer) change the cursor for the
        // whole session, and this project has already hijacked the developer's
        // real pointer once with `XTestFakeMotionEvent` — window-scoped is the
        // rule here, not a preference.
        //
        // `CORDIAL_SHOW_CURSOR=1` puts it back, for debugging input where seeing
        // where the host thinks the pointer is matters.
        if std::env::var_os("CORDIAL_SHOW_CURSOR").is_none() {
            // A 1x1 all-zero bitmap used as both source and mask: no pixels are
            // drawn and none are opaque, which is the portable "no cursor".
            let blank: [c_char; 1] = [0];
            let pixmap = (xlib.create_bitmap_from_data)(display, w, blank.as_ptr(), 1, 1);
            if pixmap != 0 {
                let mut black = XColor {
                    pixel: 0, red: 0, green: 0, blue: 0, flags: 0, pad: 0,
                };
                let cursor = (xlib.create_pixmap_cursor)(
                    display, pixmap, pixmap, &mut black, &mut black, 0, 0,
                );
                if cursor != 0 {
                    (xlib.define_cursor)(display, w, cursor);
                    eprintln!("[cordial] host cursor hidden over the client window");
                } else {
                    eprintln!("[cordial] could not create a blank cursor; host pointer stays visible");
                }
                // The cursor holds its own reference to the pixmap contents, so
                // the pixmap is freed now rather than leaked for the process.
                (xlib.free_pixmap)(display, pixmap);
            }
        }

        (xlib.select_input)(display, w, INPUT_EVENT_MASK);
        (xlib.map_window)(display, w);
        // Let the window manager finish its own placement before arguing with
        // it. Moving before it has acted is a race that the window manager
        // wins, which is exactly what happened: Cordial computed 3760,480 and
        // the window still came up at 25,62.
        (xlib.sync)(display, 0);

        let root = (xlib.default_root_window)(display);
        const SUBSTRUCTURE_REDIRECT: c_long = 1 << 20;
        const SUBSTRUCTURE_NOTIFY: c_long = 1 << 19;
        const CLIENT_MESSAGE: c_int = 33;
        let atom = |n: &str| -> c_ulong {
            let c = CString::new(n).unwrap_or_default();
            (xlib.intern_atom)(display, c.as_ptr(), 0)
        };

        // An XClientMessageEvent, laid out by hand. Xlib's XEvent union is
        // large and only the leading fields matter here.
        let mut msg = [0u8; 96];
        let mut send = |message_type: c_ulong, data: [c_long; 5]| {
            msg.fill(0);
            let p = msg.as_mut_ptr();
            *(p as *mut c_int) = CLIENT_MESSAGE;
            *(p.add(8) as *mut c_ulong) = 1; // serial
            *(p.add(16) as *mut c_int) = 1; // send_event
            *(p.add(24) as *mut usize) = display as usize;
            *(p.add(32) as *mut Window) = w;
            *(p.add(40) as *mut c_ulong) = message_type;
            *(p.add(48) as *mut c_int) = 32; // format
            for (i, v) in data.iter().enumerate() {
                *(p.add(56 + i * 8) as *mut c_long) = *v;
            }
            (xlib.send_event)(
                display, root, 0,
                SUBSTRUCTURE_REDIRECT | SUBSTRUCTURE_NOTIFY,
                msg.as_mut_ptr() as *mut c_void,
            );
        };

        if place.fullscreen {
            // Name the monitor outright. `_NET_WM_STATE_FULLSCREEN` alone
            // fullscreens onto whichever monitor the window manager believes
            // the window occupies, which is the thing that was wrong.
            if let Some(m) = place.monitor {
                let a = atom("_NET_WM_FULLSCREEN_MONITORS");
                if a != 0 {
                    send(a, [m, m, m, m, 1]);
                }
            }
            let state = atom("_NET_WM_STATE");
            let fs = atom("_NET_WM_STATE_FULLSCREEN");
            if state != 0 && fs != 0 {
                const ADD: c_long = 1;
                send(state, [ADD, fs as c_long, 0, 1, 0]);
            }
        } else if (ox, oy) != (0, 0) {
            (xlib.move_window)(display, w, ox, oy);
        }
        (xlib.flush)(display);
        (xlib.sync)(display, 0);

        (w, (xlib.connection_number)(display))
    };

    let host = HostWindow {
        xlib,
        display,
        window,
        conn_fd,
        buffers: Mutex::new(Geometry {
            width: width as i32,
            height: height as i32,
            format: WINDOW_FORMAT_RGBA_8888,
        }),
        input: Mutex::new(InputState {
            buttons: 0,
            down_time_ms: 0,
            clock: std::time::Instant::now(),
        }),
        pointer_lock: Mutex::new(PointerLockState::new()),
        fullscreen: AtomicBool::new(place.fullscreen),
    };
    // No touchscreen, and that is a statement about this backend rather than
    // about the machine: X11 core input has no touch at all, XInput2's is a
    // separate extension nothing here binds, and so a touchscreen on this host
    // could not reach Cordial through this path however present it is. Saying
    // false is therefore true of what the client can actually receive, which is
    // what `isTouchDevice` is for. A user on a touchscreen who wants the mobile
    // interface on X11 has `CORDIAL_INPUT_TOUCH=1`, which overrides this.
    super::input::report_touchscreen(false);
    Ok(WINDOW.get_or_init(|| host))
}

/// Whether the pointer lock should be held, and what the Escape-suppression
/// latch should become, given one pump's inputs.
///
/// Pulled out of `sync_pointer_lock` so the three independent reasons to want
/// the lock — the engine's own request, a camera-button drag, and the forced
/// override — and the latch that keeps Escape from being immediately undone by
/// a button still held down the same frame are unit-testable without a live X
/// server, the same reason [`is_final_expose`] and `input.rs`'s
/// `resolve_mouse_delta`/`touchscreen_reported` take their inputs as plain
/// values rather than reading global state themselves. A ~260-line addition
/// with no unit test would otherwise be a first for this file.
///
/// The latch clears itself the first pump nothing is asking for the lock —
/// `previously_suppressed` carried forward unchanged only while `asked` stays
/// true — which is what lets a released Escape be re-armed by the next camera
/// drag rather than staying suppressed for the rest of the session.
fn pointer_lock_decision(
    engine_wants: bool,
    buttons: i32,
    no_drag_lock: bool,
    force: bool,
    previously_suppressed: bool,
) -> (bool, bool) {
    const CAMERA_BUTTONS: i32 = super::input::BUTTON_SECONDARY | super::input::BUTTON_TERTIARY;
    let dragging = !no_drag_lock && (buttons & CAMERA_BUTTONS) != 0;
    let asked = engine_wants || dragging || force;
    let suppressed = asked && previously_suppressed;
    (asked && !suppressed, suppressed)
}

/// The relative motion implied by one `MotionNotify` while the pointer is
/// locked, or `None` if the event is the synthetic echo of this backend's own
/// `XWarpPointer` call back to the centre and must be swallowed rather than
/// reported as movement.
///
/// Separate from [`HostWindow::dispatch_motion`] for the same reason as
/// [`pointer_lock_decision`] above: the centre-relative arithmetic and the
/// echo check are the two things in the locked motion path actually worth
/// getting wrong, and neither needs a window to test.
fn locked_pointer_delta(
    event_pos: (i32, i32),
    centre: (i32, i32),
    ignore_next_warp: bool,
) -> Option<(i32, i32)> {
    if ignore_next_warp && event_pos == centre {
        return None;
    }
    Some((event_pos.0 - centre.0, event_pos.1 - centre.1))
}

impl HostWindow {
    /// The X11 `Window`, which is what `eglCreateWindowSurface` takes as its
    /// native window on this platform.
    pub fn egl_native_window(&self) -> c_ulong {
        self.window
    }

    pub fn egl_native_display(&self) -> Display {
        self.display
    }

    /// The X connection's descriptor, so the looper can wait on input rather
    /// than poll for it.
    pub fn connection_fd(&self) -> c_int {
        self.conn_fd
    }

    pub fn geometry(&self) -> (i32, i32, i32) {
        let g = *self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        (g.width, g.height, g.format)
    }

    /// Ask the window manager to add or remove `_NET_WM_STATE_FULLSCREEN`, the
    /// same message `open` sends when `--fullscreen` was asked for at startup.
    ///
    /// This exists for one reason and it is worth stating plainly: **the
    /// fullscreen bug cannot be photographed on Wayland.** This GNOME session
    /// refuses `org.gnome.Shell.Screenshot` and `ScreenshotWindow` with
    /// `AccessDenied`, `grim` is wlroots-only, and `import` cannot see a native
    /// Wayland surface. An X11 window can be photographed with
    /// `import -window`, so the transition can at least be *looked at* on one
    /// backend. It is not the same code path as the Wayland one — GTK and a
    /// subsurface are not involved here — and a result from it says what the
    /// engine does with a fullscreen-sized surface, not what
    /// `sync_canvas_geometry` does.
    pub fn set_fullscreen(&self, on: bool) {
        let xlib = &self.xlib;
        // SAFETY: `display`/`window` are this struct's own live handles, and
        // the event is the same 96-byte `XClientMessageEvent` layout `open`
        // builds by hand a few hundred lines above; see its comment for why the
        // union is written out rather than declared.
        unsafe {
            let root = (xlib.default_root_window)(self.display);
            let name = |s: &std::ffi::CStr| (xlib.intern_atom)(self.display, s.as_ptr(), 0);
            let (state, fs) = (name(c"_NET_WM_STATE"), name(c"_NET_WM_STATE_FULLSCREEN"));
            if state == 0 || fs == 0 {
                return;
            }
            const REMOVE: c_long = 0;
            const ADD: c_long = 1;
            const CLIENT_MESSAGE: c_int = 33;
            const SUBSTRUCTURE_REDIRECT: c_long = 1 << 20;
            const SUBSTRUCTURE_NOTIFY: c_long = 1 << 19;
            let mut msg = [0u8; 96];
            let p = msg.as_mut_ptr();
            *(p as *mut c_int) = CLIENT_MESSAGE;
            *(p.add(8) as *mut c_ulong) = 1;
            *(p.add(16) as *mut c_int) = 1;
            *(p.add(24) as *mut usize) = self.display as usize;
            *(p.add(32) as *mut Window) = self.window;
            *(p.add(40) as *mut c_ulong) = state;
            *(p.add(48) as *mut c_int) = 32;
            for (i, v) in [if on { ADD } else { REMOVE }, fs as c_long, 0, 1, 0].iter().enumerate() {
                *(p.add(56 + i * 8) as *mut c_long) = *v;
            }
            (xlib.send_event)(
                self.display,
                root,
                0,
                SUBSTRUCTURE_REDIRECT | SUBSTRUCTURE_NOTIFY,
                msg.as_mut_ptr() as *mut c_void,
            );
            (xlib.flush)(self.display);
        }
        self.fullscreen.store(on, Ordering::Relaxed);
    }

    /// Take or release the pointer to match what the engine and the mouse are
    /// currently asking for. Called at both ends of `pump_input_events`, so a
    /// button released mid-drain still ungrabs before the next frame rather
    /// than a whole pump late.
    ///
    /// This duplicates Wayland's own `sync_pointer_lock` (`wayland.rs:3622`)
    /// rather than sharing it with it, which is exactly the thing ADR-024
    /// asks not to happen to logic common to both backends. Not shared here
    /// on purpose, for now: the two currently compute genuinely different
    /// things from different primitives. Wayland reasons about a
    /// pre-/post-acceleration delta pair handed to it by
    /// `zwp_relative_pointer_v1`; this backend reasons about warping the
    /// pointer back to a fixed centre and filtering out the synthetic
    /// `MotionNotify` that warp itself produces. A shared function today
    /// would be a wrapper over two unlike mechanisms, not one mechanism
    /// written once.
    ///
    /// ADR-028 records the actual plan and should be read rather than
    /// inferred from this comment: X11 input moves to XInput2, taking its
    /// motion from `XI_RawMotion` instead of core `MotionNotify`. That
    /// event's `raw_values` (pre-acceleration) and `valuators`
    /// (post-acceleration) are exactly the pair Wayland already has, and once
    /// both backends compute the same pair from the same kind of source, the
    /// module ADR-024 asks for is a real refactor rather than a wrapper.
    /// **This warp path does not go away once that lands.** ADR-028 keeps it
    /// as the fallback for a server that refuses `XIQueryVersion` (older than
    /// X.Org 1.7): core X11's `MotionNotify` has already been through the
    /// server's own acceleration curve by the time it is reported, and no
    /// core request recovers what the device actually sent, so the warp
    /// cannot be the default but stays as the only thing that works when XI2
    /// is not there to ask. Landing XI2 is sequenced after this change, not
    /// inside it — see ADR-028's "Sequencing" — so `pointer_lock_decision`
    /// and `locked_pointer_delta` above are not where that work belongs.
    fn sync_pointer_lock(&self) {
        let engine_wants = super::input::engine_wants_pointer_lock() == Some(true);

        let buttons = self
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .buttons;

        let no_drag_lock = std::env::var_os("CORDIAL_NO_DRAG_LOCK").is_some();
        let force = std::env::var_os("CORDIAL_FORCE_POINTER_LOCK").is_some();

        if std::env::var_os("CORDIAL_NO_POINTER_LOCK").is_some() {
            self.release_pointer_lock();
            return;
        }

        let mut state = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let (want, suppressed) =
            pointer_lock_decision(engine_wants, buttons, no_drag_lock, force, state.suppressed);
        state.suppressed = suppressed;
        let held = state.locked;
        drop(state);

        if want && !held {
            self.lock_pointer();
        } else if !want && held {
            self.release_pointer_lock();
        }
    }

    fn lock_pointer(&self) {
        let (width, height, _) = self.geometry();

        if width <= 0 || height <= 0 {
            return;
        }

        let centre = (width / 2, height / 2);
        let root =
            unsafe { (self.xlib.default_root_window)(self.display) };

        let mut root_return = 0;
        let mut child_return = 0;
        let mut root_x = 0;
        let mut root_y = 0;
        let mut win_x = 0;
        let mut win_y = 0;
        let mut mask = 0;

        let queried = unsafe {
            (self.xlib.query_pointer)(
                self.display,
                root,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };

        let saved_root =
            if queried != 0 { Some((root_x, root_y)) } else { None };

        // X11 CurrentTime is 0.
        // owner_events = True
        // pointer_mode = GrabModeAsync
        // keyboard_mode = GrabModeAsync
        let result = unsafe {
            (self.xlib.grab_pointer)(
                self.display,
                self.window,
                1,
                0x4 | 0x8 | 0x40,
                1,
                1,
                self.window,
                0,
                0,
            )
        };

        if result != 0 {
            // Printed unconditionally, not gated on `trace_mouse()`. The grab
            // this replaces (`set_pointer_capture`) reported a refusal
            // unconditionally too: another client already holding the
            // pointer is a real failure of the lock the user asked for, and
            // burying it behind a trace flag nobody has set by default is
            // exactly the kind of silent stub AGENTS.md rules out.
            eprintln!("[cordial] X11 pointer lock was refused (XGrabPointer={result})");
            return;
        }

        {
            let mut state = self
                .pointer_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());

            state.locked = true;
            state.ignore_next_warp = true;
            state.centre = centre;
            state.saved_root = saved_root;
        }

        unsafe {
            (self.xlib.warp_pointer)(
                self.display,
                0,
                self.window,
                0,
                0,
                0,
                0,
                centre.0,
                centre.1,
            );
            (self.xlib.flush)(self.display);
        }

        super::input::reset_mouse_delta();
        // `forget_pending_unlocked_delta`'s own doc says it is called "at
        // every site that also calls `reset_mouse_delta`" -- nothing on the
        // X11 path currently writes `PENDING_UNLOCKED_DELTA` (only
        // `wayland.rs`'s `relative_pointer_motion` does), so there is never
        // anything here to forget. Called anyway so the doc's claim stays
        // true rather than true of Wayland only, and so a lock taken right
        // as the pointer crosses into the canvas does not carry a stray
        // sample forward if this backend ever grows a relative-motion source
        // of its own.
        super::input::forget_pending_unlocked_delta();

        if super::input::trace_mouse() {
            eprintln!(
                "[cordial] X11 pointer lock acquired at ({}, {})",
                centre.0,
                centre.1
            );
        }
    }

    fn release_pointer_lock(&self) {
        let (was_locked, saved_root) = {
            let mut state = self
                .pointer_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());

            let was_locked = state.locked;
            let saved_root = state.saved_root.take();

            state.locked = false;
            state.ignore_next_warp = false;
            state.centre = (0, 0);

            (was_locked, saved_root)
        };

        if !was_locked {
            return;
        }

        unsafe {
            // X11 CurrentTime is 0.
            (self.xlib.ungrab_pointer)(self.display, 0);

            if let Some((x, y)) = saved_root {
                let root =
                    (self.xlib.default_root_window)(self.display);

                (self.xlib.warp_pointer)(
                    self.display,
                    root,
                    root,
                    0,
                    0,
                    0,
                    0,
                    x,
                    y,
                );
            }

            (self.xlib.flush)(self.display);
        }

        super::input::reset_mouse_delta();
        // See the matching call in `lock_pointer` for why this is here too.
        super::input::forget_pending_unlocked_delta();

        if super::input::trace_mouse() {
            eprintln!("[cordial] X11 pointer lock released");
        }
    }

    fn escape_pointer_lock(&self) -> bool {
        let held = self
            .pointer_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .locked;

        if !held {
            return false;
        }

        self.pointer_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .suppressed = true;

        self.release_pointer_lock();
        true
    }

    pub fn close(&self) {
        // SAFETY: both handles came from this struct's own creation calls.
        unsafe {
            (self.xlib.destroy_window)(self.display, self.window);
            (self.xlib.flush)(self.display);
        }
    }
}

pub fn current() -> Option<&'static HostWindow> {
    WINDOW.get()
}

// ------------------------------------------------------------- input pump
//
// Mouse and keyboard, delivered to the engine through the same AGDK
// `GameActivity` natives real Android input goes through — `onTouchEventNative`
// and `onKeyDownNative`/`onKeyUpNative` — via `cordial-linker-sys`'s
// `game_activity` module and the synthesised `MotionEvent`/`KeyEvent` objects in
// `native/game_activity.cpp`.
//
// The design constraint is that this must never block: it runs inside
// `looper::pump`'s own ~50ms-timeout loop, on the thread that also owns the
// engine's message pump, so any call here that waits is a frame the engine
// never gets to render. `XPending`/`XNextEvent` are what actually read queued
// events, but calling either when nothing is queued risks a blocking read in
// at least some libX11 builds. So every drain starts with a zero-timeout
// `poll(2)` on Xlib's own connection fd (`XConnectionNumber`) — a pure
// kernel-side check that can only return immediately — and only touches Xlib
// at all when that says there is something to read.

/// The common prefix shared by `XKeyEvent`, `XButtonEvent` and `XMotionEvent`.
///
/// Xlib deliberately lays these three structs out identically — that is
/// documented behaviour, not a coincidence being relied on here — except for
/// one field whose *meaning* differs: `keycode` for key events, `button` for
/// button events, `is_hint` for motion. It is read generically as `detail` and
/// interpreted according to `type_`.
#[repr(C)]
struct XInputEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    root: c_ulong,
    subwindow: c_ulong,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    detail: c_uint,
    same_screen: c_int,
}

// X11 event `type` values, from X.h.
const KEY_PRESS: c_int = 2;
const KEY_RELEASE: c_int = 3;
const MOTION_NOTIFY: c_int = 6;
const FOCUS_OUT: c_int = 10;
const BUTTON_PRESS: c_int = 4;
const BUTTON_RELEASE: c_int = 5;
const EXPOSE: c_int = 12;
const CONFIGURE_NOTIFY: c_int = 22;

/// `XConfigureEvent`. Another distinct layout: it carries the window's new
/// geometry rather than a damaged rectangle.
#[repr(C)]
struct XConfigureEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    event: c_ulong,
    window: c_ulong,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    border_width: c_int,
    above: c_ulong,
    override_redirect: c_int,
}

/// `XExposeEvent`. A different layout from `XInputEvent` above — Expose
/// carries a damaged rectangle and a batching `count`, not a pointer/keycode
/// `detail` — so it gets its own struct rather than being folded into the
/// shared one.
#[repr(C)]
struct XExposeEvent {
    type_: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    /// How many more `Expose` events follow for the same repaint, so a
    /// window manager can deliver several damaged rectangles as a batch. 0 on
    /// the last (or only) one — exactly the point at which the whole window
    /// has finished telling us what it needs repainted, and the one point at
    /// which `onSurfaceRedrawNeededNative` should actually fire. Firing on
    /// every event in the batch would mean N redraw requests for one
    /// exposure.
    count: c_int,
}

// X11 modifier bits (X.h) actually consulted below.
const SHIFT_MASK: c_uint = 1 << 0;
const LOCK_MASK: c_uint = 1 << 1; // Caps Lock
const CONTROL_MASK: c_uint = 1 << 2;
const MOD1_MASK: c_uint = 1 << 3; // Alt, on essentially every layout in practice

use super::input::{META_ALT_ON, META_CAPS_LOCK_ON, META_CTRL_ON, META_SHIFT_ON};

fn android_meta_state(x11_state: c_uint) -> i32 {
    let mut m = 0;
    if x11_state & SHIFT_MASK != 0 {
        m |= META_SHIFT_ON;
    }
    if x11_state & CONTROL_MASK != 0 {
        m |= META_CTRL_ON;
    }
    if x11_state & MOD1_MASK != 0 {
        m |= META_ALT_ON;
    }
    if x11_state & LOCK_MASK != 0 {
        m |= META_CAPS_LOCK_ON;
    }
    m
}

// `android.view.MotionEvent.BUTTON_*` / `ACTION_*`, and the keysym table, now
// live in `input.rs` — shared with the Wayland backend. See its module doc for
// why the keysym table in particular carries over unchanged: X11 keysyms and
// XKB keysyms are the same numbering.
use super::input::{
    deliver_key, deliver_surface_redraw, deliver_mouse, edit_text_buffer, keysym_to_android,
    pass_key_event, pass_mouse_button, pass_mouse_move, pass_text, report_keyboard_state, Caret,
    Edit, ACTION_BUTTON_PRESS, ACTION_BUTTON_RELEASE, ACTION_DOWN, ACTION_HOVER_MOVE, ACTION_MOVE,
    ACTION_UP, BUTTON_BACK, BUTTON_FORWARD, BUTTON_PRIMARY, BUTTON_SECONDARY, BUTTON_TERTIARY,
};

/// X11 numbers buttons 1/2/3 as left/middle/right; Android's bit assignment
/// puts secondary (right) before tertiary (middle). X11's conventional 8/9
/// side buttons become Android's back/forward bits. Buttons 4-7 are the wheel
/// and are handled by [`x11_button_to_wheel`] instead — they must not fall
/// through to here, because delivering a scroll as some button press is worse
/// than dropping it.
fn x11_button_to_android(button: c_uint) -> Option<i32> {
    match button {
        1 => Some(BUTTON_PRIMARY),
        2 => Some(BUTTON_TERTIARY),
        3 => Some(BUTTON_SECONDARY),
        8 => Some(BUTTON_BACK),
        9 => Some(BUTTON_FORWARD),
        _ => None,
    }
}

/// X11's representation of the wheel: four pseudo-buttons, one press-and-release
/// pair per detent, in the order up/down/left/right.
///
/// Returns `(hscroll, vscroll)` in detents with Android's signs — positive
/// away from the user, positive to the right — which is the unit
/// [`super::input::wheel`] takes. One notch is exactly one here, with no
/// conversion to guess at, which is the one thing X11 does better than
/// `wl_pointer.axis`.
fn x11_button_to_wheel(button: c_uint) -> Option<(f32, f32)> {
    match button {
        4 => Some((0.0, 1.0)),
        5 => Some((0.0, -1.0)),
        6 => Some((-1.0, 0.0)),
        7 => Some((1.0, 0.0)),
        _ => None,
    }
}

// `*mut c_void` rather than a typed `*mut PollFd`, to match the `poll`
// declaration `bionic::mod` already has for the emulated libc's own use of the
// same host symbol — `rustc` warns (`clashing_extern_declarations`) about two
// `extern "C" fn poll` with different signatures anywhere in the crate, since
// both ultimately bind the one process-wide C symbol.
extern "C" {
    fn poll(fds: *mut c_void, nfds: c_ulong, timeout_ms: c_int) -> c_int;
}
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
const POLLIN: i16 = 0x001;

/// Whether an `Expose` event is the last one in its batch — `count` is how
/// many more follow for the same repaint, so 0 is the point at which the
/// window has finished describing what it needs redrawn. Pulled out as its
/// own function so the batching decision is unit-testable without a live X11
/// connection.
fn is_final_expose(count: c_int) -> bool {
    count == 0
}

impl HostWindow {
    fn now_ms(&self) -> i64 {
        let state = self.input.lock().unwrap_or_else(|e| e.into_inner());
        state.clock.elapsed().as_millis() as i64
    }

    fn dispatch_button(&self, handle: i64, ev: &XInputEvent, press: bool) {
        // The wheel first. X11 sends a press *and* a release for every detent,
        // and a wheel has no "released" state to report — sending both would
        // scroll twice per notch, so the release half is discarded here rather
        // than by the engine.
        if let Some((hscroll, vscroll)) = x11_button_to_wheel(ev.detail) {
            if press {
                let now = self.now_ms();
                super::input::wheel(handle, ev.x as f32, ev.y as f32, hscroll, vscroll, now);
            }
            return;
        }
        let Some(android_button) = x11_button_to_android(ev.detail) else {
            return;
        };
        let (x, y) = (ev.x as f32, ev.y as f32);

        let mut state = self.input.lock().unwrap_or_else(|e| e.into_inner());
        let now = state.clock.elapsed().as_millis() as i64;

        if press {
            if state.buttons == 0 {
                state.down_time_ms = now;
            }
            state.buttons |= android_button;
            let (buttons, down_time) = (state.buttons, state.down_time_ms);
            drop(state);
            // Real Android mouse input delivers exactly this pair for a
            // click: ACTION_DOWN establishes the gesture, then
            // ACTION_BUTTON_PRESS names which button did it.
            deliver_mouse(handle, ACTION_DOWN, x, y, buttons, 0, now, down_time);
            deliver_mouse(handle, ACTION_BUTTON_PRESS, x, y, buttons, android_button, now, down_time);
        } else {
            state.buttons &= !android_button;
            let (buttons, down_time) = (state.buttons, state.down_time_ms);
            drop(state);
            deliver_mouse(handle, ACTION_BUTTON_RELEASE, x, y, buttons, android_button, now, down_time);
            deliver_mouse(handle, ACTION_UP, x, y, buttons, 0, now, down_time);
        }

        // The interface's own input path, alongside AGDK's — and every button,
        // not only the primary one. The gate that used to stand here dropped
        // right and middle before they reached Roblox at all, and a
        // right-button drag is how a mouse turns the camera.
        pass_mouse_button(x, y, press, android_button);
    }

    fn dispatch_motion(&self, handle: i64, ev: &XInputEvent) {
        {
            let mut state = self
                .pointer_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());

            if state.locked {
                let centre = state.centre;

                let Some((dx, dy)) =
                    locked_pointer_delta((ev.x, ev.y), centre, state.ignore_next_warp)
                else {
                    state.ignore_next_warp = false;
                    return;
                };
                let (cx, cy) = centre;

                // Preserve the existing Android/AGDK motion path while the
                // pointer is captured. The absolute position remains the
                // capture centre, but Roblox still sees the same MotionEvent
                // sequence it saw before pointer locking was introduced.
                let input = self
                    .input
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());

                let buttons = input.buttons;
                let down_time = input.down_time_ms;
                let now = input.clock.elapsed().as_millis() as i64;
                drop(input);

                let action =
                    if buttons != 0 {
                        ACTION_MOVE
                    } else {
                        ACTION_HOVER_MOVE
                    };

                deliver_mouse(
                    handle,
                    action,
                    cx as f32,
                    cy as f32,
                    buttons,
                    0,
                    now,
                    down_time,
                );

                // The relative delta is still delivered through Roblox's
                // NativeInputInterface path for camera rotation.
                if dx != 0 || dy != 0 {
                    drop(state);

                    super::input::pass_mouse_move_delta(
                        cx as f32,
                        cy as f32,
                        dx as f32,
                        dy as f32,
                    );

                    let mut state = self
                        .pointer_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());

                    state.ignore_next_warp = true;
                    drop(state);

                    unsafe {
                        (self.xlib.warp_pointer)(
                            self.display,
                            0,
                            self.window,
                            0,
                            0,
                            0,
                            0,
                            cx,
                            cy,
                        );
                        (self.xlib.flush)(self.display);
                    }
                }

                return;
            }
        }

        let (x, y) = (ev.x as f32, ev.y as f32);
        let state = self
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let now = state.clock.elapsed().as_millis() as i64;
        let (buttons, down_time) = (state.buttons, state.down_time_ms);
        drop(state);
        // A held button makes this a drag — part of the gesture the DOWN
        // started, hence ACTION_MOVE with the same down_time. No button held
        // makes it a hover, which is what a mouse (as opposed to touch) sends
        // when it moves without a button down.
        let action = if buttons != 0 { ACTION_MOVE } else { ACTION_HOVER_MOVE };
        deliver_mouse(handle, action, x, y, buttons, 0, now, down_time);
        // And the path the interface reads. Both are driven: AGDK's contract is
        // real and the engine consumes it, it is simply not what hit-tests the
        // Lua UI.
        pass_mouse_move(x, y);
    }

    fn dispatch_key(&self, handle: i64, buf: &mut [u8; 256], down: bool) {
        let mut keysym: c_ulong = 0;
        let mut text = [0u8; 8];
        // SAFETY: `buf` holds the XKeyEvent `XNextEvent` just filled, laid out
        // identically to `XInputEvent` above (that layout compatibility is
        // documented Xlib behaviour). A null compose-status argument is
        // documented to mean "skip compose-key processing", not "pass a valid
        // pointer" — Xlib treats it as optional.
        let n = unsafe {
            (self.xlib.lookup_string)(
                buf.as_mut_ptr() as *mut c_void,
                text.as_mut_ptr() as *mut c_char,
                text.len() as c_int,
                &mut keysym,
                std::ptr::null_mut(),
            )
        };
        let ev = unsafe { &*(buf.as_ptr() as *const XInputEvent) };
        let unicode = if n > 0 { text[0] as i32 } else { 0 };
        let meta = android_meta_state(ev.state);
        let now = self.now_ms();

        if down && keysym == 0xff1b && self.escape_pointer_lock() {
            return;
        }

        // XK_F11. Like the Wayland game window, this backend is not the GTK
        // launcher and therefore cannot inherit its `win.fullscreen` action.
        if keysym == 0xffc8 {
            if down {
                self.set_fullscreen(!self.fullscreen.load(Ordering::Relaxed));
            }
            return;
        }

        if super::input::trace_text() {
            // `text=` is a length unless `CORDIAL_TRACE_TEXT_SHOW_PASSWORDS=1`:
            // one character at a time is still a password, printed slowly.
            eprintln!(
                "[cordial] key {} keysym={keysym:#x} text={} keycode={:?} focus={:?}",
                if down { "down" } else { "up" },
                super::input::redacted(
                    std::str::from_utf8(&text[..n.max(0) as usize]).unwrap_or("")
                ),
                keysym_to_android(keysym),
                cordial_linker_sys::game_activity::focused_textbox(),
            );
        }

        // Real per-key downTime tracking (one slot per held key) is not
        // implemented; both fields use the current time on every call. That
        // is a simplification, not a faithful `downTime`, and is called out in
        // the report — it does not block a key reaching the engine, only the
        // precision of one timing field most UI code does not consult.
        // Keys the Android keycode table covers. A keysym with no mapping — the
        // shifted symbols, `@` among them — used to `return` here, which also
        // skipped the text path below and silently dropped the character. Text
        // does not need an Android keycode: `@` is a character whether or not
        // AKEYCODE has a name for it, and an email address is unusable without
        // it. So this is now a branch rather than an exit.
        if let Some(keycode) = keysym_to_android(keysym) {
            deliver_key(handle, down, keycode, ev.detail as i32, meta, 0, unicode, now, now);
            // The evdev code, not the Android keycode. X11 keycodes are evdev
            // offset by 8 -- XKB reserves the low 8 for historical reasons every
            // consumer has to undo. See `pass_key_event`.
            pass_key_event(down, ev.detail as i32 - 8, meta);
        } else {
            super::trace(format_args!("unmapped X11 keysym {keysym:#x}"));
        }

        // And the text path. Android text fields are edited by state, not by
        // keystrokes — delivering the key alone leaves the box empty, which is
        // exactly what the login form did before this. Only on key-down: a
        // release would deliver the same state twice.
        if down {
            // Only when the engine has told us a box is focused, via
            // `showKeyboard`. Sending text with no focused box means sending it
            // to handle 0, which is not a box — the engine drops it, silently,
            // which is exactly how this failed before.
            let Some(which) = cordial_linker_sys::game_activity::focused_textbox() else {
                return;
            };
            // Ctrl+V, before anything reads the character.
            //
            // There is no engine call to look for here and that is correct
            // rather than missing: on Android the `EditText` over the GL
            // surface handles the paste itself and the engine only ever sees
            // text arrive through `gametextinput`. Cordial is that editor, so
            // a paste is an insert through this same path — see
            // `clipboard::paste_into_engine`, which does exactly what the loop
            // below does with a typed character.
            if super::input::is_paste_shortcut(keysym, meta) {
                if let Err(e) = super::clipboard::paste_into_engine(handle) {
                    super::trace(format_args!("clipboard paste failed: {e}"));
                }
                return;
            }
            let typed = if n > 0 {
                std::str::from_utf8(&text[..n as usize]).unwrap_or("")
            } else {
                ""
            };
            // Editing keys, before text: an IME consumes these itself rather
            // than committing them, and `XLookupString` reports nothing for
            // them anyway. Keysyms from keysymdef.h.
            let edit = match keysym {
                0xff08 => Edit::Backspace,           // XK_BackSpace
                0xffff => Edit::Delete,              // XK_Delete
                0xff51 => Edit::Move(Caret::Left),   // XK_Left
                0xff53 => Edit::Move(Caret::Right),  // XK_Right
                0xff50 => Edit::Move(Caret::Home),   // XK_Home
                0xff57 => Edit::Move(Caret::End),    // XK_End
                _ => Edit::Insert(typed),
            };
            if let Some((contents, caret)) = edit_text_buffer(edit) {
                // AGDK's GameTextInput path, and Roblox's own. Both are driven
                // for the same reason as the mouse: the first is the documented
                // contract, the second is what the interface reads.
                let _ =
                    cordial_linker_sys::game_activity::text_input(handle, &contents, caret, caret);
                pass_text(which, &contents, caret);
                deliver_surface_redraw(handle);
            }
        }
    }

    /// Drain and deliver whatever X11 input is already queued, then return.
    /// See the module-level comment above for why this never blocks.
    fn pump_input_events(&self, handle: i64) {
        self.sync_pointer_lock();

        // Before draining input: if the engine has opened or closed an editor
        // since last time, acknowledge it. Cheap — an atomic load and a
        // comparison unless something actually changed.
        if super::input::keyboard_report_enabled() {
            let (gw, gh, _) = self.geometry();
            report_keyboard_state((gw, gh));
        }

        let mut pfd = PollFd { fd: self.conn_fd, events: POLLIN, revents: 0 };
        // SAFETY: `pfd` is a live array of length 1; a 0ms timeout makes this a
        // pure non-blocking check.
        let ready = unsafe { poll(&mut pfd as *mut PollFd as *mut c_void, 1, 0) };
        if ready <= 0 {
            return;
        }

        // Bounded so a burst of queued motion events cannot turn one drain
        // call into unbounded work inside the render loop's own timing
        // budget.
        const MAX_EVENTS_PER_DRAIN: usize = 64;
        for _ in 0..MAX_EVENTS_PER_DRAIN {
            // SAFETY: `self.display` is open; reached only after `poll` above
            // found the connection readable (or a previous iteration left
            // events already queued client-side).
            if unsafe { (self.xlib.pending)(self.display) } <= 0 {
                break;
            }
            let mut buf = [0u8; 256];
            // SAFETY: 256 bytes covers every concrete event struct in the
            // `XEvent` union on every platform Xlib ships for; `buf` is live
            // for the call.
            unsafe { (self.xlib.next_event)(self.display, buf.as_mut_ptr() as *mut c_void) };
            let event_type = unsafe { *(buf.as_ptr() as *const c_int) };

            match event_type {
                BUTTON_PRESS | BUTTON_RELEASE => {
                    let ev = unsafe { &*(buf.as_ptr() as *const XInputEvent) };
                    self.dispatch_button(handle, ev, event_type == BUTTON_PRESS);
                }
                MOTION_NOTIFY => {
                    let ev = unsafe { &*(buf.as_ptr() as *const XInputEvent) };
                    self.dispatch_motion(handle, ev);
                }
                KEY_PRESS | KEY_RELEASE => {
                    self.dispatch_key(handle, &mut buf, event_type == KEY_PRESS);
                }
                FOCUS_OUT => {
                    self.release_pointer_lock();

                    let mut state = self
                        .input
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());

                    state.buttons = 0;
                }
                EXPOSE => {
                    // SAFETY: `event_type == EXPOSE` means `XNextEvent` just
                    // filled `buf` as the `XExposeEvent` member of Xlib's
                    // `XEvent` union — a different layout from
                    // `XInputEvent` above (see `XExposeEvent`'s own doc
                    // comment), but the one this specific event type is
                    // documented to have.
                    let ev = unsafe { &*(buf.as_ptr() as *const XExposeEvent) };
                    if is_final_expose(ev.count) {
                        deliver_surface_redraw(handle);
                    }
                }
                CONFIGURE_NOTIFY => {
                    // SAFETY: the event type says `XNextEvent` filled `buf` as
                    // the `XConfigureEvent` member of Xlib's union.
                    let ev = unsafe { &*(buf.as_ptr() as *const XConfigureEvent) };
                    self.dispatch_configure(handle, ev.width, ev.height);
                }
                _ => {}
            }
        }

        self.sync_pointer_lock();
    }

    /// The window changed size. Update what the engine is told about it.
    ///
    /// X sends `ConfigureNotify` for moves as well as resizes, and a resize
    /// drag produces a stream of them, so this returns early unless the size
    /// actually changed — re-driving `onSurfaceChangedNative` for every pixel
    /// of a drag would rebuild the engine's framebuffers dozens of times a
    /// second.
    ///
    /// **This used to update only the render surface.** `load.rs` calls
    /// `config::set_screen` once, right after the window first opens, so
    /// `AConfiguration_getScreenWidthDp`/`getScreenHeightDp` agree with the
    /// window at launch — but nothing on this path ever called it again. A
    /// resize (and fullscreen is a resize) kept the true render surface
    /// current while `AConfiguration` went on answering whatever size the
    /// window had when it first opened, which is a screen-size contradiction
    /// of exactly the shape `docs/analysis/platform-identity.md` warns about,
    /// just discovered on the resize path rather than the launch one. Calling
    /// it here as well closes that gap for `AConfiguration` specifically.
    ///
    /// **What this does not close.** `native/init_params.cpp`'s own
    /// `DisplayMetrics`/`Configuration`/`InitParams` objects are a separate
    /// path — `DisplayMetrics` in particular is driven by a `g_width`/
    /// `g_height` pair set only through `set_display_size`, which has no
    /// caller anywhere in this tree, so it is stuck at its compiled 1280x720
    /// regardless of this call. That file is out of scope for this change
    /// (see AGENTS.md's file list for who owns it); this closes only the
    /// `AConfiguration` half of the contradiction, not the `DisplayMetrics`
    /// or `User-Agent` half. `INFERRED` that either half affects the camera —
    /// nothing here was run against the engine to check.
    fn dispatch_configure(&self, handle: i64, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        let format = {
            let mut g = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
            if g.width == width && g.height == height {
                return;
            }
            g.width = width;
            g.height = height;
            g.format
        };
        super::config::set_screen(width, height);
        if let Err(e) = cordial_linker_sys::game_activity::surface_resized(
            handle, format, width, height,
        ) {
            super::trace(format_args!("surface resize failed: {e}"));
        }
    }
}

/// Drain and deliver whatever host input is queued, for the current window (if
/// one is open — the loader/asset-only paths that never call `open()` make
/// this a no-op).
pub fn pump_input_events(handle: i64) {
    if let Some(w) = current() {
        w.pump_input_events(handle);
    }
}

// ------------------------------------------------------- ANativeWindow_*

/// The `ANativeWindow*` handed to the engine.
///
/// There is exactly one window, so the pointer is the `HostWindow` itself rather
/// than a separately allocated handle. `acquire`/`release` are then genuinely
/// no-ops instead of pretending to refcount something with a single owner.
fn handle() -> *mut c_void {
    WINDOW.get().map_or(std::ptr::null_mut(), |w| w as *const HostWindow as *mut c_void)
}

fn as_window(p: *mut c_void) -> Option<&'static HostWindow> {
    (!p.is_null()).then(|| WINDOW.get()).flatten()
}

extern "C" fn native_window_from_surface(_env: *mut c_void, _surface: *mut c_void) -> *mut c_void {
    // Cordial's Java `Surface` has no state of its own: there is one window and
    // the Surface object exists only so `onSurfaceCreatedNative`'s signature can
    // be satisfied. Returning the single window is therefore correct rather than
    // a simplification.
    let w = handle();
    // The returned pointer is traced, not just the call. A null here means the
    // engine was handed nothing to render into and every later step will fail
    // for a reason that looks unrelated — and "the Surface has no native peer"
    // is exactly the kind of plausible diagnosis that has been wrong before on
    // this engine. Printing the value settles it instead of inviting the guess.
    super::trace(format_args!("ANativeWindow_fromSurface -> {w:?}"));
    w
}

extern "C" fn native_window_acquire(window: *mut c_void) {
    let _ = window;
}

extern "C" fn native_window_release(window: *mut c_void) {
    let _ = window;
}

extern "C" fn native_window_get_width(window: *mut c_void) -> i32 {
    as_window(window).map_or(0, |w| w.geometry().0)
}

extern "C" fn native_window_get_height(window: *mut c_void) -> i32 {
    as_window(window).map_or(0, |w| w.geometry().1)
}

extern "C" fn native_window_get_format(window: *mut c_void) -> i32 {
    as_window(window).map_or(0, |w| w.geometry().2)
}

/// The engine states the buffer size and format it wants. Android resizes the
/// underlying buffers; here the values are recorded and reported back, because
/// the EGL surface is sized by the X window and the engine only needs the two to
/// agree.
extern "C" fn native_window_set_buffers_geometry(
    window: *mut c_void,
    width: i32,
    height: i32,
    format: i32,
) -> i32 {
    let Some(w) = as_window(window) else {
        return -22; // -EINVAL
    };
    let mut g = w.buffers.lock().unwrap_or_else(|e| e.into_inner());
    // Zero means "whatever the window is", per the API.
    if width > 0 {
        g.width = width;
    }
    if height > 0 {
        g.height = height;
    }
    if format > 0 {
        g.format = format;
    }
    0
}

/// Direct software access to the window's pixels.
///
/// Roblox renders through GLES, so this is not on its path. Returning an error
/// rather than a fake buffer is deliberate: a caller that gets a buffer will
/// write to it and expect the result on screen, and silently discarding that
/// would be far harder to diagnose than a refused lock.
extern "C" fn native_window_lock(
    _window: *mut c_void,
    _buffer: *mut c_void,
    _dirty: *mut c_void,
) -> i32 {
    -38 // -ENOSYS
}

extern "C" fn native_window_unlock_and_post(_window: *mut c_void) -> i32 {
    -38 // -ENOSYS
}

/// `eglCreateWindowSurface`, with the native window translated.
///
/// Android's EGL takes an `ANativeWindow*`. The host's EGL, on X11, takes a
/// `Window` — an XID. Roblox naturally passes the `ANativeWindow*` Cordial
/// handed it through `ANativeWindow_fromSurface`, and Mesa read that pointer as
/// an XID and answered:
///
/// ```text
/// [FLog::SurfaceController] Mode 4 failed: Error creating context: eglCreateWindowSurface 3003
/// [FLog::SurfaceController] RenderView is NULL
/// ```
///
/// 3003 is `EGL_BAD_ALLOC`. Substituting the real window is the whole fix, and
/// it belongs here rather than in `glcount` because the translation is not
/// diagnostic — without it there is no surface at all, whether or not anyone
/// asked for call counts.
///
/// There is exactly one window in this runtime, so any pointer arriving here is
/// that window; the argument is replaced unconditionally rather than compared
/// against a handle that could only ever have one value.
extern "C" fn egl_create_window_surface(
    dpy: *mut c_void,
    config: *mut c_void,
    _native_window: *mut c_void,
    attribs: *mut c_void,
) -> *mut c_void {
    crate::android::glcount::CREATE_WINDOW_SURFACE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    let name = CString::new("eglCreateWindowSurface").unwrap_or_default();
    // SAFETY: RTLD_DEFAULT; libEGL is in the global scope by the time the engine
    // reaches this call.
    let f = unsafe { dlsym(std::ptr::null_mut(), name.as_ptr()) };
    if f.is_null() {
        return std::ptr::null_mut();
    }
    type Fn_ = extern "C" fn(*mut c_void, *mut c_void, c_ulong, *mut c_void) -> *mut c_void;
    // SAFETY: resolved from the host for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let win = current().map(|w| w.egl_native_window()).unwrap_or(0);
    f(dpy, config, win, attribs)
}

/// `eglSwapInterval`, with the requested interval clamped to 0.
///
/// The engine asks for `eglSwapInterval(1)` — see the `[FLog::Graphics]` log
/// line of that exact text right after `EGL_MIN_SWAP_INTERVAL: 0`. Honouring
/// that request is what produces the ~1 fps GLES fallback: measured directly
/// (wrapping `eglSwapBuffers` with a timer around the real call), every swap
/// blocks for 0.97-1.00s, not the ~16ms a 60Hz vblank wait should take. That
/// number is too round to be a real refresh interval and stayed exactly 1.00s
/// whether or not the window had input focus (`_NET_ACTIVE_WINDOW` sent by
/// hand made no difference — focus was already ruled out at the Android level
/// separately). Setting the Mesa debug knob `vblank_mode=0` in the process
/// environment makes the block disappear entirely (swaps return in under a
/// millisecond), which isolates the cause to Mesa's DRI3/Present vblank wait,
/// not to Cordial's window, the compositor, or the engine's own pacing.
///
/// The reachable explanation: this host's X server is Xwayland (rootless,
/// under Mutter), which does not own a CRTC and cannot answer DRI3's
/// `GetMSC`/`Present` vblank queries the way a real Xorg/DRM master would.
/// When Mesa's `loader_dri3` can't get real MSC/vblank data it falls back to
/// pacing swaps against a synthetic interval rather than failing outright —
/// on this host that fallback lands on exactly 1 Hz. Vulkan's presentation
/// engine does not go through this code path at all (its own WSI, not GLX/
/// EGL's DRI3 loader), which is why the same host presents at a steady ~27
/// fps over `vkQueuePresentKHR` while GLES stalls on `eglSwapBuffers`.
///
/// Rather than exporting the Mesa env var — which would blanket-disable vsync
/// for every GL/EGL user in the process, including the diagnostic probes in
/// `gl.rs` — the fix is scoped to exactly the call the engine makes: force
/// the interval Mesa actually receives to 0. `eglSwapBuffers` then returns as
/// soon as the frame is submitted instead of waiting on a vblank source this
/// host cannot supply. The engine still paces itself (its own `RenderJob`
/// timing, the same mechanism that limits the Vulkan path to ~27 fps rather
/// than an unthrottled spin), so this does not hand the engine a runaway
/// framerate — it removes an extra, broken 1-Hz throttle underneath that
/// pacing, on top of it.
extern "C" fn egl_swap_interval(dpy: *mut c_void, _interval: c_int) -> u32 {
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    let name = CString::new("eglSwapInterval").unwrap_or_default();
    // SAFETY: RTLD_DEFAULT; libEGL is in the global scope by the time the
    // engine reaches this call.
    let f = unsafe { dlsym(std::ptr::null_mut(), name.as_ptr()) };
    if f.is_null() {
        return 0;
    }
    type Fn_ = extern "C" fn(*mut c_void, c_int) -> u32;
    // SAFETY: resolved from the host for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    f(dpy, 0)
}

// The `NativeInputInterface` natives, the text-entry state machine, and
// `set_input_natives` itself have all moved to `input.rs` — see its module
// doc. `dispatch_key`, above, calls back into them by name.

pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("ANativeWindow_fromSurface", native_window_from_surface),
        f!("ANativeWindow_acquire", native_window_acquire),
        f!("ANativeWindow_release", native_window_release),
        f!("ANativeWindow_getWidth", native_window_get_width),
        f!("ANativeWindow_getHeight", native_window_get_height),
        f!("ANativeWindow_getFormat", native_window_get_format),
        f!("ANativeWindow_setBuffersGeometry", native_window_set_buffers_geometry),
        f!("ANativeWindow_lock", native_window_lock),
        f!("ANativeWindow_unlockAndPost", native_window_unlock_and_post),
        f!("eglCreateWindowSurface", egl_create_window_surface),
        f!("eglSwapInterval", egl_swap_interval),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_lock_is_wanted_for_any_of_its_three_independent_reasons() {
        // Nothing asking: no lock, and the latch has nothing to hold.
        assert_eq!(pointer_lock_decision(false, 0, false, false, false), (false, false));
        // The engine's own request, alone.
        assert_eq!(pointer_lock_decision(true, 0, false, false, false), (true, false));
        // A camera-button drag, alone -- and blocked by CORDIAL_NO_DRAG_LOCK.
        assert_eq!(pointer_lock_decision(false, BUTTON_SECONDARY, false, false, false), (true, false));
        assert_eq!(pointer_lock_decision(false, BUTTON_SECONDARY, true, false, false), (false, false));
        // The primary button is not a camera button and must not arm the lock.
        assert_eq!(pointer_lock_decision(false, BUTTON_PRIMARY, false, false, false), (false, false));
        // The forced override, alone.
        assert_eq!(pointer_lock_decision(false, 0, false, true, false), (true, false));
    }

    #[test]
    fn the_escape_latch_holds_until_nothing_is_asking_any_more() {
        // Escape has just suppressed the lock, but the engine (or a held
        // camera button) is still asking for it the same pump: the latch
        // must hold, not be immediately overridden.
        assert_eq!(pointer_lock_decision(true, 0, false, false, true), (false, true));
        // The ask has stopped -- the button was released, the engine let go
        // -- so the latch clears and a later ask can re-arm the lock.
        assert_eq!(pointer_lock_decision(false, 0, false, false, true), (false, false));
    }

    #[test]
    fn the_warp_echo_is_swallowed_and_real_motion_is_not() {
        // The synthetic MotionNotify this backend's own XWarpPointer produces
        // lands exactly on the capture centre and must be dropped, or every
        // recentring warp would report itself as a fresh delta.
        assert_eq!(locked_pointer_delta((640, 360), (640, 360), true), None);
        // The same coincidence with the latch already spent (a previous
        // frame consumed the echo) is real motion, not another echo.
        assert_eq!(locked_pointer_delta((640, 360), (640, 360), false), Some((0, 0)));
        // Ordinary motion away from centre, latch armed or not, is never
        // swallowed -- only an exact match does that.
        assert_eq!(locked_pointer_delta((645, 358), (640, 360), true), Some((5, -2)));
        assert_eq!(locked_pointer_delta((645, 358), (640, 360), false), Some((5, -2)));
    }

    #[test]
    fn only_the_last_expose_in_a_batch_triggers_a_redraw() {
        // A window manager delivering several damaged rectangles as one
        // repaint sets `count` to how many more follow; firing on every one
        // of them would mean N redraw requests for a single exposure.
        assert!(!is_final_expose(3));
        assert!(!is_final_expose(1));
        assert!(is_final_expose(0));
    }

    #[test]
    fn the_wheel_pseudo_buttons_are_not_clicks() {
        // X11 has no wheel; it has buttons 4-7. Letting them fall through to
        // `x11_button_to_android` is how a scroll would arrive as some button
        // press, and the two tables have to stay disjoint for that not to
        // happen — hence both assertions, not just the wheel one.
        for b in 4..=7 {
            assert!(x11_button_to_wheel(b).is_some(), "button {b} is the wheel");
            assert!(x11_button_to_android(b).is_none(), "button {b} must not also be a click");
        }
        for b in 1..=3 {
            assert!(x11_button_to_wheel(b).is_none(), "button {b} is a click, not the wheel");
        }
        assert_eq!(x11_button_to_android(8), Some(BUTTON_BACK));
        assert_eq!(x11_button_to_android(9), Some(BUTTON_FORWARD));
        // Up is positive, matching MotionEvent.AXIS_VSCROLL, and one X11
        // pseudo-button is exactly one detent — no conversion to get wrong.
        assert_eq!(x11_button_to_wheel(4), Some((0.0, 1.0)));
        assert_eq!(x11_button_to_wheel(5), Some((0.0, -1.0)));
        assert_eq!(x11_button_to_wheel(6), Some((-1.0, 0.0)));
        assert_eq!(x11_button_to_wheel(7), Some((1.0, 0.0)));
    }

    #[test]
    fn input_event_mask_watches_for_expose() {
        // ExposureMask (0x8000, X.h) is what makes a damaged window generate
        // `Expose` at all — without it in the mask `open()` passes to
        // `XSelectInput`, `onSurfaceRedrawNeededNative` would never have
        // anything to react to. Checked against the real constant, not a
        // re-derived copy, so a future edit that drops the bit fails this
        // test rather than only failing silently against a live window
        // manager.
        const EXPOSURE_MASK: c_long = 0x8000;
        assert_eq!(INPUT_EVENT_MASK & EXPOSURE_MASK, EXPOSURE_MASK);
        // The previously-driven input classes stay watched too — this is an
        // addition, not a replacement.
        const KEY_BUTTON_MOTION_MASK: c_long = 0x1 | 0x2 | 0x4 | 0x8 | 0x40;
        assert_eq!(
            INPUT_EVENT_MASK & KEY_BUTTON_MOTION_MASK,
            KEY_BUTTON_MOTION_MASK
        );
    }

    #[test]
    fn wm_class_matches_the_desktop_entry() {
        // A capture tool, the taskbar and the portal picker all resolve a
        // window to its application by matching WM_CLASS against
        // StartupWMClass. When they disagree nothing errors — Cordial just
        // shows up in OBS and GNOME as a nameless, iconless window, which is
        // exactly the kind of break nobody notices until a user reports it.
        // ADR-009 commits to this staying true, so it is checked rather than
        // asserted in prose.
        let desktop = include_str!("../../../../packaging/io.github.luohoa97.Cordial.desktop");
        let declared = desktop
            .lines()
            .find_map(|l| l.strip_prefix("StartupWMClass="))
            .expect("desktop entry declares StartupWMClass");
        assert_eq!(declared.trim(), WM_RES_CLASS);
    }
}
