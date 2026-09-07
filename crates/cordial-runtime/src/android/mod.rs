//! Cordial's implementation of the Android NDK APIs in `libandroid.so`.
//!
//! Thirty-two functions across four groups, all currently stubbed except assets:
//!
//! | Group | Functions | State |
//! |---|---|---|
//! | `AAsset*` | 6 | implemented — see [`asset`] |
//! | `ANativeWindow_*` | 10 | implemented over a host window — see [`window`]/[`wayland`] |
//! | `ALooper_*` | 7 | implemented over epoll — see [`looper`] |
//! | `AConfiguration_*` | 9 | implemented — see [`config`] |
//!
//! The order is not arbitrary: assets gate everything, because the engine cannot
//! load a shader or a font without them. See docs/design/path-to-a-frame.md.

pub mod accessibility;
pub mod asset;
pub mod capture;
pub mod clipboard;
pub mod config;
pub mod editor_font;
pub mod frame_pacing;
pub mod gamepad;
pub mod gl;
pub mod glcount;
pub mod input;
pub mod looper;
pub mod system;
pub mod vulkan;
pub mod wayland;
pub mod window;

use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static TRACE: AtomicBool = AtomicBool::new(false);

/// Log every Android API call. AGDK's `initializeNativeCode` returns a bare 0 on
/// failure with nothing logged, so the only way to find where it stopped is to
/// watch which of these it reached.
pub fn set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

pub(crate) fn trace(args: std::fmt::Arguments<'_>) {
    if TRACE.load(Ordering::Relaxed) {
        eprintln!("[android] {args}");
    }
}

// ------------------------------------------------------------ display backend
//
// ADR-011: Wayland is the target; X11 (via `window.rs`, over Xwayland where
// there is no real X server) stays in the tree as a diagnosable fallback while
// the Wayland path proves itself, not as a second supported configuration. See
// that ADR for why — the short version is that a resize on Xwayland cannot be
// made atomic no matter how this code is written, because the protocol has no
// way to express "the new content is ready"; `xdg_toplevel`'s configure/ack/
// commit sequence exists for exactly that.
//
// The choice is made once, from the environment, and then fixed for the life
// of the process — `open_window` and `overrides()` have to agree on it, and a
// value that could change after the window is open would let them disagree.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    X11,
    Wayland,
}

/// Which display backend this run uses. `WAYLAND_DISPLAY` set means a Wayland
/// compositor is available and is preferred; its absence means either a bare
/// Xorg session or an Xwayland-rootless host, and X11 is what both of those
/// actually offer a client. Reported unconditionally, the same way `window.rs`
/// always reports its window placement — "which backend Cordial picked" is
/// exactly the kind of fact a bug report needs and a trace flag would hide.
pub fn backend() -> Backend {
    static CHOSEN: OnceLock<Backend> = OnceLock::new();
    *CHOSEN.get_or_init(|| {
        // **This is the reversion the previous comment here promised.** It used
        // to require `CORDIAL_WAYLAND` as well as `WAYLAND_DISPLAY`, opt-in
        // "until `wayland.rs` is real", because preferring Wayland on the
        // presence of a compositor alone would have made the client refuse to
        // start for everyone the moment an unimplemented backend merged. The
        // backend is real now -- it is the one the shell has spawned every
        // client with since `launch.rs` started setting the variable
        // unconditionally, so it is also the only one with any recent mileage
        // on it. Preferring Wayland whenever a compositor is there is what
        // ADR-011 specifies, and the doc comment above this function has been
        // describing that behaviour rather than the code's for some time.
        //
        // What the opt-in actually cost, and why this is a bug fix rather than
        // a preference: the whole web view feature is inert on X11. The
        // presenter attaches an `AdwDialog` to the GTK host window, which only
        // the Wayland backend creates, so on X11 every openWindow the engine
        // sends -- Join, sign-in, Robux -- was dropped. A hand-run
        // `cordial-run`, which is the invocation AGENTS.md documents, took the
        // X11 path by default and therefore had no web views at all, while the
        // same build launched through `just dev` had them. That difference was
        // read as the web view being broken.
        //
        // `CORDIAL_X11=1` forces the old path back, because a reversion with no
        // escape hatch is how a regression becomes unreportable: anyone whose
        // session breaks on Wayland needs a way to say so from a working
        // client rather than from a bisect.
        let b = if std::env::var_os("CORDIAL_X11").is_some() {
            Backend::X11
        } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            Backend::Wayland
        } else {
            Backend::X11
        };
        match b {
            Backend::Wayland => {
                println!("[android] display backend: Wayland (set CORDIAL_X11=1 to force X11)")
            }
            Backend::X11 if std::env::var_os("CORDIAL_X11").is_some() => {
                println!("[android] display backend: X11 (forced by CORDIAL_X11; web views cannot be attached on this backend)")
            }
            Backend::X11 => println!(
                "[android] display backend: X11 (no WAYLAND_DISPLAY; web views cannot be attached on this backend)"
            ),
        }
        b
    })
}

/// The open window, whichever backend it came from. Callers that only need
/// `geometry()` — which is all `load.rs` wants from the return value of
/// [`open_window`] — can use this directly; callers that need the backend's
/// own native handles (EGL, Vulkan) go through [`window::current`] or
/// [`wayland::current`] instead, because those handles have different shapes
/// per backend and forcing them into one enum would just move the `match` from
/// here to there.
pub enum WindowHandle {
    X11(&'static window::HostWindow),
    Wayland(&'static wayland::WaylandWindow),
}

impl WindowHandle {
    pub fn geometry(&self) -> (i32, i32, i32) {
        match self {
            WindowHandle::X11(w) => w.geometry(),
            WindowHandle::Wayland(w) => w.geometry(),
        }
    }
}

/// Open the host window on whichever backend [`backend()`] selected.
pub fn open_window(width: u32, height: u32, title: &str) -> Result<WindowHandle, String> {
    match backend() {
        Backend::Wayland => wayland::open(width, height, title).map(WindowHandle::Wayland),
        Backend::X11 => window::open(width, height, title).map(WindowHandle::X11),
    }
}

/// The canvas in pixels, whichever backend is open, and `(0, 0)` before there
/// is one.
///
/// `nativePassInput` takes the surface size as two of its six arguments, and a
/// scripted touch has no `wl_touch` event to read it off the way
/// `wayland.rs`'s handlers do. Zero is what "there is no window" honestly looks
/// like: it is also a handle of 0, so nothing reaches the engine on that path
/// anyway and there is no wrong size to be believed.
pub fn canvas_size() -> (i32, i32) {
    let g = match backend() {
        Backend::Wayland => wayland::current().map(|w| w.geometry()),
        Backend::X11 => window::current().map(|w| w.geometry()),
    };
    g.map(|(w, h, _)| (w, h)).unwrap_or((0, 0))
}

/// The active backend's input connection descriptor, for the looper to watch
/// alongside the engine's own fds. `None` before a window has been opened.
pub fn connection_fd() -> Option<c_int> {
    match backend() {
        Backend::Wayland => wayland::current().map(|w| w.connection_fd()),
        Backend::X11 => window::current().map(|w| w.connection_fd()),
    }
}

/// TEMPORARY INSTRUMENTATION -- not for commit.
/// The physical size of the display this window is on, in millimetres.
///
/// `None` when nothing knows: no window yet, no toplevel, an X11 backend that
/// does not track it, or a compositor reporting no physical size for the
/// output -- which is ordinary for virtual and remote ones. The caller passes
/// zeroes on, and the engine reads those as null, which is a branch it already
/// has a handler for. See `DeviceUtils` in `native/init_params.cpp`.
pub fn display_physical_mm() -> Option<(i32, i32)> {
    match backend() {
        Backend::Wayland => wayland::display_physical_mm(),
        Backend::X11 => None,
    }
}

pub fn backend_instr_geometry() -> String {
    match backend() {
        Backend::Wayland => wayland::instr_geometry(),
        Backend::X11 => "x11".to_string(),
    }
}

/// TEMPORARY INSTRUMENTATION -- not for commit.
pub fn backend_set_fullscreen(on: bool) {
    match backend() {
        Backend::Wayland => wayland::instr_set_fullscreen(on),
        // X11 too, and only because a Wayland surface cannot be photographed
        // on this desktop — see `window::HostWindow::set_fullscreen`. A
        // fullscreen transition that can be seen on one backend is worth more
        // than one that can only be counted on the other.
        Backend::X11 => {
            if let Some(w) = window::current() {
                w.set_fullscreen(on);
            }
        }
    }
}

/// Close the window as the close button would, from a scripted run. See
/// [`wayland::instr_close_window`] for why this is a fair test of the real
/// close path and not a shortcut past it.
pub fn backend_close_window() {
    if let Backend::Wayland = backend() {
        wayland::instr_close_window();
    }
}

/// Whether the user has closed the engine's window, for the active backend.
///
/// The X11 backend answers `false` unconditionally rather than growing a second
/// implementation of this: ADR-011 makes X11 the diagnostic fallback, and a
/// closed window there still ends the way it always has, on `--run`.
pub fn window_closed() -> bool {
    match backend() {
        Backend::Wayland => wayland::window_closed(),
        Backend::X11 => false,
    }
}

/// Whether the engine's window currently has focus, for the active backend.
///
/// `None` means "this backend does not know", which is not the same answer as
/// `Some(false)` and must not be collapsed into one: the caller drives
/// `onWindowFocusChangedNative` off this, and telling the engine it has lost
/// focus because nothing was watching would throttle a window the user is
/// looking at. X11 answers `None` for that reason — `window.rs` selects no
/// `FocusChangeMask` and ADR-011 makes X11 the diagnostic fallback, so it keeps
/// the behaviour it has always had rather than growing a second implementation
/// of this.
/// **A window that is not visible does not have focus, whatever the compositor
/// says about activation.**
///
/// Measured over 15 runs: in two of them mutter reported `FOCUSED | SUSPENDED`
/// -- covered, not being drawn, still activated -- for 20 s and 7 s. Throughout,
/// this returned `true`, no `onWindowFocusChangedNative(false)` was sent, and
/// the engine's looper thread spun at 8.5-10 M `ALooper_pollOnce` calls a second
/// for 105% of a core. That is the whole of the "idle at 100% while unfocused"
/// report, and it is the one part of that spin which is ours.
///
/// Folding visibility in is *more* faithful to Android rather than less: an
/// activity that is not visible is stopped and does not hold window focus, so
/// an engine written against that lifecycle is being told the truth by this and
/// was being told something Android would never say by the version before it.
///
/// `None` from either signal still means "not known" and is not collapsed into
/// `Some(false)` -- telling the engine it lost focus because nothing was
/// watching would throttle a window the user is looking at. Only a definite
/// `Some(false)` from visibility overrides a definite `Some(true)` from focus.
/// X11 answers `None` for both and keeps the behaviour it has always had.
pub fn backend_focused() -> Option<bool> {
    // `CORDIAL_REPORT_FOCUS=0` stops telling the engine about focus at all,
    // which is a control rather than a preference.
    //
    // Reporting real focus transitions is recent and was added for a good
    // reason: before it, `onWindowFocusChangedNative` reached the engine
    // exactly twice a session, so Roblox believed it was focused the whole
    // time and kept simulating at full rate behind whatever you had switched
    // to. See `looper::pump`'s own comment.
    //
    // But a user reports that clicking Join and alt-tabbing away *before the
    // game finishes loading* leaves WASD dead for the rest of the session,
    // while the menu and Shift+F5 still work -- and that the same test on
    // Sober does not reproduce. Sober is the interesting half: it is the same
    // engine on the same machine, and the difference we know of is that it
    // almost certainly never reports focus loss, because that is where Cordial
    // was until recently too.
    //
    // Everything else plausible has been ruled out by measurement rather than
    // argument: the engine imports no `AConfiguration_getKeyboard`, jnivm's
    // gap register says "nothing went unanswered this run" for a full session,
    // and `init_params.cpp` already reports the desktop profile Roblox would
    // want -- `kTouchscreenNoTouch`, `kKeyboardQwerty`, `kHardKeyboardHiddenNo`.
    // So the engine is not missing a capability and is not missing a method.
    //
    // Returning `None` here is exactly "not known", which the caller already
    // treats as leave-the-last-state-alone, so this reproduces the old
    // behaviour precisely rather than approximating it. That makes it a real
    // A/B: same binary, same session, flip the variable.
    //
    // **This is a diagnostic, not the fix.** If it turns out to be the cause,
    // the answer is to stop reporting focus loss *while the game is still
    // loading* rather than to stop reporting it at all -- giving up the CPU
    // saving to dodge a load-time race would be trading one real bug for
    // another.
    if !report_focus() {
        return None;
    }
    let focused = match backend() {
        Backend::Wayland => wayland::focused(),
        Backend::X11 => None,
    };
    // **Visibility does not answer the focus question, and conflating them
    // killed clients.**
    //
    // This used to read `(Some(true), Some(false)) => Some(false)`: a window the
    // compositor said was focused but not visible was reported to the engine as
    // unfocused. `visible()` is `xdg_toplevel`'s SUSPENDED or MINIMIZED, and the
    // intent was to stop simulating behind something -- reasonable, and wrong in
    // one specific way that matters more than the saving.
    //
    // Focus is only ever reported on a *transition*. So a `false` manufactured
    // from a SUSPENDED that the compositor never clears can never be taken back:
    // the engine gets APP_CMD_LOST_FOCUS, stops drawing, and no later
    // APP_CMD_GAINED_FOCUS is ever sent because, as far as this function is
    // concerned, nothing changed. The window stays on screen, plainly focused,
    // and dead.
    //
    // That is what the engine's own log shows on a client frozen for eleven
    // minutes -- found by reading `appData/logs/*_Player_*.log`, which Cordial
    // writes every run and which nothing here had ever looked at:
    //
    //     t=13.08  nativeActivity_onSurfaceChanged: state:7
    //     t=13.09  APP_CMD_WINDOW_REDRAW_NEEDED
    //     t=17.33  APP_CMD_LOST_FOCUS
    //     (then six hundred seconds of nothing but flag-cache timers)
    //
    // The 17 s matters: presents drop to the 1.0/s idle throttle at about
    // thirteen seconds, so the loss lands on an already-quiet engine and takes
    // it to zero. An earlier scripted test that dropped focus at 3.9 s appeared
    // to disprove all of this, because at 3.9 s the engine is still in its 60/s
    // startup phase and carries on drawing regardless. The artificial test was
    // measuring a different moment than the bug.
    //
    // So focus now means focus. An occluded window may keep simulating, which
    // costs CPU that was worth reclaiming; a window that can never be told it
    // has focus again costs the whole session. `backend_visible` is unchanged
    // and still available to the pump's own throttle, which is where a
    // visibility policy belongs.
    focused
}

/// Whether to tell the engine about focus changes at all. On by default.
///
/// Read once and cached: the answer cannot change within a run, and this sits
/// on the pump's per-tick path where a `getenv` would be paid millions of times
/// for a value that never moves.
fn report_focus() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let off = matches!(std::env::var("CORDIAL_REPORT_FOCUS").as_deref(), Ok("0") | Ok("off"));
        if off {
            println!(
                "[android] CORDIAL_REPORT_FOCUS=0: the engine will never be told the window lost \
                 focus. It will keep simulating at full rate while you are switched away, which \
                 is what this costs. Set to compare against the default when movement keys stop \
                 working after alt-tabbing during a load."
            );
        }
        !off
    })
}

/// Whether the engine's window is visible, for the active backend.
///
/// `None` means "not known", on the same footing and for the same reason as
/// [`backend_focused`]. X11 answers `None`: `window.rs` tracks no visibility
/// and a `_NET_WM_STATE_HIDDEN` reader would be a second implementation of
/// something ADR-011 makes the fallback.
pub fn backend_visible() -> Option<bool> {
    match backend() {
        Backend::Wayland => wayland::visible(),
        Backend::X11 => None,
    }
}

/// Drain and deliver whatever host input is queued, for the active backend.
pub fn pump_input_events(handle: i64) {
    match backend() {
        Backend::Wayland => wayland::pump_input_events(handle),
        Backend::X11 => window::pump_input_events(handle),
    }
}

/// Everything the Android layer implements so far.
pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    let mut v = asset::overrides();
    v.extend(config::overrides());
    v.extend(looper::overrides());
    // Only the selected backend's ANativeWindow_*/EGL overrides are
    // registered — registering both would mean whichever one is NOT hosting
    // the window still answers `ANativeWindow_getWidth` and the rest with
    // whatever its own (unopened) window's defaults are, which is a wrong
    // answer that looks like a right one.
    v.extend(match backend() {
        Backend::Wayland => wayland::overrides(),
        Backend::X11 => window::overrides(),
    });
    if std::env::var_os("CORDIAL_COUNT_GL").is_some() {
        v.extend(glcount::overrides());
    }
    v
}
