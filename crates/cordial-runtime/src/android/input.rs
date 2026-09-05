//! Display-server-independent input plumbing, shared by [`super::window`] (X11)
//! and [`super::wayland`].
//!
//! X11 delivers keysyms directly. Wayland delivers raw evdev keycodes plus an
//! xkb keymap the client has to interpret itself — see ADR-011. Everything
//! *below* that difference is identical: both backends end up with a keysym, a
//! button number, or committed text, and from there the two paths converge.
//! This module is that convergence point. It used to live inside `window.rs`,
//! written for the only backend that existed; the text-entry state machine in
//! particular (`TextField`, the caret arithmetic, the reseed-on-focus-change
//! logic) took real iteration to get right — see the tests below — and a
//! second display backend is exactly the situation duplicating it would have
//! caused a second, silently-diverging copy of the same bugs to be fixed twice.
//!
//! What stays behind in each backend is the part that is genuinely
//! display-specific: opening a connection, reading its events, and turning
//! them into the keysym/button/text vocabulary this module speaks.

use std::ffi::{c_ulong, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

// --------------------------------------------------------- Android vocabulary
//
// `android.view.MotionEvent`/`KeyEvent` constants both backends synthesise
// events against, via `deliver_mouse`/`deliver_key` below.

pub const BUTTON_PRIMARY: i32 = 1;
pub const BUTTON_SECONDARY: i32 = 2;
pub const BUTTON_TERTIARY: i32 = 4;
pub const BUTTON_BACK: i32 = 8;
pub const BUTTON_FORWARD: i32 = 16;

/// `nativePassMouseButton`'s own button index, which is not Android's bitmask.
///
/// Only one value here is established: 0 is the left button, because that is
/// what Cordial has always sent and clicking Roblox's interface works. The
/// other two are `INFERRED` from Roblox's own `Enum.UserInputType`, where
/// `MouseButton1`/`2`/`3` are left/right/middle in that order — a zero-based
/// index whose 0 is the left button is that enum minus one. The dex declares
/// the parameter as a bare `I` and strips parameter names, so nothing readable
/// settles it; a human with a mouse does, in one click.
///
/// `None` means Android has a button the GameActivity path can represent but
/// this native interface has no established ordinal for. Getting that wrong is
/// not silent: a side button would act as a primary click rather than doing
/// nothing.
pub fn roblox_mouse_button(android_button: i32) -> Option<i32> {
    match android_button {
        BUTTON_PRIMARY => Some(0),
        BUTTON_SECONDARY => Some(1),
        BUTTON_TERTIARY => Some(2),
        // Android has well-defined back and forward button bits, and the
        // GameActivity path below carries those bits directly. The native
        // interface instead takes a small ordinal whose meaning past middle
        // is not established, so treating either as left would be a false
        // click. Leave that path out until a run establishes its mapping.
        BUTTON_BACK | BUTTON_FORWARD => None,
        _ => None,
    }
}
pub const ACTION_DOWN: i32 = 0;
pub const ACTION_UP: i32 = 1;
pub const ACTION_MOVE: i32 = 2;
pub const ACTION_HOVER_MOVE: i32 = 7;
pub const ACTION_BUTTON_PRESS: i32 = 11;
pub const ACTION_BUTTON_RELEASE: i32 = 12;
/// `ACTION_SCROLL`. Not delivered by [`deliver_mouse`] — a scroll carries no
/// button state and no gesture start, so it has its own call; see
/// [`deliver_scroll`].
pub const ACTION_SCROLL: i32 = 8;

// `android.view.KeyEvent.META_*`.
pub const META_SHIFT_ON: i32 = 1;
pub const META_ALT_ON: i32 = 2;
pub const META_CTRL_ON: i32 = 0x1000;
pub const META_CAPS_LOCK_ON: i32 = 0x100000;

/// A pragmatic subset of keysyms mapped to `android.view.KeyEvent.KEYCODE_*`.
///
/// The values are X11's `keysymdef.h` numbering, but that numbering is not an
/// X11 peculiarity — it is the shared keysym space `xkbcommon` also uses (the
/// "xkb" in the name is literally "X Keyboard extension"), which is what makes
/// this table usable from both backends rather than needing a second one keyed
/// on evdev codes. Covers what a desktop text field and basic UI navigation
/// need — letters, digits, common punctuation, arrows, and the usual control
/// keys. Anything outside this set is dropped rather than guessed at.
pub fn keysym_to_android(keysym: c_ulong) -> Option<i32> {
    let k = keysym as u32;
    Some(match k {
        0x30..=0x39 => 7 + (k - 0x30) as i32,  // 0..9 -> AKEYCODE_0..9
        0x61..=0x7a => 29 + (k - 0x61) as i32, // a..z -> AKEYCODE_A..Z
        0x41..=0x5a => 29 + (k - 0x41) as i32, // A..Z (shifted) -> the same keycodes
        // F1..F12. `XK_F1` is 0xffbe and `AKEYCODE_F1` is 131, both
        // contiguous, so one range covers the row. Absent until now, and their
        // absence dropped the whole function row before it reached the engine
        // -- see `dispatch_key`, where the evdev call used to sit inside the
        // `if let` this returns `None` to.
        0xffbe..=0xffc9 => 131 + (k - 0xffbe) as i32,
        0x0020 => 62,                          // space
        0xff0d | 0xff8d => 66,                 // Return, KP_Enter
        0xff08 => 67,                          // BackSpace
        0xff09 => 61,                          // Tab
        0xff1b => 111,                         // Escape
        0xff51 => 21,                          // Left
        0xff52 => 19,                          // Up
        0xff53 => 22,                          // Right
        0xff54 => 20,                          // Down
        0xffe1 => 59,                          // Shift_L
        0xffe2 => 60,                          // Shift_R
        0xffe3 => 113,                         // Control_L
        0xffe4 => 114,                         // Control_R
        0xffe9 => 57,                          // Alt_L
        0xffea => 58,                          // Alt_R
        0xffe5 => 115,                         // Caps_Lock
        0xffff => 112,                         // Delete (forward delete)
        0xff50 => 122,                         // Home
        0xff57 => 123,                         // End
        0xff55 => 92,                          // Page_Up
        0xff56 => 93,                          // Page_Down
        0xff63 => 124,                         // Insert
        0x002c => 55,                          // comma
        0x002e => 56,                          // period
        0x002f => 76,                          // slash
        0x003b => 74,                          // semicolon
        0x0027 => 75,                          // apostrophe
        0x0060 => 68,                          // grave
        0x002d => 69,                          // minus
        0x003d => 70,                          // equal
        0x005b => 71,                          // bracketleft
        0x005d => 72,                          // bracketright
        0x005c => 73,                          // backslash
        _ => return None,
    })
}

/// Say that a native the input path wanted is not there — at the first drop,
/// and then at each power of ten.
///
/// "Not there" covers both ways it happens: an AGDK native that
/// `initializeNativeCode` has not put in the natives table, and a
/// `NativeInputInterface`/`NativeGLInterface` export the loader could not
/// resolve. Both end the same way, with an input event going nowhere.
///
/// `Ok(None)` used to be silent everywhere in this file, on the grounds that a
/// call arriving before `initializeNativeCode` has finished is a normal startup
/// race. The cost of that silence was measured the hard way: a session run with
/// `CORDIAL_ANDROID_TRACE=1`, pressing keys in an experience, printed no
/// `onKeyDownNative` line at all — and "the trace said nothing" and "the engine
/// never received the key" were the same observation, with no way to tell them
/// apart without changing the code first.
///
/// Not once, and not per event. Once is indistinguishable from a startup race;
/// per event would bury the log under one line per keystroke. At decade
/// boundaries a race prints a single line and a native that never registers
/// keeps coming back, which is the distinction that was missing.
///
/// Deliberately not behind `CORDIAL_ANDROID_TRACE`: input being dropped on the
/// floor is not tracing, and the one run where it mattered had the flag on and
/// still learned nothing.
pub(crate) fn report_unregistered(name: &'static str) {
    crate::unimplemented::record(crate::unimplemented::Kind::NativeNotRegistered, name);
    static DROPPED: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());
    let n = {
        let mut seen = DROPPED.lock().unwrap_or_else(|e| e.into_inner());
        match seen.iter_mut().find(|(k, _)| *k == name) {
            Some((_, count)) => {
                *count += 1;
                *count
            }
            None => {
                seen.push((name, 1));
                1
            }
        }
    };
    let decade = {
        let mut d = 1u64;
        while d < n {
            d = d.saturating_mul(10);
        }
        d == n
    };
    if decade {
        eprintln!(
            "[android] {name} is not registered in the natives table (or was not \
             resolved at load); {n} input event(s) dropped so far, reported at \
             each power of ten. A single line early in startup is the normal \
             race against initializeNativeCode; a line that keeps returning is not."
        );
    }
}

/// Deliver one **mouse** event through AGDK's `onTouchEventNative`.
///
/// Called `deliver_touch` until the touch path arrived and made the name a
/// liability: this is the pipe a `wl_pointer` or an X11 pointer event takes,
/// and the `MotionEvent` it builds reports `SOURCE_MOUSE`/`TOOL_TYPE_MOUSE`.
/// Fingers go to [`touch_down`] and friends instead, and reach the same native
/// through [`cordial_linker_sys::game_activity::touch_multi`]. The Android
/// native has one name for both because on a phone the APK's own Java decides
/// which is which before calling it; here that decision is Cordial's, and these
/// two functions are where it is written down.
#[allow(clippy::too_many_arguments)]
pub fn deliver_mouse(
    handle: i64,
    action: i32,
    x: f32,
    y: f32,
    button_state: i32,
    action_button: i32,
    event_time_ms: i64,
    down_time_ms: i64,
) {
    if no_agdk_touch() {
        return;
    }
    match cordial_linker_sys::game_activity::touch(
        handle,
        action,
        x,
        y,
        button_state,
        action_button,
        event_time_ms,
        down_time_ms,
    ) {
        Ok(Some(consumed)) => {
            super::trace(format_args!("onTouchEventNative(action={action}) -> {consumed}"))
        }
        Ok(None) => report_unregistered("onTouchEventNative"),
        Err(e) => super::trace(format_args!("onTouchEventNative(action={action}) failed: {e}")),
    }
}

/// Deliver one AGDK wheel event as `ACTION_SCROLL` with the scroll axes filled.
///
/// Private, and reached only through [`wheel`], so that the sign and scale
/// policy cannot be applied to one of the two wheel paths and not the other.
fn deliver_scroll(handle: i64, x: f32, y: f32, hscroll: f32, vscroll: f32, event_time_ms: i64) {
    if no_agdk_touch() {
        return;
    }
    match cordial_linker_sys::game_activity::scroll(handle, x, y, hscroll, vscroll, event_time_ms) {
        Ok(Some(consumed)) => super::trace(format_args!(
            "onTouchEventNative(ACTION_SCROLL h={hscroll} v={vscroll}) -> {consumed}"
        )),
        Ok(None) => report_unregistered("onTouchEventNative"),
        Err(e) => super::trace(format_args!("onTouchEventNative(ACTION_SCROLL) failed: {e}")),
    }
}

// --------------------------------------------------------- device identity
//
// **The engine discriminates input by which native is called, not by anything
// in a payload.** `nativePassInput` is a finger, `nativePassMouseMove` and
// `nativePassMouseButton` are a mouse, `nativePassKeyEvent` is a keyboard, and
// none of the four carries a source, a tool type or a device id -- the device
// identity *is* the function name. On a real phone the APK's own Java reads
// `MotionEvent.getSource()`/`getToolType()` and routes accordingly; Cordial
// replaces that Java, so the routing is ours to do, per event.
//
// Which is what makes it reversible. `UserInputService.PreferredInput` and
// `LastInputType` follow the last device used and change back the moment the
// other one is touched, which is the behaviour a phone with a keyboard cover
// has. (`TouchEnabled`/`KeyboardEnabled`/`MouseEnabled` are a different signal
// -- a capability latch that goes true on first use and is never revoked. Sober
// #1577, a hybrid laptop stuck in mobile controls, is a game reading the latch.
// Nothing on this side can un-latch it, and nothing here tries to.)
//
// So there is no mode. Every entry point below names the device it is for, and
// the backends call the one matching the device the event arrived on.

/// The device Cordial claims for input it produced itself.
///
/// Scripted input -- [`script_click`], and the development control surface's
/// `move`/`click`/`down`/`up` in `devctl.rs` -- has no device behind it, so
/// something has to choose one. This is that choice, and it is the *only* thing
/// `CORDIAL_INPUT_TOUCH` still changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntheticDevice {
    Mouse,
    Finger,
}

/// `CORDIAL_INPUT_TOUCH`, which used to be a global mode and is now an override
/// of two defaults.
///
/// It was a `static const bool` in `native/game_activity.cpp` that seeded
/// `MotionEvent.source` and `toolType` for the whole process, so setting it
/// relabelled a real mouse as a finger for the life of the session -- the
/// session-wide decision this file no longer makes anywhere. What it overrides
/// now:
///
/// - what [`synthetic_device`] answers, so a machine with no touchscreen can
///   still drive the touch path -- through the MCP's own `cordial_click`, which
///   is how this client is driven anyway (see AGENTS.md), and not by hijacking
///   the developer's mouse;
/// - what [`report_touchscreen`] tells the engine `PlatformParams.isTouchDevice`
///   is.
///
/// Three states on purpose. `1` forces a touchscreen the host has not got, `0`
/// forces it off on a host that has one -- which is what a hybrid-laptop user
/// who wants the desktop interface asks for, and Sober #1577 is that request
/// arriving as a bug report -- and unset lets the seat answer. A real mouse
/// event is a mouse event under all three.
fn input_touch_override() -> Option<bool> {
    static OVERRIDE: OnceLock<Option<bool>> = OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        let raw = std::env::var_os("CORDIAL_INPUT_TOUCH");
        parse_touch_override(raw.map(|v| v.to_string_lossy().into_owned()))
    })
}

/// The three states, separated from the environment so the table is testable.
///
/// Pure for the reason [`keepalive_wanted`] is: the interesting cases here are
/// the ones nobody sets deliberately -- an empty value, and a `0` that used to
/// mean the same as unset and now does not.
fn parse_touch_override(value: Option<String>) -> Option<bool> {
    let v = value?;
    let v = v.trim().to_ascii_lowercase();
    // An empty value is "unset with extra steps" -- what a shell that expanded
    // a variable to nothing produces -- and reading it as `false` would
    // silently disable a real touchscreen for somebody who meant to say
    // nothing at all.
    if v.is_empty() {
        return None;
    }
    Some(!matches!(v.as_str(), "0" | "false" | "no" | "off"))
}

/// What the engine is told, given the seat, the off switch and the override.
///
/// Separate from [`report_touchscreen`] because that one talks to the engine
/// and this one is the whole decision. Reading them apart is also how the
/// precedence stays reviewable: `CORDIAL_NO_TOUCH` beats `CORDIAL_INPUT_TOUCH`
/// beats the seat, and the first of those is not a preference but a fact --
/// with it set nothing can reach either touch native, so a `true` here would be
/// a stub lying about a device that cannot produce an event.
fn touchscreen_reported(seat_has_touch: bool, no_touch: bool, over: Option<bool>) -> bool {
    if no_touch {
        return false;
    }
    over.unwrap_or(seat_has_touch)
}

/// Which device scripted input claims to be. See [`input_touch_override`].
pub fn synthetic_device() -> SyntheticDevice {
    // `CORDIAL_NO_TOUCH=1` wins, and has to: it means nothing reaches either
    // touch native, so routing scripted input to them would drop it in silence
    // rather than deliver it as a finger.
    if no_touch() {
        return SyntheticDevice::Mouse;
    }
    match input_touch_override() {
        Some(true) => SyntheticDevice::Finger,
        _ => SyntheticDevice::Mouse,
    }
}

/// The platform contact id scripted touches use.
///
/// `i64::MIN` rather than a small number because the tracker keys on whatever
/// id the compositor hands out and `wl_touch`'s is an `int32_t` -- so any value
/// a real contact could take is a value a scripted one could collide with, and
/// a collision means one gesture wearing another's pointer id. Nothing outside
/// `i32` can arrive from `wl_touch`.
const SYNTHETIC_CONTACT: i64 = i64::MIN;

/// Tell the engine whether this host has a touchscreen, before it asks.
///
/// Called from each display backend's `open()` with what the seat advertised.
/// The engine reads `PlatformParams.isTouchDevice` -- twice, on a cold start,
/// measured -- and it is the only one of `isTouchDevice`/`isKeyboardDevice`/
/// `isMouseDevice` it reads at all, so this is the whole of what Cordial can
/// say about the machine's peripherals.
///
/// **"Exists" here means "the seat advertised a touchscreen when the window
/// opened", and that is a real limit rather than a definition of convenience.**
/// Wayland's `wl_seat.capabilities` arrives again whenever the seat's devices
/// change, and `wayland.rs`'s `seat_capabilities` does act on the later ones
/// -- a touchscreen plugged in mid-session gets a `wl_touch` bound and its
/// contacts routed to the touch native. What it cannot do is revise this: the
/// backend's `open()` runs before `cordial_appbridge_init`, the engine reads
/// `isTouchDevice` during that initialisation, and this build exposes no native
/// by which a platform amends it afterwards. So a screen that appears late
/// works as an input device and is invisible as a *declared* one.
///
/// That matters less than it sounds, and the reason is worth stating rather
/// than leaving to be rediscovered: `UserInputService.TouchEnabled` latches on
/// the first touch that actually arrives, not on this field, and
/// `PreferredInput`/`LastInputType` follow whichever native was called last. A
/// late touchscreen still flips both. `isTouchDevice` is what the engine builds
/// its first interface from, not what it follows.
pub fn report_touchscreen(seat_has_touch: bool) {
    let present = touchscreen_reported(seat_has_touch, no_touch(), input_touch_override());
    let why = match (no_touch(), input_touch_override()) {
        (true, _) => "CORDIAL_NO_TOUCH=1",
        (_, Some(_)) => "forced by CORDIAL_INPUT_TOUCH",
        // Not "from the seat": the X11 backend has no seat and reports false
        // because that backend cannot carry a contact at all, whatever the
        // machine has plugged in.
        (_, None) => "as the display backend found it",
    };
    println!("[android] touchscreen: {present} ({why}); PlatformParams.isTouchDevice follows");
    cordial_linker_sys::game_activity::set_touchscreen_present(present);
}

// --------------------------------------------------------------------- touch
//
// Fingers, and the one place a contact becomes an Android action.
//
// Everything here is display-server-independent on purpose: a backend's job is
// to say "this contact went down at (x, y)", and the translation into
// `ACTION_POINTER_DOWN` with an index packed into bits 8-15, into pointer ids
// that survive a finger in the middle lifting, and into the two natives that
// carry it happens once -- here -- where it can be unit-tested without a
// compositor, a touchscreen, or a loaded engine. The alternative was a copy per
// backend, which is the mistake the text-entry state machine at the top of this
// file exists to record.
//
// Only `wayland.rs` produces these today. X11 core has no touch at all and
// XInput2's is a separate extension nobody here has bound, so `window.rs` is
// silent rather than wrong.

/// `MotionEvent.ACTION_CANCEL` -- the gesture is over and did not mean
/// anything. Android's own answer to a contact that vanished rather than
/// lifted.
pub const ACTION_CANCEL: i32 = 3;
/// `MotionEvent.ACTION_POINTER_DOWN`/`_UP`: a second or later finger arriving,
/// and a finger leaving while others stay.
///
/// Both carry the pointer *index* the event is about in bits 8-15, which the
/// first and last contact do not need -- `ACTION_DOWN` can only ever be about
/// index 0, and `ACTION_UP` about the sole pointer left.
pub const ACTION_POINTER_DOWN: i32 = 5;
pub const ACTION_POINTER_UP: i32 = 6;
/// `AMOTION_EVENT_ACTION_POINTER_INDEX_SHIFT`, whose mask is `0xff00`.
const ACTION_POINTER_INDEX_SHIFT: u32 = 8;

/// `nativePassInput`'s own action vocabulary, which is **not** `MotionEvent`'s.
///
/// The descriptor `nativePassInput(IFFIII)V` is read out of this build's dex.
/// These three values are not: they come from mocktail (Apache-2.0), which
/// resolves and drives the same export, and they follow `Enum.UserInputState`'s
/// Begin/Change/End rather than `MotionEvent`, where UP is 1 and MOVE is 2.
/// Confusing the two delivers every drag as a release -- a plausible-looking
/// bug rather than a crash, which is the kind this file collects. `INFERRED`
/// until one session on a machine with a touchscreen settles it;
/// `CORDIAL_NO_TOUCH=1` turns off a wrong mapping meanwhile.
const TOUCH_DOWN: i32 = 0;
const TOUCH_MOVE: i32 = 1;
const TOUCH_UP: i32 = 2;

/// How many contacts are tracked at once.
///
/// Android's own `MotionEvent` tops out at 16 pointers and no hand asks for
/// more. The cap is here so that a compositor which sends a down Cordial never
/// sees the matching up for cannot grow this vector for the life of the
/// session.
const MAX_CONTACTS: usize = 16;

pub use cordial_linker_sys::game_activity::TouchContact;

/// `action | (index << 8)`, Android's packing for the two `_POINTER_` actions.
fn pack_pointer_action(action: i32, index: usize) -> i32 {
    action | ((index as i32) << ACTION_POINTER_INDEX_SHIFT)
}

/// The action for a contact arriving at pointer `index`.
///
/// Index 0 is a plain `ACTION_DOWN` rather than a packed `ACTION_POINTER_DOWN`
/// with a zero index. The two are different constants that happen to sit in
/// different bits; Android's contract is that the first contact of a gesture is
/// a down, and an engine that switches on the masked action would see a second
/// finger where there was a first.
fn contact_down_action(index: usize) -> i32 {
    if index == 0 { ACTION_DOWN } else { pack_pointer_action(ACTION_POINTER_DOWN, index) }
}

/// The action for a contact leaving from pointer `index`, given how many are
/// down *including this one*.
fn contact_up_action(index: usize, contacts: usize) -> i32 {
    if contacts <= 1 { ACTION_UP } else { pack_pointer_action(ACTION_POINTER_UP, index) }
}

/// One touch event, resolved into everything both natives need.
///
/// Built while the tracker's lock is held and delivered after it is dropped. A
/// JNI call into the engine is not something to hold an input-path mutex
/// across, and the only reason it would be safe today is that nothing
/// re-enters -- which is a property of this engine rather than of this code.
#[derive(Debug, PartialEq)]
struct TouchDispatch {
    /// The packed `MotionEvent` action for the AGDK path.
    action: i32,
    /// Every contact on the glass in pointer-index order, including one that is
    /// lifting: Android reports a departing pointer in the array of the event
    /// that says it left, and removing it first tells the engine one fewer
    /// finger was down than there was.
    contacts: Vec<TouchContact>,
    /// When the first contact of this gesture went down.
    down_time_ms: i64,
    /// What `nativePassInput` is told, one call per entry: a contact and its own
    /// down/move/up action. One entry for an ordinary event, every contact for a
    /// cancel.
    pass: Vec<(TouchContact, i32)>,
}

/// The contacts currently on the glass.
///
/// Keyed by whatever id the platform uses -- `wl_touch` hands out an `int32_t`
/// per contact and is free to reuse it the moment the finger is up -- and
/// translated to an Android pointer id allocated here. Deliberately not the
/// same number: Android's has to be stable for the life of the contact and
/// small enough to index by, and a compositor promises neither.
#[derive(Default)]
struct TouchContacts {
    /// `(platform id, contact)` in pointer-index order. The order is the whole
    /// contract with Android -- an entry's position *is* the pointer index the
    /// packed actions above refer to.
    contacts: Vec<(i64, TouchContact)>,
    down_time_ms: i64,
}

impl TouchContacts {
    fn index_of(&self, platform_id: i64) -> Option<usize> {
        self.contacts.iter().position(|(id, _)| *id == platform_id)
    }

    /// The lowest pointer id nothing is using.
    ///
    /// Android's own InputReader allocates this way, and it is not tidiness: a
    /// gesture that puts three fingers down and lifts the middle one must give
    /// the next finger id 1 again, or an engine keeping a per-id slot leaks one
    /// per contact across a session of pinching.
    fn free_pointer_id(&self) -> i32 {
        let mut id = 0;
        while self.contacts.iter().any(|(_, c)| c.id == id) {
            id += 1;
        }
        id
    }

    fn snapshot(&self) -> Vec<TouchContact> {
        self.contacts.iter().map(|(_, c)| *c).collect()
    }

    fn down(&mut self, platform_id: i64, x: f32, y: f32, time_ms: i64) -> Option<TouchDispatch> {
        // A second down for a contact already on the glass is not something
        // this can represent, and guessing which finger the compositor meant is
        // how one pointer id ends up owned by two. Drop it, and let
        // `dispatch_touch` say so under the trace.
        if self.index_of(platform_id).is_some() || self.contacts.len() >= MAX_CONTACTS {
            return None;
        }
        if self.contacts.is_empty() {
            self.down_time_ms = time_ms;
        }
        let contact = TouchContact { id: self.free_pointer_id(), x, y };
        self.contacts.push((platform_id, contact));
        let index = self.contacts.len() - 1;
        Some(TouchDispatch {
            action: contact_down_action(index),
            contacts: self.snapshot(),
            down_time_ms: self.down_time_ms,
            pass: vec![(contact, TOUCH_DOWN)],
        })
    }

    fn motion(&mut self, platform_id: i64, x: f32, y: f32) -> Option<TouchDispatch> {
        let index = self.index_of(platform_id)?;
        self.contacts[index].1.x = x;
        self.contacts[index].1.y = y;
        let contact = self.contacts[index].1;
        Some(TouchDispatch {
            // `ACTION_MOVE` carries every contact and names none: which one
            // moved is read off the array, not out of the action.
            action: ACTION_MOVE,
            contacts: self.snapshot(),
            down_time_ms: self.down_time_ms,
            pass: vec![(contact, TOUCH_MOVE)],
        })
    }

    fn up(&mut self, platform_id: i64) -> Option<TouchDispatch> {
        let index = self.index_of(platform_id)?;
        let contact = self.contacts[index].1;
        let dispatch = TouchDispatch {
            action: contact_up_action(index, self.contacts.len()),
            // Snapshot before the removal, so the lifting contact is still in
            // the array the engine is handed. See the field's own comment.
            contacts: self.snapshot(),
            down_time_ms: self.down_time_ms,
            pass: vec![(contact, TOUCH_UP)],
        };
        self.contacts.remove(index);
        Some(dispatch)
    }

    fn cancel(&mut self) -> Option<TouchDispatch> {
        if self.contacts.is_empty() {
            return None;
        }
        let contacts = self.snapshot();
        let dispatch = TouchDispatch {
            action: ACTION_CANCEL,
            // `nativePassInput` has no cancel anything here has established, so
            // every contact is reported up. That is the true statement
            // available -- the fingers are off the glass either way -- rather
            // than the convenient one of saying nothing and leaving the engine
            // holding contacts no later event will ever close.
            pass: contacts.iter().map(|c| (*c, TOUCH_UP)).collect(),
            contacts,
            down_time_ms: self.down_time_ms,
        };
        self.contacts.clear();
        Some(dispatch)
    }
}

static TOUCH: Mutex<Option<TouchContacts>> = Mutex::new(None);

/// `nativePassInput`, resolved by the loader like the mouse natives above.
///
/// Its own static rather than an eighth argument to [`set_input_natives`], for
/// the reason [`GET_MOUSE_LOCKED_CENTER`] has one: it arrived later, and
/// threading it through every position would make the existing call site harder
/// to read than a second setter is.
static PASS_INPUT: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

pub fn set_pass_input_native(native: *mut c_void) {
    PASS_INPUT.store(native, std::sync::atomic::Ordering::Relaxed);
}

/// One contact went down.
///
/// `platform_id` is the compositor's own id for the contact. `surface` is the
/// pixel size of the canvas the coordinates are in -- `nativePassInput` takes
/// it as two of its six arguments, and the engine learns the canvas size
/// separately through `onSurfaceChangedNative`, so carrying it per event rather
/// than assuming it keeps a resize from silently rescaling every touch.
pub fn touch_down(handle: i64, platform_id: i64, x: f32, y: f32, surface: (i32, i32), time_ms: i64) {
    if no_touch() {
        return;
    }
    let dispatch = {
        let mut state = TOUCH.lock().unwrap_or_else(|e| e.into_inner());
        state.get_or_insert_with(TouchContacts::default).down(platform_id, x, y, time_ms)
    };
    dispatch_touch(handle, dispatch, surface, time_ms, "down");
}

/// One contact moved.
pub fn touch_motion(
    handle: i64,
    platform_id: i64,
    x: f32,
    y: f32,
    surface: (i32, i32),
    time_ms: i64,
) {
    if no_touch() {
        return;
    }
    let dispatch = {
        let mut state = TOUCH.lock().unwrap_or_else(|e| e.into_inner());
        state.get_or_insert_with(TouchContacts::default).motion(platform_id, x, y)
    };
    dispatch_touch(handle, dispatch, surface, time_ms, "motion");
}

/// One contact left the glass.
pub fn touch_up(handle: i64, platform_id: i64, surface: (i32, i32), time_ms: i64) {
    if no_touch() {
        return;
    }
    let dispatch = {
        let mut state = TOUCH.lock().unwrap_or_else(|e| e.into_inner());
        state.get_or_insert_with(TouchContacts::default).up(platform_id)
    };
    dispatch_touch(handle, dispatch, surface, time_ms, "up");
}

/// Every contact is gone, and the gesture meant nothing.
///
/// `wl_touch.cancel` says the compositor has taken the sequence over -- a
/// system gesture, an edge swipe -- and that the client must undo whatever it
/// began. It is also the right thing to send when the canvas loses the touch
/// focus, for the reason `pointer_leave` synthesises button releases: no real
/// up is coming, and a contact nothing ever closes is a finger the engine
/// believes is still down for the rest of the session.
pub fn touch_cancel(handle: i64, surface: (i32, i32), time_ms: i64) {
    if no_touch() {
        return;
    }
    let dispatch = {
        let mut state = TOUCH.lock().unwrap_or_else(|e| e.into_inner());
        state.get_or_insert_with(TouchContacts::default).cancel()
    };
    dispatch_touch(handle, dispatch, surface, time_ms, "cancel");
}

/// Both pipes, for one resolved event.
///
/// The same both-paths policy the mouse already follows: AGDK's
/// `onTouchEventNative` is a real contract the engine consumes, and
/// `NativeInputInterface` is the one this project measured the Lua interface
/// actually hit-testing against for a click. **Which of the two moves a finger
/// on this build has not been measured** -- nobody here has a touchscreen -- so
/// both are driven and `CORDIAL_TRACE_TOUCH=1` prints what each was told and
/// what it answered, which is the reading that settles it in one session.
fn dispatch_touch(
    handle: i64,
    dispatch: Option<TouchDispatch>,
    surface: (i32, i32),
    time_ms: i64,
    what: &str,
) {
    let Some(d) = dispatch else {
        // A move for a contact that never went down, a second down for one that
        // already has, or a cancel with nothing on the glass. All three are the
        // compositor and this side disagreeing about what is down, and all
        // three are dropped rather than guessed at.
        if trace_touch() {
            eprintln!("[cordial] touch {what} for an unknown contact; dropped");
        }
        return;
    };
    if handle != 0 && !no_agdk_touch() {
        match cordial_linker_sys::game_activity::touch_multi(
            handle,
            d.action,
            &d.contacts,
            time_ms,
            d.down_time_ms,
        ) {
            Ok(Some(consumed)) => {
                if trace_touch() {
                    eprintln!(
                        "[cordial] onTouchEventNative(action={:#x}, contacts={}) -> {consumed}",
                        d.action,
                        d.contacts.len()
                    );
                }
            }
            Ok(None) => report_unregistered("onTouchEventNative"),
            Err(e) => super::trace(format_args!("onTouchEventNative(touch) failed: {e}")),
        }
    }

    let f = PASS_INPUT.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativePassInput");
        return;
    }
    let (w, h) = surface;
    for (contact, action) in d.pass {
        let r = cordial_linker_sys::game_activity::pass_input(
            f,
            contact.id,
            contact.x,
            contact.y,
            action,
            w,
            h,
        );
        if trace_touch() {
            eprintln!(
                "[cordial] nativePassInput(id={}, x={}, y={}, action={action}, w={w}, h={h}) \
                 -> {r:?}",
                contact.id, contact.x, contact.y
            );
        }
    }
}

/// `CORDIAL_NO_TOUCH=1` -- deliver no touch input to the engine at all.
///
/// The control arm for every claim this path could make, and the way a user
/// turns off a wrong mapping without a rebuild. A real off switch rather than a
/// trace flag: with it set nothing reaches either native, and `wayland.rs` does
/// not ask the seat for a `wl_touch` in the first place, so the run is the run
/// that shipped before any of this existed.
///
/// It is worth having for a second reason that is not about bugs. Sober #1577
/// reports a touchscreen laptop flipping irreversibly into the mobile interface
/// the first time the screen is touched, with the keyboard and mouse still
/// plugged in -- closed unfixed. Nothing here has reproduced that on Cordial,
/// and no honest value of `PlatformParams.isTouchDevice` would prevent it if it
/// does; a switch is what a player has meanwhile.
pub fn no_touch() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_NO_TOUCH").is_some())
}

/// `CORDIAL_TRACE_TOUCH=1` -- every contact, both calls, and what each
/// answered.
///
/// Its own switch rather than riding on `CORDIAL_TRACE_MOUSE`, because the
/// question a touch trace answers is which of the two paths carried the
/// gesture, and on a machine with both a touchscreen and a mouse a shared
/// switch would bury exactly that.
pub fn trace_touch() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_TRACE_TOUCH").is_some())
}

/// Deliver one AGDK key event, the `KeyEvent` synthesis both backends drive.
pub fn deliver_key(
    handle: i64,
    down: bool,
    key_code: i32,
    scan_code: i32,
    meta_state: i32,
    repeat_count: i32,
    unicode_char: i32,
    event_time_ms: i64,
    down_time_ms: i64,
) {
    if no_agdk_key() {
        return;
    }
    match cordial_linker_sys::game_activity::key(
        handle,
        down,
        key_code,
        scan_code,
        meta_state,
        repeat_count,
        unicode_char,
        event_time_ms,
        down_time_ms,
    ) {
        Ok(Some(consumed)) => {
            super::trace(format_args!("onKey{}Native(code={key_code}) -> {consumed}",
                if down { "Down" } else { "Up" }))
        }
        Ok(None) => report_unregistered(if down { "onKeyDownNative" } else { "onKeyUpNative" }),
        Err(e) => super::trace(format_args!(
            "onKey{}Native(code={key_code}) failed: {e}",
            if down { "Down" } else { "Up" }
        )),
    }
}

pub fn deliver_surface_redraw(handle: i64) {
    match cordial_linker_sys::game_activity::surface_redraw_needed(handle) {
        Ok(Some(())) => super::trace(format_args!("onSurfaceRedrawNeededNative")),
        Ok(None) => report_unregistered("onSurfaceRedrawNeededNative"),
        Err(e) => super::trace(format_args!("onSurfaceRedrawNeededNative failed: {e}")),
    }
}

// ------------------------------------------------------------ native passthrough
//
// The two `NativeInputInterface` natives Roblox's interface actually reads.
//
// Resolved once by the loader and stored here, because the input drain runs on
// the looper thread and has no access to the loaded library. Null until set, in
// which case only the AGDK path is driven — which is what shipped before, and
// which the interface ignores.
static PASS_MOUSE_MOVE: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static PASS_MOUSE_BUTTON: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static PASS_MOUSE_WHEEL: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static PASS_KEY_EVENT: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static PASS_TEXT: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
/// `syncTextboxTextAndCursorPosition2`. Separate from `PASS_TEXT` because it is
/// a different call at a different moment, not an alternative spelling of one.
static SYNC_TEXTBOX: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
/// `updateKeyboardSize`, the acknowledgement that an editor is up.
static UPDATE_KEYBOARD_SIZE: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
/// `nativeGetMainWindowIsMouseLockedCenter`. See
/// [`engine_wants_pointer_lock`].
static GET_MOUSE_LOCKED_CENTER: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
/// Focus generation the keyboard state was last reported for.
static KEYBOARD_REPORTED: Mutex<Option<u32>> = Mutex::new(None);

/// `NativeGLInterface.nativeGetTextBoxInfo()`, the engine's own answer to where
/// the focused box is. Read rather than written, like
/// [`GET_MOUSE_LOCKED_CENTER`] and unlike everything above it, and resolved
/// separately for the same reason.
static GET_TEXTBOX_INFO: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

pub fn set_textbox_info_native(native: *mut c_void) {
    GET_TEXTBOX_INFO.store(native, std::sync::atomic::Ordering::Relaxed);
}

/// Null when this build does not export it, in which case a caller has only
/// the spec `showKeyboard` volunteered and its own fallback.
pub fn textbox_info_native() -> *mut c_void {
    GET_TEXTBOX_INFO.load(std::sync::atomic::Ordering::Relaxed)
}

#[allow(clippy::too_many_arguments)]
pub fn set_input_natives(
    mouse_move: *mut c_void,
    mouse_button: *mut c_void,
    mouse_wheel: *mut c_void,
    key_event: *mut c_void,
    pass_text: *mut c_void,
    sync_textbox: *mut c_void,
    update_keyboard_size: *mut c_void,
) {
    PASS_MOUSE_MOVE.store(mouse_move, std::sync::atomic::Ordering::Relaxed);
    PASS_MOUSE_BUTTON.store(mouse_button, std::sync::atomic::Ordering::Relaxed);
    PASS_MOUSE_WHEEL.store(mouse_wheel, std::sync::atomic::Ordering::Relaxed);
    PASS_KEY_EVENT.store(key_event, std::sync::atomic::Ordering::Relaxed);
    PASS_TEXT.store(pass_text, std::sync::atomic::Ordering::Relaxed);
    SYNC_TEXTBOX.store(sync_textbox, std::sync::atomic::Ordering::Relaxed);
    UPDATE_KEYBOARD_SIZE.store(update_keyboard_size, std::sync::atomic::Ordering::Relaxed);
}

// ------------------------------------------------------------------- gamepad
//
// The same passthrough pattern as the mouse and keyboard natives above, with
// one rule they do not need: these six are stored all together or not at all.
//
// A partial set is worse than none. `nativeSetGamepadSupportedKeyWithGamepadType`
// is the call that tells the engine which buttons the pad has, and a build that
// exported the event natives but not the registration ones would take button and
// axis events for a device it had never been told the shape of. That looks
// exactly like working gamepad support with half the inputs silently dead --
// which is the failure `report_unregistered` exists to make visible, arriving in
// the one form it cannot see, because each individual call would have succeeded.
//
// mocktail reaches the same conclusion from the other end and nulls its whole
// gamepad symbol set when the `WithGamepadType` trio is incomplete
// (`src/runtime/roblox_capability_resolver.cc`, Apache-2.0). The idea, not the
// code.

static GAMEPAD_CONNECT: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static GAMEPAD_DISCONNECT: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static GAMEPAD_BUTTON: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static GAMEPAD_AXIS: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static GAMEPAD_SUPPORTED_KEY: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static GAMEPAD_SUPPORTED_MOTION: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Store the gamepad natives, or refuse the lot and say which were missing.
///
/// Returns the names that did not resolve, empty when all six did. The caller
/// prints them once at bring-up; nothing here logs, so that this stays testable
/// without a loaded library.
///
/// The names are in the order the six arguments are, which is the order they are
/// listed in `load.rs`.
#[allow(clippy::too_many_arguments)]
pub fn set_gamepad_natives(
    connect: *mut c_void,
    disconnect: *mut c_void,
    button: *mut c_void,
    axis: *mut c_void,
    supported_key: *mut c_void,
    supported_motion: *mut c_void,
) -> Vec<&'static str> {
    let all = [
        ("nativeGamepadConnectEventWithGamepadType", connect),
        ("nativeGamepadDisconnectEvent", disconnect),
        ("nativeGamepadButtonEvent", button),
        ("nativeGamepadAxisEvent", axis),
        ("nativeSetGamepadSupportedKeyWithGamepadType", supported_key),
        ("nativeSetGamepadSupportedMotionWithGamepadType", supported_motion),
    ];
    let missing: Vec<&'static str> =
        all.iter().filter(|(_, p)| p.is_null()).map(|(n, _)| *n).collect();
    if !missing.is_empty() {
        // Deliberately not a partial store. See the comment above.
        return missing;
    }
    GAMEPAD_CONNECT.store(connect, std::sync::atomic::Ordering::Relaxed);
    GAMEPAD_DISCONNECT.store(disconnect, std::sync::atomic::Ordering::Relaxed);
    GAMEPAD_BUTTON.store(button, std::sync::atomic::Ordering::Relaxed);
    GAMEPAD_AXIS.store(axis, std::sync::atomic::Ordering::Relaxed);
    GAMEPAD_SUPPORTED_KEY.store(supported_key, std::sync::atomic::Ordering::Relaxed);
    GAMEPAD_SUPPORTED_MOTION.store(supported_motion, std::sync::atomic::Ordering::Relaxed);
    Vec::new()
}

/// Whether all six gamepad natives resolved. False also means false for every
/// one of them individually, by [`set_gamepad_natives`]'s all-or-nothing rule,
/// so one load answers for the set.
pub fn gamepad_natives_ready() -> bool {
    !GAMEPAD_CONNECT.load(std::sync::atomic::Ordering::Relaxed).is_null()
}

/// `nativeGamepadConnectEventWithGamepadType`. See
/// [`super::gamepad`] for where `gamepad_type` comes from and why it is a
/// number a human has to supply.
pub fn deliver_gamepad_connect(id: i32, gamepad_type: i32) {
    let f = GAMEPAD_CONNECT.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativeGamepadConnectEventWithGamepadType");
        return;
    }
    let r = cordial_linker_sys::game_activity::gamepad_connect(f, id, gamepad_type);
    if trace_gamepad() {
        eprintln!("[cordial] nativeGamepadConnect(id={id}, type={gamepad_type}) -> {r:?}");
    }
}

pub fn deliver_gamepad_disconnect(id: i32) {
    let f = GAMEPAD_DISCONNECT.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativeGamepadDisconnectEvent");
        return;
    }
    let r = cordial_linker_sys::game_activity::gamepad_disconnect(f, id);
    if trace_gamepad() {
        eprintln!("[cordial] nativeGamepadDisconnect(id={id}) -> {r:?}");
    }
}

pub fn deliver_gamepad_button(id: i32, key_code: i32, action: i32) {
    let f = GAMEPAD_BUTTON.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativeGamepadButtonEvent");
        return;
    }
    let r = cordial_linker_sys::game_activity::gamepad_button(f, id, key_code, action);
    if trace_gamepad() {
        eprintln!("[cordial] nativeGamepadButton(id={id}, key={key_code}, action={action}) -> {r:?}");
    }
}

pub fn deliver_gamepad_axis(id: i32, axis: i32, x: f32, y: f32, z: f32) {
    let f = GAMEPAD_AXIS.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativeGamepadAxisEvent");
        return;
    }
    let r = cordial_linker_sys::game_activity::gamepad_axis(f, id, axis, x, y, z);
    if trace_gamepad() {
        eprintln!("[cordial] nativeGamepadAxis(id={id}, axis={axis}, {x}, {y}, {z}) -> {r:?}");
    }
}

pub fn deliver_gamepad_supported_key(id: i32, key_code: i32, supported: bool, gamepad_type: i32) {
    let f = GAMEPAD_SUPPORTED_KEY.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativeSetGamepadSupportedKeyWithGamepadType");
        return;
    }
    let r = cordial_linker_sys::game_activity::gamepad_supported_key(
        f, id, key_code, supported, gamepad_type,
    );
    if trace_gamepad() {
        eprintln!(
            "[cordial] nativeSetGamepadSupportedKey(id={id}, key={key_code}, \
             supported={supported}, type={gamepad_type}) -> {r:?}"
        );
    }
}

pub fn deliver_gamepad_supported_motion(
    id: i32,
    axis: i32,
    source: i32,
    supported: bool,
    gamepad_type: i32,
) {
    let f = GAMEPAD_SUPPORTED_MOTION.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativeSetGamepadSupportedMotionWithGamepadType");
        return;
    }
    let r = cordial_linker_sys::game_activity::gamepad_supported_motion(
        f, id, axis, source, supported, gamepad_type,
    );
    if trace_gamepad() {
        eprintln!(
            "[cordial] nativeSetGamepadSupportedMotion(id={id}, axis={axis}, source={source}, \
             supported={supported}, type={gamepad_type}) -> {r:?}"
        );
    }
}

/// `CORDIAL_TRACE_GAMEPAD=1`. Its own switch rather than riding on
/// `CORDIAL_TRACE_MOUSE`, because a pad at rest still emits a steady trickle of
/// axis noise and mixing that into the mouse trace would bury it.
pub fn trace_gamepad() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_TRACE_GAMEPAD").is_some())
}

/// `NativeInputInterface.nativeGetMainWindowIsMouseLockedCenter()Z`, resolved
/// separately from [`set_input_natives`] because it is the only one of these
/// that Cordial *reads* rather than writes.
pub fn set_mouse_lock_native(native: *mut c_void) {
    GET_MOUSE_LOCKED_CENTER.store(native, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the engine currently wants the pointer locked to the centre of the
/// window — first person, or anything else that turns the camera with the mouse
/// rather than with a cursor.
///
/// `None` means Cordial does not know, and that is a third answer rather than a
/// polite `false`: the native may not be exported by this build, may not be
/// resolvable yet during startup, or may have failed. A caller that treats
/// "unknown" as "no lock wanted" is making the stub lie in the sense AGENTS.md
/// means it; here the caller keeps its own drag-driven lock instead, which is
/// honest about resting on something other than the engine's word.
///
/// **The direction of this call was the hypothesis worth being explicit about,
/// and half of it is now measured.** The native is a getter on
/// `NativeInputInterface` that Cordial had never called, so nothing
/// distinguished "the platform is supposed to poll it" from "the engine calls
/// something else and this is dead" — and a `false` that never changes is what
/// a dead getter and an idle one both look like.
///
/// Two runs on 2026-08-28, `CORDIAL_TRACE_MOUSE=1`, 30 seconds each, identical:
///
/// ```text
/// input: nativeGetMainWindowIsMouseLockedCenter resolved
/// [cordial] nativeGetMainWindowIsMouseLockedCenter() -> false
/// ```
///
/// So it resolves, it is called every pump, it answers, and it does not throw —
/// the `FAILED` latch below never fires and `None` is never returned for that
/// reason. **The dead-getter branch is closed.**
///
/// **And it answers `true`, observed 2026-09-04.** This used to say the true
/// case was unmeasured and that the engine-driven half of pointer capture was
/// `INFERRED` until somebody ran a session in first person. One was: a signed-in
/// profile, a joined experience, the camera scrolled in, on engine
/// 2.736.0.1408 under a nested sway. `pointerlock` went from `engine=false` to
/// `engine=true` on the scroll and Cordial requested the lock in response, and
/// back to false on scrolling out. The engine-driven half is measured now.
///
/// Two things that came out of the same session and are worth having here,
/// because both contradict something this tree believed:
///
/// - **Escape makes the engine stop asking.** It is Roblox's menu key and
///   still reaches the engine, and opening the menu drops the request:
///   `engine=true` before, `engine=false` immediately after, `true` again when
///   a second Escape closes the menu. See
///   `wayland::toggle_pointer_lock_suppression`.
/// - **A right-button press does not make the engine ask.** Held in third
///   person it stayed `engine=false` for the whole drag. mocktail documents
///   the opposite for its build; see `wayland::LOCK_WANTED_BEFORE_RIGHT_DRAG`.
///
/// Called through `call_static_bare_bool`, the same `(JNIEnv*, jclass)` shape
/// every other `NativeInputInterface` native here is called with — see
/// `native/game_activity.cpp`'s `cordial_input_mouse_move`, which passes the
/// class object in exactly this position.
/// The `fakeenginelock` override: 0 off, 1 force false, 2 force true.
///
/// See [`engine_wants_pointer_lock`] for what a reading taken with this set is
/// and is not evidence of.
pub static FAKE_ENGINE_LOCK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn engine_wants_pointer_lock() -> Option<bool> {
    // **A test seam, and it tests Cordial and not the engine.** 0 is off, 1
    // false, 2 true. Nothing sets it but the `fakeenginelock` control verb,
    // which is only reachable with `CORDIAL_DEV_CONTROL=1`.
    //
    // It exists because the state machine around this answer cannot otherwise
    // be exercised at all. The comment above says why: this getter has never
    // been observed returning true, because that needs first person or shift
    // lock and cannot be had from the home page. The right-drag latch keys
    // entirely off a false-then-true transition, so without a seam the only
    // available evidence for it is reading the code -- which is how it shipped
    // and how its own wedge got in.
    //
    // What a run with this on proves is that Cordial reacts correctly to that
    // transition. That the engine *makes* it stays `INFERRED`, from mocktail's
    // `window_pointer_capture_owner.cc` documenting the same behaviour, and
    // nothing here upgrades that.
    match FAKE_ENGINE_LOCK.load(std::sync::atomic::Ordering::Relaxed) {
        1 => return Some(false),
        2 => return Some(true),
        _ => {}
    }
    let f = GET_MOUSE_LOCKED_CENTER.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        return None;
    }
    // A native that throws once will throw every pump, and 20 identical lines a
    // second buries whatever the session was actually about. One line, then
    // never again — and never again also means never *called* again, because
    // the failure was in the call itself.
    static FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if FAILED.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    match cordial_linker_sys::game_activity::call_static_bare_bool(
        f,
        "com/roblox/engine/jni/NativeInputInterface",
    ) {
        Ok(v) => {
            if trace_mouse() {
                static LAST: Mutex<Option<bool>> = Mutex::new(None);
                let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
                if *last != Some(v) {
                    *last = Some(v);
                    eprintln!("[cordial] nativeGetMainWindowIsMouseLockedCenter() -> {v}");
                }
            }
            Some(v)
        }
        Err(e) => {
            FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "[cordial] nativeGetMainWindowIsMouseLockedCenter() failed ({e}); \
                 not asking again this session. Pointer capture now depends on the \
                 mouse button alone."
            );
            None
        }
    }
}

/// Tell the engine whether an editor is up, when that has changed.
///
/// This closes the handshake `showKeyboard` opens. It runs from the input pump
/// rather than from inside `showKeyboard` itself because on Android the reply
/// comes from the UI thread after the IME has actually appeared, not
/// synchronously from within the request — and calling back into the engine
/// from inside its own call is a re-entry this has no reason to risk.
pub fn report_keyboard_state(current_geometry: (i32, i32)) {
    // `CORDIAL_NO_KEYBOARD_REPORT=1` suppresses this entirely, and by default it
    // is suppressed — see `keyboard_report_enabled` for the measurement.
    if !keyboard_report_enabled() {
        return;
    }
    let f = UPDATE_KEYBOARD_SIZE.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        return;
    }
    let generation = cordial_linker_sys::game_activity::textbox_generation();
    {
        let mut seen = KEYBOARD_REPORTED.lock().unwrap_or_else(|e| e.into_inner());
        if *seen == Some(generation) {
            return;
        }
        *seen = Some(generation);
    }
    // The dex declares this as `updateKeyboardSize(Z, I, I, I, I)`, and the
    // real-Android capture pins the argument order and the resting value. Its
    // Java-side layout callback logs, twice, at surface bring-up:
    //
    //     rbx.glview.layout: onUpdateKeyboardSize() v:false x:0 y:999 w:2491 h:0
    //
    // So it is (visible, x, y, width, height), and the keyboard-hidden baseline
    // the real client reports is *not* an empty rectangle: it is visible=false
    // with the box still pinned to the bottom edge of the UI space, full width,
    // zero height. Cordial's rectangle was already that shape; only the boolean
    // was wrong. It used to send `visible=true` with a zero height, which claims
    // a soft keyboard is on screen and simultaneously that it covers nothing,
    // and that was measured to bounce focus continuously.
    //
    // A desktop genuinely has no soft keyboard, so the resting baseline is the
    // truthful report here as well as the observed one, and it does not become
    // less true when a box takes focus. Do not "fix" this by zeroing x/y/w
    // as well: an all-zero rectangle is a third value nothing has ever been
    // seen to send. `INFERRED` only in that the capture shows the app's own
    // Java callback rather than the JNI call it feeds; the shape is observed,
    // the 1:1 with the native call is not.
    let (w, h) = current_geometry;
    let r = cordial_linker_sys::game_activity::update_keyboard_size(f, false, 0, h, w, 0);
    if trace_text() {
        eprintln!("[cordial] updateKeyboardSize(visible=false, x=0, y={h}, w={w}, h=0) -> {r:?}");
    }
}

/// `NativeGLInterface.nativePassKeyEvent(Z down, I keyCode, I modifiers, Z isRepeat)`.
///
/// Traced, because it had never once been observed. Every keyboard
/// investigation here has read `onKeyDownNative` lines, and this is the *other*
/// path a keystroke takes — the one the interface actually reads, by the same
/// argument that made `nativePassMouseButton` rather than `onTouchEventNative`
/// the thing that moves the UI. A path with no instrumentation cannot be ruled
/// in or out, and both were being ruled on from the same silence.
///
/// `key_code` is an `android.view.KeyEvent.KEYCODE_*`, produced by
/// [`keysym_to_android`]. It is emphatically *not* an evdev code, and the trace
/// prints it so that a run can say which of the two the engine is being handed
/// rather than a reader having to trust the call chain. The two numbering
/// schemes agree at exactly one letter — evdev `KEY_D` and `AKEYCODE_D` are
/// both 32 — so "only D works" is the signature of a raw evdev code reaching
/// something that wanted an Android one, and one traced keystroke settles it.
/// `NativeInputInterface.nativePassKeyEvent(Z down, I code, I modifiers, Z repeat)`.
///
/// **`code` is a Linux evdev code, not an Android keycode**, and getting that
/// backwards cost days. The symptom was that exactly one key worked — `D` — and
/// that holding Alt made the character *jump*.
///
/// Both fall out of the same arithmetic. `AKEYCODE_D` is 32 and `KEY_D` is 32,
/// so `D` worked by pure collision and hid the problem. `AKEYCODE_ALT_LEFT` is
/// 57 and `KEY_SPACE` is 57, so Alt read as Space, and Space is jump. The rest
/// simply landed on codes with no meaning: `W` went as `AKEYCODE_W` 51, which is
/// `KEY_COMMA`; `A` as 29, which is `KEY_LEFTCTRL`; `S` as 47, which is `KEY_V`.
///
/// Four theories were measured and disproved before this, all of them assuming a
/// number was wrong somewhere in a translation table. The number was fine. It
/// was the *vocabulary* — every one of them took for granted that this native
/// wanted what AGDK's `onKeyDownNative` wants, and it does not. Note the
/// signature has no scan-code slot at all, which is the tell: a native that
/// takes one code and no scan code is taking the platform's own.
///
/// `CORDIAL_KEY_ANDROID_CODES=1` restores the old behaviour as a control.
/// Whether this evdev code is a key a focused text field would consume.
///
/// Letters, digits, space and the punctuation between them -- the keys that
/// produce a character. Deliberately *not* Escape, Tab, Enter, the function
/// keys, the arrows or the modifiers: a game legitimately hears those while a
/// box is open, and Escape in particular is how somebody gets out of one.
///
/// Ranges rather than a table because evdev's layout is contiguous here:
/// 2-13 is the number row, 16-27 and 30-41 and 44-53 are the three letter rows
/// with their neighbouring punctuation, and 57 is space.
fn evdev_is_text_key(code: i32) -> bool {
    matches!(code, 2..=13 | 16..=27 | 30..=41 | 44..=53 | 57)
}

/// Send the game keystrokes that a focused text box has already eaten.
///
/// Off by default, because doing it is a bug -- see `pass_key_event`. Kept as
/// the control, so the old behaviour can be measured against the new one in the
/// same session rather than argued about.
fn keys_to_game_while_typing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_KEYS_TO_GAME_WHILE_TYPING").is_some())
}

pub fn pass_key_event(down: bool, evdev_code: i32, modifiers: i32) {
    if no_pass_key() {
        return;
    }
    // **A focused text box eats its own keys, and the game must not hear them
    // too.**
    //
    // Every path funnels through here, so this is the one place it can be said.
    // Without it, typing "w" into a chat box also walks the character and "/"
    // reopens chat over what you were writing -- reported against Sober, which
    // has the same shape: its #987 describes pressing "/" and the previous
    // message sending again.
    //
    // Android does this for us and that is why the engine never asked: an
    // `EditText` with focus consumes the key, the IME commits it, and the
    // activity's own key handler never sees it. Cordial is the platform here,
    // so the suppression is Cordial's job.
    //
    // Only character keys. Escape, Tab, Enter, the arrows, the function keys
    // and the modifiers still go through -- a game hears those while a box is
    // open, and Escape is how somebody leaves one.
    //
    // `KEYS_HELD` is still updated below, before this returns, or a key held
    // when a box took focus would never be recorded as released.
    let would_suppress = !keys_to_game_while_typing()
        && evdev_is_text_key(evdev_code)
        && cordial_linker_sys::game_activity::focused_textbox().is_some();

    // **A key whose press reached the engine must have its release reach it
    // too, whatever happened to the focus in between.**
    //
    // This is why "/" took two presses to open chat. Pressing "/" with nothing
    // focused is what *causes* chat's TextBox to focus, and that focus arrives
    // asynchronously, from `showKeyboard` on another thread. When it lands
    // between the key's own down and up -- which it does, on a timing that
    // varies run to run -- the test above answers "no box" for the down and
    // "box" for the up. The press went to the engine and the release did not,
    // so as far as the engine is concerned "/" is still held, and the next
    // press is a repeat of a key already down rather than a new one.
    //
    // Measured live, joined to an experience with `CORDIAL_TRACE_TEXT=1`,
    // sweeping the gap between down and up:
    //
    //     pass_key_event down=true  code=53 mods=0x0 focus=None
    //     textbox focused handle=140015588225152 current=0 bytes
    //     pass_key_event suppressed: code=53 down=false
    //
    // With the guard off entirely (`CORDIAL_KEYS_TO_GAME_WHILE_TYPING=1`) the
    // same race happens and nothing is dropped: 6 of 8 gaps opened chat,
    // against 2 of 8 with it on. So the guard, not engine timing, was the
    // unreliability.
    //
    // Pairing releases to presses keeps Sober #987 intact, which is the whole
    // reason the guard exists: a key whose *down* was suppressed because a box
    // already had focus is not recorded here, so its up is suppressed too, and
    // typing "/" into an open chat box still cannot reopen chat.
    let release_of_a_forwarded_press = !down && take_forwarded_press(evdev_code);

    if would_suppress && !release_of_a_forwarded_press {
        track_key_held(down, evdev_code);
        if trace_text() {
            eprintln!(
                "[cordial] pass_key_event suppressed: code={evdev_code} down={down} \
                 (a text box has focus and would eat this)"
            );
        }
        return;
    }
    if down {
        remember_forwarded_press(evdev_code);
    }
    if trace_text() && release_of_a_forwarded_press && would_suppress {
        eprintln!(
            "[cordial] pass_key_event code={evdev_code} up: forwarded anyway, \
             its press reached the engine before the box took focus"
        );
    }
    track_key_held(down, evdev_code);
    let key_code = evdev_code;
    let f = PASS_KEY_EVENT.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativePassKeyEvent");
        return;
    }
    let r = cordial_linker_sys::game_activity::pass_key_event(f, down, key_code, modifiers, false);
    super::trace(format_args!(
        "nativePassKeyEvent(down={down}, keyCode={key_code}, modifiers={modifiers:#x}) -> {r:?}"
    ));
    // Focus is printed here rather than only in `dispatch_key` because the
    // control socket calls this function directly, so a synthetic keystroke
    // produced no focus line at all and "is a text box eating the movement
    // keys?" could not be answered from a driven run. `super::trace` is no use
    // for it: that is `CORDIAL_TRACE=1`, which aborts the engine.
    if trace_text() {
        eprintln!(
            "[cordial] pass_key_event down={down} code={key_code} mods={modifiers:#x} \
             focus={:?} gen={}",
            cordial_linker_sys::game_activity::focused_textbox(),
            cordial_linker_sys::game_activity::textbox_generation(),
        );
    }
}

/// Which evdev codes are currently held, tracked here rather than in
/// `window.rs`/`wayland.rs` because both backends already funnel every key
/// transition through this one function. A `Vec` rather than a `HashSet`: the
/// realistic size is single digits (nobody holds more than a few keys at
/// once), so a linear scan is cheaper than hashing and `Vec::new()` is a
/// `const fn`, which a `HashSet` field on a static would complicate for no
/// measured benefit.
///
/// This exists for [`idle_keepalive`] — see that function for what it is
/// tracking held keys *for*.
static KEYS_HELD: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// Codes whose press was forwarded to the engine and whose release has not
/// been yet.
///
/// Separate from [`KEYS_HELD`], which cannot answer this: that one is updated
/// on the suppressed path too, deliberately, so it records a key held when a
/// box took focus. This one records only what the engine was actually told
/// about, which is the distinction `pass_key_event` needs to pair a release to
/// its press.
///
/// A `Vec` for the same reason as `KEYS_HELD` -- a handful of entries at most,
/// a linear scan beats hashing, and `Vec::new()` is a `const fn`.
static FORWARDED_PRESSES: Mutex<Vec<i32>> = Mutex::new(Vec::new());

fn remember_forwarded_press(evdev_code: i32) {
    let mut sent = FORWARDED_PRESSES.lock().unwrap_or_else(|e| e.into_inner());
    if !sent.contains(&evdev_code) {
        sent.push(evdev_code);
    }
}

/// Whether this code's press was forwarded, consuming the record.
///
/// Consuming rather than peeking, so one press releases exactly once. Without
/// that, a release whose press was suppressed could ride on a stale entry from
/// an earlier press of the same key and defeat the guard.
fn take_forwarded_press(evdev_code: i32) -> bool {
    let mut sent = FORWARDED_PRESSES.lock().unwrap_or_else(|e| e.into_inner());
    match sent.iter().position(|&c| c == evdev_code) {
        Some(i) => {
            sent.remove(i);
            true
        }
        None => false,
    }
}

fn track_key_held(down: bool, evdev_code: i32) {
    let mut held = KEYS_HELD.lock().unwrap_or_else(|e| e.into_inner());
    if down {
        if !held.contains(&evdev_code) {
            held.push(evdev_code);
        }
    } else {
        held.retain(|&c| c != evdev_code);
    }
}

/// Send a zero-delta `nativePassMouseMove` while a key is held, so the engine's
/// own idle throttle does not mistake "walking in a straight line without
/// touching the mouse" for nobody playing.
///
/// **What this is answering.** `docs/NEXT.md` §1d established that presents
/// collapse from ~60/s to exactly 1.0/s about thirteen seconds into an idle
/// app shell, and that driving `pass_mouse_move` continuously holds it at
/// 50-60/s indefinitely — but that measurement drove mouse movement, camera
/// look and held keys all together, so it could not say which one the engine
/// was actually watching. Measured in isolation (`CORDIAL_SCRIPT=key-on`,
/// `touch-on`, `look-on`, `ping-on` against a real `libroblox.so`, landing
/// page, no account): a single held key produces exactly one down event under
/// Wayland (`keyboard_repeat_info` in `wayland.rs` is a documented no-op, and
/// nothing here reintroduces repeat), and that one event does not stop the
/// collapse — it lands at the same ~15s mark as no input at all, twice.
/// Redriving `deliver_key`/`pass_key_event` on every tick, simulating what a
/// repeat timer would send, does not stop it either. Redriving
/// `deliver_mouse`'s AGDK touch queue every tick does not stop it. Only
/// `pass_mouse_move` — `NativeInputInterface.nativePassMouseMove`, the "V2"
/// interface call, not AGDK's `onTouchEventNative` — keeps it away, and it
/// does so with the delta held at exactly zero: a fixed position resent every
/// tick holds presents at a flat 60.0/s for the whole run, no less reliably
/// than a moving one, and collapses within about a second of stopping. So the
/// engine is watching this one call landing, not the camera actually turning.
///
/// **Why a real position matters.** The dex declares this native as an
/// absolute position plus a delta, and `MOUSE_LAST` is the last position a
/// genuine pointer event reported — reusing it, with a (0, 0) delta, tells the
/// engine truthfully where the pointer already is and that it has not moved,
/// which is honest in both halves. Inventing a position (window centre, say)
/// would tell the engine the pointer jumped there, and Roblox's UI does hit
/// testing against the reported absolute position — a jump risks nudging
/// whatever the real cursor happens to be hovering. If no genuine pointer
/// event has ever landed there is nothing honest to resend, so this does
/// nothing rather than guess.
///
/// Called from [`super::looper::pump`] every tick a key is held; harmless to
/// call when the interface native is not registered yet, since
/// [`pass_mouse_move_delta`] already falls through to
/// [`report_unregistered`]'s deduplicated logging rather than spamming.
/// When Cordial stops sending [`idle_keepalive`], from `CORDIAL_THROTTLE`.
///
/// The shell's "Slow the game down in the background" row sets this; see
/// `cordial_shell::shell_config::ThrottleWhen`, which holds the reasoning for
/// why `Visible` is the default rather than `Unfocused`. Parsed once, because
/// the launch settles it and nothing changes it mid-run.
///
/// **This governs the keepalive only.** `onWindowFocusChangedNative` is driven
/// on every genuine transition whatever this says — the engine is told the
/// truth about focus and this decides what Cordial does about it. Anything
/// unrecognised, including the variable being absent, is `Visible`: an old
/// shell launching a new client, or a client started by hand, gets the default
/// rather than a refusal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThrottleWhen {
    Visible,
    Unfocused,
    Off,
}

pub fn throttle_policy() -> ThrottleWhen {
    static POLICY: std::sync::OnceLock<ThrottleWhen> = std::sync::OnceLock::new();
    *POLICY.get_or_init(|| match std::env::var("CORDIAL_THROTTLE").as_deref() {
        Ok("unfocused") => ThrottleWhen::Unfocused,
        Ok("off") => ThrottleWhen::Off,
        _ => ThrottleWhen::Visible,
    })
}

/// Whether the keepalive should run this tick.
///
/// `focused` and `visible` are `None` when the backend does not track them —
/// X11 tracks neither — and `None` keeps the keepalive running. That is
/// deliberate and is the whole reason these are three-valued: throttling a
/// window because nothing was watching it would be the same class of bug as
/// never throttling at all, arriving from the other side.
///
/// Pure, and separate from [`idle_keepalive`], so the policy table is testable
/// without a window, a compositor or a loaded engine.
pub fn keepalive_wanted(policy: ThrottleWhen, focused: Option<bool>, visible: Option<bool>) -> bool {
    match policy {
        ThrottleWhen::Off => true,
        ThrottleWhen::Unfocused => focused != Some(false),
        // **Focused beats not-visible, and that is the second condition this
        // comment used to say would be needed one day.**
        //
        // The case it was written for -- a compositor that hides a window and
        // reports nothing -- is not the one that turned up. The opposite did:
        // mutter reports `FOCUSED | SUSPENDED` for a window that is merely
        // covered, and goes on reporting it after the window is uncovered
        // again. `android::backend_focused`'s own comment has the measurement:
        // 20 s and 7 s of stuck `SUSPENDED` across 15 runs.
        //
        // With `visible != Some(false)` alone that is a window the user has
        // alt-tabbed back to, is looking at, and which stays at the engine's
        // 1.0/s idle throttle until the compositor gets round to clearing a
        // flag. Reported exactly that way: alt-tabbing away and back "ruins
        // fps". The window being focused is the strongest evidence there is
        // that somebody is looking at it, and it should not be outvoted by a
        // visibility flag that is known to stick.
        //
        // This does not weaken the setting where it earns its keep. A
        // minimised window reports `focused = Some(false)` as well as not
        // visible, so it still throttles; so does a window on another
        // workspace. What changes is only the contradictory pair, and the
        // contradiction resolves towards the signal that cannot be stale
        // without the user noticing.
        ThrottleWhen::Visible => visible != Some(false) || focused == Some(true),
    }
}

pub fn idle_keepalive() {
    let any_held = !KEYS_HELD.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
    if !any_held {
        return;
    }
    if let Some((x, y)) = mouse_last_position() {
        pass_mouse_move_delta(x, y, 0.0, 0.0);
    }
}

pub fn pass_text(which: i64, text: &str, cursor: i32) {
    // The per-keystroke sync first: this is the call that actually fills the
    // field. `nativePassText` is driven alongside it for the same reason both
    // mouse paths are — the interface declares both and the cost of driving
    // one that turns out to be a no-op is nothing.
    let sync = SYNC_TEXTBOX.load(std::sync::atomic::Ordering::Relaxed);
    if !sync.is_null() {
        if let Err(e) = cordial_linker_sys::game_activity::sync_textbox(sync, text, cursor) {
            if trace_text() {
                eprintln!("[cordial] syncTextbox failed: {e}");
            }
        }
    }
    // `nativePassText` is deliberately NOT driven per keystroke.
    //
    // The two calls are not alternatives. `syncTextboxTextAndCursorPosition2`
    // takes no box handle and updates whichever box has focus — that is the
    // per-keystroke update. `nativePassText` takes the handle `showKeyboard`
    // issued and is the *finish* call: on Android it is the soft keyboard
    // delivering its final text and dismissing itself.
    //
    // **RETRACTED, 2026-08-24: driving both no longer blurs the box.** Nine
    // runs across two sessions -- six by an agent on the sign-in field, three
    // here with `CORDIAL_PASS_TEXT_ON_KEY=1` -- typed six and eight characters
    // with every keystroke driving `nativePassText` as well, and produced zero
    // early blurs. `textbox blurred` fired only at teardown. Whatever the
    // original trace caught, it does not reproduce on this build.
    //
    // The default stays off anyway, because the run that disproved the blur
    // also showed it buys nothing: with `nativePassText` on all eight
    // keystrokes the focused field still rendered **empty**, and only Cordial's
    // own editor overlay showed the text. Feeding the engine exactly what an
    // Android IME feeds does not make it draw a focused TextBox, which is
    // `docs/NEXT.md` §1 confirmed rather than escaped. A default flipped for a
    // reason that turned out to be wrong, with no measured benefit, is a change
    // nobody could attribute later.
    //
    // The original account, kept because the shape of it is what the flag is
    // for -- the character landed and the box immediately lost focus:
    //
    //     textbox focused ... current=0 bytes
    //     key down "g" focus=Some(140515299098752)
    //     text -> "g" caret=1
    //     textbox blurred
    //     ... textbox focused ... current=1 bytes   <- the "g" was accepted
    //
    // So the field really was receiving the text; every keystroke also ended
    // the editing session, which is why it needed re-clicking per character and
    // why no caret ever persisted.
    //
    // `CORDIAL_PASS_TEXT_ON_KEY=1` restores the old behaviour for anyone
    // testing this claim rather than taking it on trust.
    static PASS_TEXT_ON_KEY: OnceLock<bool> = OnceLock::new();
    let f = if *PASS_TEXT_ON_KEY
        .get_or_init(|| std::env::var_os("CORDIAL_PASS_TEXT_ON_KEY").is_some())
    {
        PASS_TEXT.load(std::sync::atomic::Ordering::Relaxed)
    } else {
        std::ptr::null_mut()
    };
    if !f.is_null() {
        // `nativePassText(long, String, boolean, int)`. The boolean's meaning is
        // not declared anywhere Cordial can read, so it stays a knob until a run
        // settles it: `CORDIAL_PASSTEXT_FLAG=1` sends true.
        static PASS_TEXT_FLAG: OnceLock<bool> = OnceLock::new();
        let flag = *PASS_TEXT_FLAG
            .get_or_init(|| std::env::var_os("CORDIAL_PASSTEXT_FLAG").is_some());
        if let Err(e) = cordial_linker_sys::game_activity::pass_text(f, which, text, flag, cursor) {
            if trace_text() {
                eprintln!("[cordial] passText failed: {e}");
            }
        }
    }
    if trace_text() {
        // The size, not the text. See `trace_text_contents` — this line used to
        // print a password in full on every keystroke of it.
        eprintln!(
            "[cordial] text -> {} caret={cursor} sync={} passText={}",
            redacted(text),
            !sync.is_null(),
            !f.is_null()
        );
    }
}

/// Where the pointer was the last time one was reported, so the next report
/// can carry how far it moved. `None` means "no previous position to subtract"
/// — see [`reset_mouse_delta`].
// Motion can arrive at a mouse's full report rate. Keeping the last absolute
// position behind a mutex made every report contend with the control socket's
// keepalive read, despite the two floats fitting in one atomic word. All
// producers still observe a complete pair: `swap` publishes x and y together.
const NO_MOUSE_POSITION: u64 = u64::MAX;
static MOUSE_LAST: AtomicU64 = AtomicU64::new(NO_MOUSE_POSITION);

fn pack_mouse_position(x: f32, y: f32) -> u64 {
    ((x.to_bits() as u64) << 32) | y.to_bits() as u64
}

fn unpack_mouse_position(position: u64) -> (f32, f32) {
    (f32::from_bits((position >> 32) as u32), f32::from_bits(position as u32))
}

fn mouse_last_position() -> Option<(f32, f32)> {
    let position = MOUSE_LAST.load(Ordering::Acquire);
    (position != NO_MOUSE_POSITION).then(|| unpack_mouse_position(position))
}

/// An accelerated relative-motion delta waiting for the absolute position
/// report it belongs to.
///
/// `zwp_relative_pointer_v1.relative_motion` and `wl_pointer.motion` describe
/// the same physical movement through two different protocol objects on the
/// same seat, and nothing in the relative-pointer extension's text says which
/// one a compositor writes to the wire first — see `relative_pointer_motion`
/// in `wayland.rs` for what was checked before settling on "unspecified"
/// rather than guessing an order. Stashing the delta here rather than handing
/// it straight to the engine is what lets [`pass_mouse_move`] decide, once the
/// matching absolute position turns up, whether to use it or fall back to the
/// old arithmetic — see [`resolve_mouse_delta`] for that choice and
/// [`accumulate_unlocked_delta`]/[`take_pending_unlocked_delta`] for the
/// producer and consumer either side of it.
///
/// The same packing as `MOUSE_LAST` and the same sentinel, because the trick
/// — two `f32`s in one atomic word — is about the storage and not about what
/// the pair means.
static PENDING_UNLOCKED_DELTA: AtomicU64 = AtomicU64::new(NO_MOUSE_POSITION);

/// Forget the last reported pointer position, so the next move reports a zero
/// delta rather than the distance from wherever the pointer was before.
///
/// Called when the pointer enters or leaves the canvas. Without it, a pointer
/// that left at one edge and came back at the other would report the whole
/// width of the window as a single movement — and a delta is what turns the
/// camera, so that is not a cosmetic error but a view that snaps round.
///
/// Deliberately touches only `MOUSE_LAST` and not [`PENDING_UNLOCKED_DELTA`],
/// even though every call site also wants that one forgotten — see
/// [`forget_pending_unlocked_delta`], kept separate for exactly the reason its
/// own doc gives: two statics behind one reset function would coincidentally
/// couple this function's own test to that one's.
pub fn reset_mouse_delta() {
    MOUSE_LAST.store(NO_MOUSE_POSITION, Ordering::Release);
}

/// Discard any accelerated relative-motion sample waiting for an absolute
/// position report that is no longer coming.
///
/// Called at every site that also calls [`reset_mouse_delta`] — the pointer
/// entering or leaving the canvas, and a lock being taken or released — for
/// the same reason that function exists: a relative-motion sample that
/// arrived just before the pointer left the canvas, or just before a lock
/// engaged, describes movement that is about to become meaningless once the
/// thing it was going to be attached to changes. Carrying it forward would
/// apply somebody else's movement to whatever happens next, which is the same
/// "distance from wherever it was before" bug `reset_mouse_delta` already
/// exists to prevent, arriving through the other producer.
///
/// Kept as a second function rather than folded into `reset_mouse_delta`
/// itself, even though every current call site wants both: that function has
/// its own test (`the_first_move_after_the_pointer_arrives_has_no_delta`) built
/// on the same "one test, not several, because two tests sharing global state
/// would race" reasoning [`PENDING_UNLOCKED_DELTA`]'s own test relies on, and
/// folding the two statics behind one call would silently make each test's
/// `reset_mouse_delta()` call touch the other test's state too — turning two
/// independent, deliberately single-owner tests back into the exact kind of
/// shared-global race this file's tests already go out of their way to avoid.
pub fn forget_pending_unlocked_delta() {
    PENDING_UNLOCKED_DELTA.store(NO_MOUSE_POSITION, Ordering::Release);
}

/// Split a new absolute position into itself and the movement since the last
/// one. Separate from [`pass_mouse_move`] so the arithmetic — specifically the
/// first-event case — is testable without a loaded engine.
fn mouse_delta(x: f32, y: f32) -> (f32, f32) {
    let previous = MOUSE_LAST.swap(pack_mouse_position(x, y), Ordering::AcqRel);
    if previous == NO_MOUSE_POSITION {
        (0.0, 0.0)
    } else {
        let (px, py) = unpack_mouse_position(previous);
        (x - px, y - py)
    }
}

/// Add one accelerated relative-motion sample to what is waiting for the next
/// absolute position report.
///
/// Called from `relative_pointer_motion` for every `relative_motion` event
/// that arrives while the pointer is *not* locked — see that function for why
/// unlocked motion is acted on at all now. Summed rather than overwritten
/// because a mouse reports at its own rate and `wl_pointer.motion` at the
/// compositor's; a fast mouse or a slow compositor frame can put several
/// relative samples between two absolute ones, and overwriting would silently
/// drop all but the last.
pub fn accumulate_unlocked_delta(dx: f32, dy: f32) {
    let previous = PENDING_UNLOCKED_DELTA.load(Ordering::Acquire);
    let (px, py) =
        if previous == NO_MOUSE_POSITION { (0.0, 0.0) } else { unpack_mouse_position(previous) };
    PENDING_UNLOCKED_DELTA.store(pack_mouse_position(px + dx, py + dy), Ordering::Release);
}

/// Take whatever accelerated delta has accumulated since the last absolute
/// position report, leaving nothing behind for the one after it.
///
/// `None` means no `relative_motion` event has arrived since the last time
/// this was called — either this compositor has no
/// `zwp_relative_pointer_v1`, or (see `relative_pointer_motion`'s ordering
/// note) the matching one has not been written to the wire yet. Either way,
/// [`pass_mouse_move`] falls back to the arithmetic difference of absolute
/// positions, which is what this whole path replaces only when it has
/// something better to offer.
fn take_pending_unlocked_delta() -> Option<(f32, f32)> {
    let previous = PENDING_UNLOCKED_DELTA.swap(NO_MOUSE_POSITION, Ordering::AcqRel);
    (previous != NO_MOUSE_POSITION).then(|| unpack_mouse_position(previous))
}

/// Which delta an absolute position report should carry: the desktop's own
/// accelerated relative motion if one arrived for it, or the arithmetic
/// fallback otherwise.
///
/// Pure and separate from [`pass_mouse_move`] so the precedence is testable
/// without a compositor to send either kind of event — the same reason
/// `vulkan.rs`'s `resolve_present_mode` takes its inputs as plain values
/// rather than reading the environment itself.
fn resolve_mouse_delta(pending: Option<(f32, f32)>, from_position_diff: (f32, f32)) -> (f32, f32) {
    pending.unwrap_or(from_position_diff)
}

/// `nativePassMouseMove(F x, F y, F dx, F dy)`.
///
/// The last two arguments used to be sent as constant zeros, which is the
/// likeliest reason the mouse would not turn the camera: an absolute position
/// says where the cursor is, and a camera is rotated by how far it *moved*. The
/// dex declares `(FFFF)V` and strips parameter names, so "the last two are the
/// delta" is `INFERRED` — but it is the shape this file already assumed when it
/// hardcoded zeros, and a real delta is strictly closer to the truth than a
/// value that says the pointer never moves.
///
/// While the pointer is unlocked, `dx`/`dy` prefer the compositor's own
/// accelerated relative motion over the arithmetic difference of two absolute
/// positions — see [`resolve_mouse_delta`] for the precedence and
/// `relative_pointer_motion` in `wayland.rs` for why the two used to disagree
/// only in theory and the maintainer's report says they do not in practice.
/// `mouse_delta` still runs unconditionally: `MOUSE_LAST` has to track the
/// true absolute position regardless of which delta gets sent, because a
/// later report with no matching relative sample falls back to subtracting
/// from it.
pub fn pass_mouse_move(x: f32, y: f32) {
    let from_position_diff = mouse_delta(x, y);
    let (dx, dy) = resolve_mouse_delta(take_pending_unlocked_delta(), from_position_diff);
    pass_mouse_move_delta(x, y, dx, dy);
}

/// As [`pass_mouse_move`], but with the movement supplied rather than derived
/// from the previous position.
///
/// This is what a captured pointer needs. Under `zwp_pointer_constraints_v1`
/// the cursor stops moving on purpose, so there is no new absolute position to
/// subtract a previous one from — the movement arrives on its own, through
/// `zwp_relative_pointer_v1`, and the absolute pair stays wherever the lock
/// caught it. Subtracting two identical positions would report that the mouse
/// had not moved, which is the same "constant zeros" bug the delta arguments
/// were added to fix, arriving by a different route.
///
/// `MOUSE_LAST` is deliberately left alone: it tracks where the *cursor* is,
/// and while the pointer is locked the cursor is not going anywhere.
pub fn pass_mouse_move_delta(x: f32, y: f32, dx: f32, dy: f32) {
    let f = PASS_MOUSE_MOVE.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativePassMouseMove");
        return;
    }
    let r = cordial_linker_sys::game_activity::pass_mouse_move(f, x, y, dx, dy);
    if trace_mouse() {
        eprintln!("[cordial] nativePassMouseMove(x={x}, y={y}, dx={dx}, dy={dy}) -> {r:?}");
    }
}

/// `nativePassMouseButton(F x, F y, Z down, I button)`.
///
/// `android_button` is the `MotionEvent.BUTTON_*` bit the backend decoded;
/// [`roblox_mouse_button`] turns it into this interface's own index.
///
/// Only the primary button used to be delivered here at all, and always as
/// index 0. Roblox turns the camera on a right-button drag, so a client that
/// never reports a right button cannot turn its camera with the mouse however
/// well the rest of the path works.
pub fn pass_mouse_button(x: f32, y: f32, down: bool, android_button: i32) {
    let Some(button) = roblox_mouse_button(android_button) else {
        return;
    };
    let f = PASS_MOUSE_BUTTON.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativePassMouseButton");
        return;
    }
    let r = cordial_linker_sys::game_activity::pass_mouse_button(f, x, y, down, button);
    if trace_mouse() {
        eprintln!(
            "[cordial] nativePassMouseButton(x={x}, y={y}, down={down}, \
             android={android_button}, roblox={button}) -> {r:?}"
        );
    }
}

/// One wheel movement, through both input paths, in detents.
///
/// This is what both backends call, and it is the whole of the scroll wheel:
/// `nativePassMouseWheel` is a real export that Cordial had never called once,
/// which is why scrolling did nothing anywhere in the client. X11 dropped
/// buttons 4-7 on the floor and Wayland's `wl_pointer.axis` handler was an
/// empty function.
///
/// **The unit is the detent** — one notch of a mouse wheel is 1.0 — because
/// that is the one unit both backends can produce honestly. X11 gives it for
/// free (button 4 *is* one notch); Wayland reports a distance instead and
/// `wayland.rs` converts. `MotionEvent.AXIS_VSCROLL` is documented in the same
/// unit, and Roblox's own `MouseWheel` input reports ±1 per notch, so it is
/// also the likeliest thing the third float wants.
///
/// Sign: positive is away from the user, and positive horizontal is to the
/// right, which is what Android documents for the two scroll axes. Whether
/// Roblox agrees is `INFERRED` — nothing readable declares it, and it is one
/// scroll for a human to settle. `CORDIAL_WHEEL_SCALE` is that experiment
/// without a rebuild: it multiplies both axes, so `-1` inverts and `3` makes
/// each notch scroll three.
///
/// `nativePassMouseWheel` takes one float, not two, so horizontal scroll
/// reaches only the AGDK path. `nativePassMousePan(FFFF)` is the plausible
/// home for it and is not driven here, because "plausible" is how this file
/// acquires bugs that take a session to find.
pub fn wheel(handle: i64, x: f32, y: f32, hscroll: f32, vscroll: f32, event_time_ms: i64) {
    let scale = wheel_scale();
    let (h, v) = (hscroll * scale, vscroll * scale);
    if handle != 0 {
        deliver_scroll(handle, x, y, h, v, event_time_ms);
    }
    let f = PASS_MOUSE_WHEEL.load(std::sync::atomic::Ordering::Relaxed);
    if f.is_null() {
        report_unregistered("nativePassMouseWheel");
    }
    let passed = (!f.is_null()).then(|| cordial_linker_sys::game_activity::pass_mouse_wheel(f, x, y, v));
    if trace_wheel() {
        // The arguments as the engine receives them, and what the call
        // answered. Which of the two paths ran matters as much as the numbers:
        // "the wheel does nothing" has two quite different causes, and this
        // line tells them apart without a debugger.
        eprintln!(
            "[cordial] nativePassMouseWheel(x={x}, y={y}, delta={v}) -> {passed:?}; \
             AGDK ACTION_SCROLL h={h} v={v} handle={handle}"
        );
    }
}

/// `CORDIAL_WHEEL_SCALE=<f>`, applied to both scroll axes. Negative inverts.
///
/// A knob rather than a constant because neither the sign nor the size of a
/// notch is declared anywhere Cordial can read, and both are a single scroll
/// for a human to check. Rejected values fall back to 1.0 loudly: a silently
/// ignored scale reads as "the wheel still does not work".
fn wheel_scale() -> f32 {
    static SCALE: OnceLock<f32> = OnceLock::new();
    *SCALE.get_or_init(|| {
        let Some(v) = std::env::var_os("CORDIAL_WHEEL_SCALE") else {
            return 1.0;
        };
        match v.to_string_lossy().trim().parse::<f32>() {
            Ok(f) if f.is_finite() && f != 0.0 => f,
            _ => {
                eprintln!(
                    "[cordial] CORDIAL_WHEEL_SCALE={} is not a non-zero number; using 1.0",
                    v.to_string_lossy()
                );
                1.0
            }
        }
    })
}

/// `CORDIAL_TRACE_WHEEL=1`. Its own switch for the same reason
/// `CORDIAL_TRACE_TEXT` has one: the question is what Cordial *sent*, and the
/// general trace is documented as ABI-unsafe and aborts the engine.
/// `CORDIAL_TRACE_MOUSE=1` — every pointer call Cordial makes into the engine.
///
/// Added because hovering a game card shows a Play button on Sober and does not
/// here, and nothing could say whether the hover events were arriving at all.
/// `nativePassMouseMove` had never been traced, so "the engine ignores hover"
/// and "the engine is never told about hover" looked identical from outside —
/// the same ambiguity that made the keyboard take days, where four theories all
/// assumed a delivery problem and the answer was an interpretation one.
///
/// Note the argument meaning of `nativePassMouseMove(FFFF)` is INFERRED as
/// `(x, y, dx, dy)`. Four floats and no tell to disambiguate them, unlike
/// `nativePassKeyEvent`, whose missing scan-code slot said what vocabulary it
/// wanted once anyone read it. If hover events arrive and still do nothing, that
/// inference is the first thing to doubt.
pub fn trace_mouse() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_TRACE_MOUSE").is_some())
}

pub fn trace_wheel() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_TRACE_WHEEL").is_some())
}

/// `CORDIAL_TRACE_TEXT=1`. Text entry is the one path where the interesting
/// question is what the host *saw*, not what the engine did, so it gets its own
/// switch rather than riding on the general trace — which is documented as
/// ABI-unsafe and aborts the engine.
/// `CORDIAL_NO_AGDK_TOUCH=1` — deliver pointer input only through Roblox's own
/// `NativeInputInterface`, not also through AGDK's `onTouchEventNative`.
///
/// Both paths are real and the engine consumes both, so one physical click
/// arrives twice. Kept as a control: it was the first suspect for text focus
/// bouncing and was measured *not* to be the cause, and that result is worth
/// being able to reproduce.
fn no_agdk_touch() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_NO_AGDK_TOUCH").is_some())
}

/// Which of the two key paths carries a keystroke. **AGDK's is off by default**,
/// and `CORDIAL_AGDK_KEY=1` puts it back.
///
/// ## Settled by measurement, 2026-08-19
///
/// Every key press used to go to the engine twice, and the comment here used to
/// say that nobody had ever observed one path in isolation. Both have now been
/// run alone against a real session:
///
/// | configuration | result |
/// |---|---|
/// | `nativePassKeyEvent` alone | **works** |
/// | AGDK `onKeyDownNative` alone | **nothing works at all** |
/// | both, focused at startup | works |
/// | both, not focused at startup | only `SPACE` arrives |
///
/// So the engine reads `NativeInputInterface.nativePassKeyEvent` and does not
/// read AGDK's key queue — one of the four numbering schemes this file agonises
/// over was never being consulted. And sending both is not merely redundant: the
/// last row is the bug it caused, where a client started without keyboard focus
/// (a scripted launch, `tools/join-run.sh`) lost everything but the one key that
/// happened to survive.
///
/// That also retires the old note above about `D` being the only key that moved
/// the character and Alt causing a jump. Two deliveries of one press, interpreted
/// differently, was exactly the variable nobody had removed, and removing it is
/// the fix rather than another mapping table.
///
/// AGDK delivery is kept behind the flag rather than deleted because it is the
/// standard Android path and a future engine build may start reading it. It is
/// not carrying anything today.
///
/// **Every key press is delivered to the engine twice**, through AGDK's
/// `onKeyDownNative` and through `NativeInputInterface.nativePassKeyEvent`, and
/// until now there was no way to run either alone. The touch path has had that
/// control since the focus-bounce investigation; the key path never did, so
/// nobody has ever observed one in isolation.
///
/// That matters because the symptom does not look like a mapping error. Both
/// natives are registered, both are called, both receive the correct Android
/// keycodes, and both report the engine consumed them — measured — and yet only
/// `D` moves the character, and Ctrl+Alt makes it *jump*. Jump is `SPACE`, and
/// no encoding of Ctrl or Alt is anywhere near `SPACE` in any of the four
/// numbering schemes in play here, so this is not an off-by-N. Two deliveries
/// of one press, interpreted differently, is the variable nobody has removed.
///
fn no_agdk_key() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    // Inverted: the default is now *not* to send. `CORDIAL_NO_AGDK_KEY` is still
    // honoured so anything scripted against it keeps working.
    *ON.get_or_init(|| std::env::var_os("CORDIAL_AGDK_KEY").is_none())
}

fn no_pass_key() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_NO_PASS_KEY").is_some())
}

/// Whether to acknowledge the keyboard to the engine at all. **Off by default.**
///
/// `updateKeyboardSize(visible=true)` was added to close the text-entry
/// handshake and instead destroys focus. Measured, in trace order:
///
/// ```text
/// textbox focused handle=139759059370112
/// updateKeyboardSize(visible=true)
/// textbox blurred
/// ```
///
/// Focus bounces continuously while it is driven, and a bouncing focus resets
/// the edit buffer between keystrokes because the reseed is generation-driven —
/// which is what made the field appear to clear as you typed. With it
/// suppressed, focus is stable, confirmed by control in the same session.
///
/// It is off rather than deleted because the engine plainly wants *something*
/// to acknowledge a keyboard; the fault is in the arguments or the moment, not
/// in the call existing. The arguments have since been corrected against the
/// real-Android capture — see `report_keyboard_state`, which now sends the
/// baseline that capture actually shows. `CORDIAL_KEYBOARD_REPORT=1` turns it
/// on, which is also the control for testing it. See `docs/NEXT.md` §1.
///
/// **The reason it is still off has changed.** It used to be that the corrected
/// form had never been driven through a live typing session. It has now
/// (2026-08-03, X11, `CORDIAL_SCRIPT` clicking a login field and typing into
/// it): the corrected report does *not* bounce focus, and it does not change
/// anything else either — the engine draws a focused box's text neither with it
/// nor without it, pixel-identical at every step of the same scripted sequence.
/// So it stays off for want of a reason to turn it on rather than for fear of
/// what it did, and turning it on is not a fix for text entry. Do not spend
/// another session on that hypothesis; §1 has the screenshots.
pub fn keyboard_report_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_KEYBOARD_REPORT").is_some())
}

pub fn trace_text() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_TRACE_TEXT").is_some())
}

/// `CORDIAL_TRACE_TEXT_SHOW_PASSWORDS=1` — print what was typed, not just how
/// much of it.
///
/// **The name is the documentation.** The first field anyone debugging this
/// reaches for is Roblox's password box, and `CORDIAL_TRACE_TEXT=1` used to put
/// its contents on the terminal a character at a time and then again in full on
/// every keystroke. Once Ctrl+V is bound it would also print whatever was on
/// the clipboard, which is routinely a password out of a manager and routinely
/// not even this user's. Two other places in this tree logged secrets the same
/// way in the same week — the shell's banner printed a live auth ticket, and
/// `deeplink::describe` printed a whole payload under the words "values not
/// shown" — and both were fixed to names and byte counts.
///
/// Byte counts and caret positions answer every question this switch exists
/// for. The bug it was written for is that characters do not *paint*, which a
/// length answers as well as the text does; where the text itself genuinely
/// matters — a mangled multi-byte character, say — this switch exists and says
/// out loud what turning it on means.
fn trace_text_contents() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_TRACE_TEXT_SHOW_PASSWORDS").is_some())
}

/// How to describe a piece of text in a trace line: its size, or — only when
/// [`trace_text_contents`] is on — the text itself.
pub fn redacted(text: &str) -> String {
    if trace_text_contents() {
        format!("{text:?}")
    } else {
        format!("<{} bytes, {} chars>", text.len(), text.chars().count())
    }
}

/// Whether this key press is "paste".
///
/// Shared by both backends so the shortcut cannot end up meaning one thing on
/// X11 and another on Wayland, and so the one subtlety in it is written down
/// once: **Ctrl+Shift+V is not this**. That is "paste without formatting" in
/// most of the desktop and it is also what a terminal uses for plain paste; a
/// text field that treated the two as the same would be wrong in the case where
/// the difference matters. Alt+V is not this either — that is a menu mnemonic.
///
/// `keysym` rather than an evdev code, because paste lives on whichever
/// physical key the layout calls `v`, which is the whole point of a layout.
/// Both `v` and `V` are accepted: Caps Lock does not turn paste off.
pub fn is_paste_shortcut(keysym: c_ulong, meta: i32) -> bool {
    let ctrl_only = meta & META_CTRL_ON != 0 && meta & (META_SHIFT_ON | META_ALT_ON) == 0;
    ctrl_only && (keysym == 'v' as c_ulong || keysym == 'V' as c_ulong)
}

// ------------------------------------------------------------------ text entry

static TEXT_BUFFER: Mutex<TextField> = Mutex::new(TextField::new());

/// The editing state Cordial keeps on behalf of the engine.
///
/// Android delegates text editing to the IME, and with a hardware keyboard the
/// IME is still in the loop — it receives the key events and commits finished
/// text through the InputConnection. Cordial is that IME here, so it owns the
/// caret as well as the contents. Sending the whole string with the caret
/// pinned to the end is what made typing feel broken: every keystroke dragged
/// the caret back, so arrows and clicking into the middle of a field could not
/// work by construction.
///
/// The caret is counted in `char`s, not bytes, because that is what the engine
/// is told and what a person means by "third character".
///
/// This state is display-server independent by construction: it is driven by
/// committed text and caret movements (`Edit`), which is exactly the vocabulary
/// `zwp_text_input_v3` hands over on Wayland and `XLookupString` approximates
/// on X11. Neither backend needs its own copy.
struct TextField {
    text: String,
    caret: usize,
}

impl TextField {
    const fn new() -> Self {
        TextField { text: String::new(), caret: 0 }
    }

    /// Byte offset of the caret, for slicing.
    fn byte_offset(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.caret)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }

    fn len_chars(&self) -> usize {
        self.text.chars().count()
    }

    fn seed(&mut self, text: String) {
        self.caret = text.chars().count();
        self.text = text;
    }

    /// As [`Self::seed`], but with the caret placed explicitly rather than at
    /// the end — for seeding from `InputConnection.setState`, which reports a
    /// real selection, unlike `showKeyboard`'s byte array which carries no
    /// caret at all. Clamped into range: a stale or out-of-sync `selectionEnd`
    /// from the engine must not panic the char-boundary arithmetic elsewhere
    /// in this struct.
    fn seed_with_caret(&mut self, text: String, caret_chars: i32) {
        let len = text.chars().count();
        self.caret = caret_chars.max(0) as usize;
        if self.caret > len {
            self.caret = len;
        }
        self.text = text;
    }

    fn insert(&mut self, s: &str) {
        let at = self.byte_offset();
        self.text.insert_str(at, s);
        self.caret += s.chars().count();
    }

    /// Delete the character before the caret. False when there is nothing to
    /// delete, so the caller can avoid sending an unchanged state.
    fn backspace(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        self.caret -= 1;
        let at = self.byte_offset();
        self.text.remove(at);
        true
    }

    /// Delete the character at the caret — the `Delete` key, as distinct from
    /// backspace. Without it, correcting a typo means deleting everything after
    /// it too.
    fn delete(&mut self) -> bool {
        if self.caret >= self.len_chars() {
            return false;
        }
        let at = self.byte_offset();
        self.text.remove(at);
        true
    }

    /// Move the caret. Returns whether it moved, so a Left at position zero
    /// does not resend identical state.
    fn move_caret(&mut self, to: Caret) -> bool {
        let before = self.caret;
        self.caret = match to {
            Caret::Left => self.caret.saturating_sub(1),
            Caret::Right => (self.caret + 1).min(self.len_chars()),
            Caret::Home => 0,
            Caret::End => self.len_chars(),
        };
        self.caret != before
    }

    /// `zwp_text_input_v3.delete_surrounding_text`: remove `before` bytes
    /// immediately before the caret and `after` bytes immediately after it.
    ///
    /// The protocol counts in bytes, not characters — deliberately so an IME
    /// never has to know the client's internal representation — but this
    /// buffer is a `String`, so a byte count that does not land on a UTF-8
    /// character boundary would panic on `remove`/slicing rather than
    /// misbehave quietly. Both cuts are clamped to the nearest valid boundary
    /// at or before the requested byte offset, which only ever deletes less
    /// than asked, never more and never a partial codepoint.
    fn delete_surrounding(&mut self, before: usize, after: usize) -> bool {
        let caret_byte = self.byte_offset();

        let start = if before == 0 {
            caret_byte
        } else {
            let want = caret_byte.saturating_sub(before);
            // Walk forward from `want` to the next real boundary rather than
            // backward from `caret_byte`, so a `want` that already landed
            // exactly on a boundary is left alone rather than over-deleting
            // one extra character.
            (want..=caret_byte)
                .find(|&i| self.text.is_char_boundary(i))
                .unwrap_or(caret_byte)
        };
        let end = if after == 0 {
            caret_byte
        } else {
            let want = (caret_byte + after).min(self.text.len());
            (caret_byte..=want)
                .rev()
                .find(|&i| self.text.is_char_boundary(i))
                .unwrap_or(caret_byte)
        };
        if start == end {
            return false;
        }

        let removed_chars_before_caret = self.text[start..caret_byte].chars().count();
        self.text.replace_range(start..end, "");
        self.caret = self.caret.saturating_sub(removed_chars_before_caret);
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Caret {
    Left,
    Right,
    Home,
    End,
}

/// The focus generation `TEXT_BUFFER` was last seeded for. `showKeyboard`
/// bumps the engine-side counter on every focus change; when this falls behind
/// it, the buffer belongs to a box that no longer has focus and is reseeded
/// from whatever the engine says the newly focused box contains.
///
/// Without this, moving from the username field to the password field carries
/// the username into it, and the first keystroke in a pre-filled field appends
/// rather than continues.
static TEXT_GENERATION: Mutex<Option<u32>> = Mutex::new(None);
/// Changes whenever the committed text buffer changes or is reseeded. The
/// Wayland overlay uses this as a cheap invalidation key, avoiding a clone of
/// the entire field and JNI metadata locks on every idle pump tick.
static TEXT_REVISION: AtomicU64 = AtomicU64::new(0);

/// What a key press means to the focused field.
pub enum Edit<'a> {
    Insert(&'a str),
    Backspace,
    Delete,
    Move(Caret),
    /// `zwp_text_input_v3.delete_surrounding_text` — byte counts, not chars.
    /// See [`TextField::delete_surrounding`] for why that distinction is
    /// handled inside the buffer rather than by the caller pre-converting.
    DeleteSurrounding { before_bytes: usize, after_bytes: usize },
}

/// Reseed the buffer when focus has moved since it was last filled, shared by
/// [`edit_text_buffer`] and [`text_buffer_snapshot`] so the two cannot drift
/// into different reseed conditions.
///
/// The *trigger* is still `textbox_generation()` — `showKeyboard`'s
/// focus-change counter, proven in practice (see `docs/NEXT.md` §1's account
/// of the bouncing-focus bug this generation check exists to survive). What
/// changed is the *content* reseeded: `InputConnection.setState` is the
/// engine's own outbound report of what a field contains, and — once at least
/// one has actually arrived — is preferred over `showKeyboard`'s byte array,
/// which is a one-shot snapshot taken only at the moment focus changed and
/// carries no caret at all (`seed_with_caret` uses `setState`'s
/// `selectionEnd`; `showKeyboard`'s path still defaults to the end of the
/// text, via `seed`, as it always has).
///
/// Deliberately *not* done: reseeding on every `ime_state_generation()`
/// change, i.e. treating each `setState` as a live overwrite regardless of
/// focus. `setState` is also how the engine would echo back a state Cordial
/// itself just pushed via `pass_text`/`sync_textbox`, and reseeding on that
/// echo — mid-keystroke, not at a focus boundary — is exactly the shape of
/// feedback loop that produced the focus-bounce bug `keyboard_report_enabled`
/// documents. Restricting the new source to the existing, already-safe reseed
/// boundary avoids reopening that without the interactive test needed to
/// confirm a live-overwrite version does not regress it.
fn reseed_if_needed(buf: &mut TextField) {
    let generation = cordial_linker_sys::game_activity::textbox_generation();
    let mut seen = TEXT_GENERATION.lock().unwrap_or_else(|e| e.into_inner());
    if *seen != Some(generation) {
        if trace_text() {
            // **Lengths only, never the text.** This runs on every focus and
            // one of the boxes it runs for is a password field.
            //
            // Worth keeping rather than deleting after the investigation it was
            // added for: `ime_gen` has been 0 on every run measured so far,
            // meaning `InputConnection.setState` never fires and the
            // `ime_state_*` branch below has never once been taken. If that
            // ever changes, this line is how anyone finds out. `showkb_len` is
            // the branch that does the work, and it is correct -- a box
            // refocused with content in it reports that content's length.
            eprintln!(
                "[cordial] textbox reseed gen={generation} ime_gen={} ime_len={} showkb_len={}",
                cordial_linker_sys::game_activity::ime_state_generation(),
                cordial_linker_sys::game_activity::ime_state_text().chars().count(),
                cordial_linker_sys::game_activity::textbox_text().chars().count(),
            );
        }
        if cordial_linker_sys::game_activity::ime_state_generation() > 0 {
            let text = cordial_linker_sys::game_activity::ime_state_text();
            let (_, selection_end) = cordial_linker_sys::game_activity::ime_state_selection();
            buf.seed_with_caret(text, selection_end);
        } else {
            // No `setState` has landed yet this session — nothing has told
            // Cordial anything through the new path, and treating that as
            // "the field is empty" would wrongly blank a pre-filled box that
            // `showKeyboard`'s snapshot still has correctly.
            buf.seed(cordial_linker_sys::game_activity::textbox_text());
        }
        *seen = Some(generation);
        TEXT_REVISION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Apply one edit to the focused field.
///
/// Returns the contents and caret to send, or `None` when nothing changed —
/// resending identical state on every arrow key at the end of a field makes the
/// engine redraw for no reason.
pub fn edit_text_buffer(edit: Edit<'_>) -> Option<(String, i32)> {
    let mut buf = TEXT_BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    reseed_if_needed(&mut buf);

    let changed = match edit {
        Edit::Insert(s) => {
            // Control characters are not text. A field receives what a person
            // typed, not every key they pressed.
            if s.is_empty() || s.chars().any(|c| c.is_control()) {
                false
            } else {
                buf.insert(s);
                true
            }
        }
        Edit::Backspace => buf.backspace(),
        Edit::Delete => buf.delete(),
        Edit::Move(to) => buf.move_caret(to),
        Edit::DeleteSurrounding { before_bytes, after_bytes } => {
            buf.delete_surrounding(before_bytes, after_bytes)
        }
    };

    changed.then(|| {
        TEXT_REVISION.fetch_add(1, Ordering::Relaxed);
        (buf.text.clone(), buf.caret as i32)
    })
}

/// Cheap invalidation key for consumers which only need a fresh snapshot
/// after the text buffer actually changes.
pub fn text_buffer_revision() -> u64 {
    TEXT_REVISION.load(Ordering::Relaxed)
}

/// The focused field's contents and caret, reseeding first exactly as
/// [`edit_text_buffer`] does, but without requiring an edit to apply.
///
/// The Wayland IME bridge needs this to splice a not-yet-committed preedit
/// string into the caret position for display — that is not an edit to the
/// committed buffer (see `wayland.rs`'s module doc on why preedit is tracked
/// separately), so it cannot go through `edit_text_buffer`, which only ever
/// reports state when something actually changed.
pub fn text_buffer_snapshot() -> (String, i32) {
    let mut buf = TEXT_BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    reseed_if_needed(&mut buf);
    (buf.text.clone(), buf.caret as i32)
}

/// The same reading, taken without touching anything.
///
/// [`text_buffer_snapshot`] reseeds from the engine before it answers, which is
/// right for a caller about to render the field and wrong for one that only
/// wants to know what is in it. The development control surface is the second
/// kind: a query that mutates the buffer it is reporting on is an instrument
/// that changes what it measures, and this project has lost enough time to
/// those already.
pub fn text_buffer_peek() -> (String, i32) {
    let buf = TEXT_BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    (buf.text.clone(), buf.caret as i32)
}

/// Take the editor widget's text as the truth, wholesale.
///
/// **This is the buffer stopping being an editor and becoming a mirror.** The
/// comment on `CORDIAL_NO_TEXT_BUFFER` in `wayland.rs` calls Cordial keeping a
/// shadow copy of a field Roblox owns "a design error, not a feature", and
/// lists what it cost: an empty group clearing the box, characters landing at
/// the end regardless of the caret, and the caret being this side's guess
/// rather than anybody's fact. A real `gtk::Text` now owns the text and does
/// its own editing, so none of those are this buffer's to get wrong any more.
///
/// It is kept rather than deleted because three things still read it --
/// `text_buffer_snapshot` for the overlay, `splice_preedit` for IME display,
/// and `reseed_if_needed` for the engine's own value at a focus boundary -- and
/// because the X11 backend has no editor widget and still edits it the old way.
///
/// The generation is stamped as current so [`reseed_if_needed`] does not
/// immediately overwrite what the user just typed with what the engine last
/// said the box contained.
pub fn adopt_editor_text(text: &str, caret: i32) {
    let mut buf = TEXT_BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    let caret = caret.max(0) as usize;
    let caret = caret.min(text.chars().count());
    if buf.text == text && buf.caret == caret {
        return;
    }
    buf.text = text.to_owned();
    buf.caret = caret;
    *TEXT_GENERATION.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(cordial_linker_sys::game_activity::textbox_generation());
    TEXT_REVISION.fetch_add(1, Ordering::Relaxed);
}

// ------------------------------------------------------------ scripted input
//
// A click and a keystroke Cordial delivers to itself, for the experiments the
// text path cannot otherwise have.
//
// The rule against synthesising input (AGENTS.md, `docs/NEXT.md`'s "how to work
// on this") is about the *compositor*: `XTestFake*`, `ydotool`,
// `wlr-virtual-keyboard` and the RemoteDesktop portal all land on whatever has
// focus, which is the developer's session. Nothing here goes near one. Cordial
// is the client, so these call the same natives the backends' own
// `dispatch_button`/`dispatch_key` call, with the same arguments, one layer
// below the display server. The X11 keycode-to-keysym and the xkb translations
// are the only thing they do not exercise, and those are established (see
// `pass_key_event` on the evdev/AKEYCODE vocabulary that cost days).
//
// This exists because the last open question about text entry — does the engine
// draw a focused box's own text — takes a keystroke to answer, and every
// previous attempt stalled exactly there.

/// The evdev code for an ASCII character, for [`script_type`]'s
/// `nativePassKeyEvent` argument.
///
/// A separate table from [`keysym_to_android`] because the two want different
/// vocabularies and conflating them is the bug documented at length on
/// [`pass_key_event`]: the native takes the *platform's* code, and on Linux
/// that is evdev's. Deliberately small — a scripted run types identifiers and
/// digits, and a character with no entry is dropped rather than guessed at, so
/// a missing one shows up as a missing character rather than as some other key.
fn ascii_to_evdev(c: char) -> Option<i32> {
    const LETTERS: [i32; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    Some(match c {
        'a'..='z' => LETTERS[c as usize - 'a' as usize],
        '1'..='9' => 2 + (c as i32 - '1' as i32),
        '0' => 11,
        ' ' => 57,
        _ => return None,
    })
}

/// One click at a canvas position, through whichever device
/// [`synthetic_device`] says this run is pretending to be.
///
/// As a mouse it is exactly what [`super::window::HostWindow`]'s
/// `dispatch_button` delivers: `ACTION_DOWN`/`ACTION_BUTTON_PRESS`, then the
/// release pair, plus `nativePassMouseButton` on each half. As a finger it is
/// one contact down and up, through the same tracker a `wl_touch` drives, so
/// the pointer-id arithmetic and both natives are exercised rather than
/// bypassed.
pub fn script_click(handle: i64, x: f32, y: f32, now_ms: i64) {
    if synthetic_device() == SyntheticDevice::Finger {
        // No hover. A finger cannot hover, and inventing one would make this a
        // worse model of a touchscreen than the mouse path it is standing in
        // for. `touch_down` is where the surface size the touch native wants
        // comes from, so there is nothing to pass here that the real path does
        // not also pass.
        let surface = super::canvas_size();
        touch_down(handle, SYNTHETIC_CONTACT, x, y, surface, now_ms);
        touch_up(handle, SYNTHETIC_CONTACT, surface, now_ms + 40);
        return;
    }
    // A hover first. Roblox's interface highlights on hover and hit-tests the
    // press against where it believes the pointer is, and a press with no
    // preceding motion is a shape a real mouse never produces.
    deliver_mouse(handle, ACTION_HOVER_MOVE, x, y, 0, 0, now_ms, 0);
    pass_mouse_move(x, y);

    deliver_mouse(handle, ACTION_DOWN, x, y, BUTTON_PRIMARY, 0, now_ms, now_ms);
    deliver_mouse(handle, ACTION_BUTTON_PRESS, x, y, BUTTON_PRIMARY, BUTTON_PRIMARY, now_ms, now_ms);
    pass_mouse_button(x, y, true, BUTTON_PRIMARY);

    let up = now_ms + 40;
    deliver_mouse(handle, ACTION_BUTTON_RELEASE, x, y, 0, BUTTON_PRIMARY, up, now_ms);
    deliver_mouse(handle, ACTION_UP, x, y, 0, 0, up, now_ms);
    pass_mouse_button(x, y, false, BUTTON_PRIMARY);
}

/// The control surface's `move`, routed by device.
///
/// As a mouse this is `nativePassMouseMove` and nothing else, which is what
/// `devctl` has always sent for a bare move -- deliberately not also the AGDK
/// pipe, because `move` is the arm that isolates the two.
///
/// As a finger it is a contact *dragging*, and a `move` for a contact that is
/// not down does nothing at all. That is not a gap: a finger has no hover, so
/// the tracker has no contact to move and drops it (loudly under
/// `CORDIAL_TRACE_TOUCH=1`). An MCP `click`, which pushes a move and then a
/// press and a release, still works -- the move is discarded and the press
/// starts the contact.
pub fn script_move(handle: i64, x: f32, y: f32, now_ms: i64) {
    match synthetic_device() {
        SyntheticDevice::Mouse => pass_mouse_move(x, y),
        SyntheticDevice::Finger => {
            touch_motion(handle, SYNTHETIC_CONTACT, x, y, super::canvas_size(), now_ms)
        }
    }
}

/// The control surface's `down`/`up`, routed by device.
///
/// A finger has no buttons, so anything but the primary is dropped rather than
/// delivered as a contact -- the same reasoning as [`roblox_mouse_button`]
/// returning `None` for the side buttons. A right-click that silently became a
/// tap would be a worse answer than one that visibly does nothing.
pub fn script_button(handle: i64, x: f32, y: f32, down: bool, android_button: i32, now_ms: i64) {
    match synthetic_device() {
        SyntheticDevice::Mouse => pass_mouse_button(x, y, down, android_button),
        SyntheticDevice::Finger if android_button == BUTTON_PRIMARY => {
            let surface = super::canvas_size();
            if down {
                touch_down(handle, SYNTHETIC_CONTACT, x, y, surface, now_ms);
            } else {
                touch_up(handle, SYNTHETIC_CONTACT, surface, now_ms);
            }
        }
        SyntheticDevice::Finger => {
            if trace_touch() {
                eprintln!(
                    "[cordial] scripted button {android_button:#x} has no meaning to a finger; \
                     dropped"
                );
            }
        }
    }
}

/// Type a string into whatever box the engine says has focus, one character at
/// a time, down and up, through every path a real keystroke takes.
///
/// Returns how many characters were delivered to a focused box. Zero with a
/// non-empty argument means no box had focus, which is a result rather than a
/// failure — sending text with no focused box means sending it to handle 0 and
/// the engine drops it in silence.
pub fn script_type(handle: i64, text: &str, now_ms: i64) -> usize {
    let mut delivered = 0;
    for (i, c) in text.chars().enumerate() {
        let keysym = c as c_ulong;
        let evdev = ascii_to_evdev(c);
        let t = now_ms + i as i64 * 60;
        if let (Some(keycode), Some(evdev)) = (keysym_to_android(keysym), evdev) {
            deliver_key(handle, true, keycode, evdev + 8, 0, 0, c as i32, t, t);
            pass_key_event(true, evdev, 0);
        }
        let Some(which) = cordial_linker_sys::game_activity::focused_textbox() else {
            if trace_text() {
                eprintln!("[cordial] script type: no focused textbox");
            }
            continue;
        };
        let mut buf = [0u8; 4];
        if let Some((contents, caret)) = edit_text_buffer(Edit::Insert(c.encode_utf8(&mut buf))) {
            let _ = cordial_linker_sys::game_activity::text_input(handle, &contents, caret, caret);
            pass_text(which, &contents, caret);
            delivered += 1;
        }
        if let (Some(keycode), Some(evdev)) = (keysym_to_android(keysym), evdev) {
            deliver_key(handle, false, keycode, evdev + 8, 0, 0, c as i32, t + 30, t);
            pass_key_event(false, evdev, 0);
        }
    }
    delivered
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole policy table, because the interesting cases are the ones
    /// nobody thinks about: a backend that does not know, and a window that is
    /// unfocused but plainly visible on the other monitor.
    #[test]
    fn the_keepalive_stops_only_where_the_setting_says_it_should() {
        use ThrottleWhen::*;
        // Off never throttles, whatever the window is doing.
        for f in [None, Some(true), Some(false)] {
            for v in [None, Some(true), Some(false)] {
                assert!(keepalive_wanted(Off, f, v), "Off must keep going: {f:?} {v:?}");
            }
        }
        // Unfocused throttles on lost focus and ignores visibility.
        assert!(!keepalive_wanted(Unfocused, Some(false), Some(true)));
        assert!(keepalive_wanted(Unfocused, Some(true), Some(false)));
        // Visible is the default, and the case it exists for: a window on the
        // second monitor, not focused, still being watched.
        assert!(keepalive_wanted(Visible, Some(false), Some(true)));
        assert!(!keepalive_wanted(Visible, Some(false), Some(false)));
        // **Focused but reported not visible keeps running.** This asserted
        // the opposite until 2026-09-05, and the opposite is the bug: mutter
        // leaves `SUSPENDED` set on a window that has been uncovered, so the
        // pair happens to a window the user is looking at and the engine sits
        // at its 1.0/s idle throttle until the flag clears. See
        // `keepalive_wanted` for the measurement.
        assert!(
            keepalive_wanted(Visible, Some(true), Some(false)),
            "a focused window is being looked at, whatever a sticky SUSPENDED says"
        );
        // And the throttle still works where it is meant to: minimised reports
        // both, and so does a window on another workspace.
        assert!(!keepalive_wanted(Visible, Some(false), Some(false)));
        // "Not known" is not "not visible". X11 tracks neither and must keep
        // the behaviour it has always had rather than throttling itself.
        assert!(keepalive_wanted(Visible, None, None));
        assert!(keepalive_wanted(Unfocused, None, None));
    }

    #[test]
    fn paste_is_ctrl_v_and_not_its_neighbours() {
        assert!(is_paste_shortcut('v' as c_ulong, META_CTRL_ON));
        assert!(is_paste_shortcut('V' as c_ulong, META_CTRL_ON | META_CAPS_LOCK_ON));
        // Ctrl+Shift+V is "paste without formatting" everywhere else, and a
        // terminal's plain paste. Not the same key.
        assert!(!is_paste_shortcut('v' as c_ulong, META_CTRL_ON | META_SHIFT_ON));
        assert!(!is_paste_shortcut('v' as c_ulong, META_ALT_ON | META_CTRL_ON));
        assert!(!is_paste_shortcut('v' as c_ulong, 0));
        assert!(!is_paste_shortcut('c' as c_ulong, META_CTRL_ON));
    }

    #[test]
    fn a_trace_line_does_not_carry_the_text_by_default() {
        // The switch is read from the environment once per process, so this
        // asserts the default shape rather than flipping it: a password must
        // not be reconstructible from what this returns.
        let line = redacted("hunter2");
        assert!(!line.contains("hunter2"), "trace line leaked the text: {line}");
        assert!(line.contains('7'), "trace line should still carry the size: {line}");
    }

    #[test]
    fn evdev_codes_are_the_platforms_not_androids() {
        // The one collision that hid the vocabulary bug for days: `d` is 32 in
        // both numbering schemes. Every other letter must differ from its
        // AKEYCODE, or this table has been filled in from the wrong one.
        assert_eq!(ascii_to_evdev('d'), Some(32));
        assert_eq!(ascii_to_evdev('a'), Some(30));
        assert_eq!(keysym_to_android('a' as c_ulong), Some(29));
        assert_eq!(ascii_to_evdev('w'), Some(17));
        assert_eq!(ascii_to_evdev(' '), Some(57));
        assert_eq!(ascii_to_evdev('@'), None);
    }

    #[test]
    fn a_caret_edits_where_it_is_not_at_the_end() {
        // Every keystroke used to send the whole string with the caret pinned to
        // the end, which meant arrows and clicking into the middle of a field
        // could not work however the engine behaved. This is the regression that
        // made typing feel broken rather than absent.
        let mut f = TextField::new();
        f.seed("hello".into());
        assert_eq!(f.caret, 5);
        assert!(f.move_caret(Caret::Home));
        assert_eq!(f.caret, 0);
        f.insert("say ");
        assert_eq!(f.text, "say hello");
        assert_eq!(f.caret, 4);
    }

    #[test]
    fn backspace_and_delete_are_not_the_same_key() {
        // Backspace removes before the caret, Delete at it. Treating Delete as
        // backspace loses the character on the wrong side of the cursor, which
        // is the sort of bug people describe as "it eats my text".
        let mut f = TextField::new();
        f.seed("abc".into());
        f.move_caret(Caret::Home);
        assert!(!f.backspace()); // nothing before the caret
        assert!(f.delete());
        assert_eq!(f.text, "bc");
        assert_eq!(f.caret, 0);
        f.move_caret(Caret::End);
        assert!(f.backspace());
        assert_eq!(f.text, "b");
    }

    #[test]
    fn the_caret_is_counted_in_characters_not_bytes() {
        // The engine is told a character offset. Counting bytes puts the caret
        // mid-codepoint for any non-ASCII input and slices a String there, which
        // panics rather than misbehaving quietly.
        let mut f = TextField::new();
        f.seed("héllo".into());
        assert_eq!(f.caret, 5);
        f.move_caret(Caret::Home);
        f.move_caret(Caret::Right);
        f.move_caret(Caret::Right);
        assert_eq!(f.caret, 2);
        f.insert("X");
        assert_eq!(f.text, "héXllo");
    }

    #[test]
    fn a_caret_move_that_goes_nowhere_reports_no_change() {
        // Left at position zero must not resend identical state; the engine
        // would redraw the field on every held arrow key for nothing.
        let mut f = TextField::new();
        f.seed("ab".into());
        f.move_caret(Caret::Home);
        assert!(!f.move_caret(Caret::Left));
        assert!(f.move_caret(Caret::Right));
        f.move_caret(Caret::End);
        assert!(!f.move_caret(Caret::Right));
    }

    #[test]
    fn delete_surrounding_counts_bytes_not_chars() {
        // "café" is 4 chars but 5 bytes (é is 2 bytes in UTF-8). An IME asking
        // to delete 2 bytes before the caret means "delete é", not "delete fé"
        // — treating the count as chars would delete one codepoint too many.
        let mut f = TextField::new();
        f.seed("café".into());
        assert_eq!(f.caret, 4);
        assert!(f.delete_surrounding(2, 0));
        assert_eq!(f.text, "caf");
        assert_eq!(f.caret, 3);
    }

    #[test]
    fn delete_surrounding_deletes_both_sides_of_the_caret() {
        // set_surrounding_text/delete_surrounding_text lets an IME correct
        // text on either side of where composition is happening, not only
        // backspace-style before the caret.
        let mut f = TextField::new();
        f.seed("hello world".into());
        f.move_caret(Caret::Home);
        for _ in 0..6 {
            f.move_caret(Caret::Right);
        }
        assert_eq!(f.caret, 6); // caret sits just before "world"
        assert!(f.delete_surrounding(6, 2));
        assert_eq!(f.text, "rld");
        assert_eq!(f.caret, 0);
    }

    #[test]
    fn delete_surrounding_clamps_to_a_char_boundary_rather_than_panicking() {
        // A byte count that lands mid-codepoint must not slice the string
        // there — this is the case the doc comment on `delete_surrounding`
        // calls out explicitly, so it gets its own test rather than trusting
        // the boundary-walk to be exercised incidentally.
        let mut f = TextField::new();
        f.seed("café".into()); // caret at 4 chars = byte 5 (é is 2 bytes)
        // Asking for 1 byte lands between é's two bytes, mid-codepoint. The
        // buffer clamps down to the nearest boundary at or after that point
        // — which is the caret itself here — rather than either panicking or
        // deleting more than the 1 byte actually requested. Nothing to
        // delete is therefore the correct, safe answer, not a bug.
        assert!(!f.delete_surrounding(1, 0));
        assert_eq!(f.text, "café");
    }

    #[test]
    fn every_mouse_button_maps_to_its_own_index() {
        // The bug this replaces was not a wrong index, it was no index at all:
        // only the primary button was ever delivered, and always as 0. A test
        // that the three are distinct is what stops a future edit collapsing
        // them back into one.
        assert_eq!(roblox_mouse_button(BUTTON_PRIMARY), Some(0));
        assert_eq!(roblox_mouse_button(BUTTON_SECONDARY), Some(1));
        assert_eq!(roblox_mouse_button(BUTTON_TERTIARY), Some(2));
        assert_eq!(roblox_mouse_button(BUTTON_BACK), None);
        assert_eq!(roblox_mouse_button(BUTTON_FORWARD), None);
    }

    #[test]
    fn the_first_move_after_the_pointer_arrives_has_no_delta() {
        // One test rather than several, because `mouse_delta` reads and writes
        // a process-wide last-position and Rust runs tests in parallel threads
        // — two tests sharing it would race and fail intermittently, which is
        // worse than no test.
        //
        // The case that matters is the first one. A pointer that re-enters the
        // canvas at the far side must not report the width of the window as a
        // single movement: a delta is what turns the camera, so that would
        // snap the view round rather than merely be slightly wrong.
        reset_mouse_delta();
        assert_eq!(mouse_delta(100.0, 50.0), (0.0, 0.0));
        assert_eq!(mouse_delta(103.0, 47.0), (3.0, -3.0));
        assert_eq!(mouse_delta(103.0, 47.0), (0.0, 0.0));
        reset_mouse_delta();
        assert_eq!(mouse_delta(900.0, 47.0), (0.0, 0.0));
        reset_mouse_delta();
    }

    #[test]
    fn unaccelerated_diff_is_only_a_fallback_for_a_relative_sample() {
        // `resolve_mouse_delta` is the whole of the precedence, and it wants no
        // global state at all — the same reason `resolve_present_mode` takes
        // plain values instead of reading the environment.
        assert_eq!(resolve_mouse_delta(Some((5.0, -2.0)), (1.0, 1.0)), (5.0, -2.0));
        assert_eq!(resolve_mouse_delta(None, (1.0, 1.0)), (1.0, 1.0));
    }

    #[test]
    fn pending_unlocked_delta_sums_until_taken_and_then_is_gone() {
        // One test rather than several, for the reason given above
        // `mouse_delta`'s own test: `PENDING_UNLOCKED_DELTA` is process-wide,
        // and parallel tests sharing it would race.
        //
        // A fast mouse can put several `relative_motion` samples between two
        // `wl_pointer.motion` reports — that is what `accumulate_unlocked_delta`
        // is for, and summing rather than overwriting is the property this
        // checks. Taking it has to both return the sum and clear it, or the
        // next absolute position would reapply movement that was already sent.
        forget_pending_unlocked_delta();
        assert_eq!(take_pending_unlocked_delta(), None, "nothing accumulated yet");
        accumulate_unlocked_delta(2.0, -1.0);
        accumulate_unlocked_delta(1.5, 0.5);
        assert_eq!(take_pending_unlocked_delta(), Some((3.5, -0.5)));
        assert_eq!(take_pending_unlocked_delta(), None, "taking drains it");
        // A lock or a canvas transition invalidates whatever was waiting, the
        // same way `reset_mouse_delta` invalidates `MOUSE_LAST` — see
        // `forget_pending_unlocked_delta`'s own comment for why carrying it
        // forward would be the bug that function exists to prevent, arriving
        // through the other producer.
        accumulate_unlocked_delta(9.0, 9.0);
        forget_pending_unlocked_delta();
        assert_eq!(take_pending_unlocked_delta(), None, "reset discards a pending sample");
    }

    /// Characterises a gap `relative_pointer_motion`'s own comment on
    /// ordering used to undersell, calling the residual case "a smear of at
    /// most one report and not a double count". That is only true if a
    /// relative sample never arrives after its *own* physical sample's
    /// absolute report has already been drained. This test is the case where
    /// it does: sample A's absolute report goes out first, using the
    /// arithmetic fallback because nothing has accumulated yet, and only then
    /// does A's own `relative_motion` turn up and accumulate. The next
    /// absolute report, for an unrelated sample B, then drains *A*'s leftover
    /// delta instead of computing its own — A's movement is sent twice and
    /// B's is not sent at all, which is a double count and a drop on that one
    /// pair of reports, not a harmless one-report delay.
    ///
    /// One test, not several, for the reason this file's other
    /// `PENDING_UNLOCKED_DELTA`/`MOUSE_LAST` tests already give: both are
    /// process-wide statics and parallel tests sharing them would race.
    ///
    /// This does not establish that any real compositor delivers events in
    /// this order for consecutive samples — see this test's own name being
    /// cited from `relative_pointer_motion`'s comment in `wayland.rs`, and
    /// `docs/NEXT.md`'s "Ordering was checked rather than assumed" section,
    /// both of which mark that question `INFERRED` rather than observed. What
    /// this test does establish is that the code does not defend against it
    /// if it happens.
    #[test]
    fn a_relative_sample_delivered_after_its_own_absolute_report_corrupts_the_next_one() {
        forget_pending_unlocked_delta();
        reset_mouse_delta();

        // Sample A's absolute report: first-ever position, so the fallback is
        // (0, 0) regardless — nothing to corrupt yet, but it establishes
        // `MOUSE_LAST` for B's diff below.
        let from_diff_a = mouse_delta(10.0, 10.0);
        let (dx_a, dy_a) = resolve_mouse_delta(take_pending_unlocked_delta(), from_diff_a);
        assert_eq!((dx_a, dy_a), (0.0, 0.0), "first report has nothing to diff against");

        // A's own relative sample, arriving late — after A's absolute report
        // already went out and was resolved above.
        accumulate_unlocked_delta(4.0, -2.0);

        // Sample B's absolute report. The true movement since A was
        // (12.0, 8.0); `resolve_mouse_delta` computes that correctly as the
        // fallback, but prefers the stale accumulated value unconditionally.
        let from_diff_b = mouse_delta(22.0, 18.0);
        assert_eq!(from_diff_b, (12.0, 8.0), "B's own true movement, computed correctly");
        let (dx_b, dy_b) = resolve_mouse_delta(take_pending_unlocked_delta(), from_diff_b);
        assert_eq!(
            (dx_b, dy_b),
            (4.0, -2.0),
            "B is sent A's leftover delta instead of its own — the gap this test records"
        );

        forget_pending_unlocked_delta();
        reset_mouse_delta();
    }

    /// `CORDIAL_INPUT_TOUCH` has three states, and the third is the one that
    /// used to be missing.
    ///
    /// It was `e && *e && *e != '0'` in C++ -- unset and `0` were the same
    /// answer, because there was only ever one thing to override. Now that the
    /// seat can say "there is a touchscreen" on its own, `0` has to be able to
    /// contradict it or a hybrid laptop has no way to ask for the desktop
    /// interface. Sober #1577 is that request arriving as a bug report.
    #[test]
    fn the_touch_override_can_say_no_and_not_only_yes() {
        assert_eq!(parse_touch_override(None), None, "unset lets the seat answer");
        assert_eq!(parse_touch_override(Some("1".into())), Some(true));
        assert_eq!(parse_touch_override(Some("0".into())), Some(false));
        assert_eq!(parse_touch_override(Some("off".into())), Some(false));
        assert_eq!(parse_touch_override(Some("true".into())), Some(true));
        // A value that survived a shell expanding to nothing is not a `false`.
        // Reading it as one would turn off a real touchscreen for somebody who
        // meant to say nothing.
        assert_eq!(parse_touch_override(Some("".into())), None);
        assert_eq!(parse_touch_override(Some("  ".into())), None);
    }

    /// The whole of what `PlatformParams.isTouchDevice` is told, including the
    /// two cases where it disagrees with the seat.
    ///
    /// `isTouchDevice` is the one field of the three the engine actually reads
    /// -- measured, twice per cold start -- so this table is the entirety of
    /// what Cordial can say about the machine's peripherals, and getting the
    /// precedence wrong is a client laying out mobile controls for a device
    /// that cannot produce a contact.
    #[test]
    fn what_the_engine_is_told_about_a_touchscreen_follows_the_seat_unless_overridden() {
        // Nothing set: the seat is the answer, both ways.
        assert!(touchscreen_reported(true, false, None));
        assert!(!touchscreen_reported(false, false, None));
        // The override contradicts the seat in both directions.
        assert!(touchscreen_reported(false, false, Some(true)), "a host with no touchscreen can still ask for the mobile interface");
        assert!(!touchscreen_reported(true, false, Some(false)), "a hybrid laptop can ask for the desktop one");
        // `CORDIAL_NO_TOUCH` wins over both, and has to: with it set nothing
        // reaches either touch native, so claiming a touchscreen would be a
        // promise no code path can keep.
        assert!(!touchscreen_reported(true, true, Some(true)));
        assert!(!touchscreen_reported(true, true, None));
    }

    /// The action byte for a two-finger gesture, contact by contact.
    ///
    /// This is the whole of what makes multi-touch different from one finger
    /// repeated, and none of it can be checked on this machine any other way:
    /// there is no touchscreen here, so the alternative to a unit test is
    /// shipping the arithmetic unexercised. The numbers are Android's public
    /// `MotionEvent` constants and the packing is `action | (index << 8)`.
    #[test]
    fn a_second_finger_arrives_and_leaves_as_a_pointer_action() {
        let mut t = TouchContacts::default();

        let first = t.down(7, 10.0, 20.0, 1_000).expect("a first contact is accepted");
        assert_eq!(first.action, ACTION_DOWN, "the first contact is a plain down, not 0x0005");
        assert_eq!(first.contacts, vec![TouchContact { id: 0, x: 10.0, y: 20.0 }]);
        assert_eq!(first.down_time_ms, 1_000);

        let second = t.down(9, 30.0, 40.0, 1_050).expect("a second contact is accepted");
        assert_eq!(second.action, ACTION_POINTER_DOWN | (1 << 8));
        assert_eq!(second.contacts.len(), 2);
        assert_eq!(
            second.down_time_ms, 1_000,
            "down time is the gesture's, not this contact's -- Android measures a \
             long press from the first finger"
        );

        // The *first* finger lifts while the second stays. Its index is 0, and
        // the contact that is leaving must still be in the array.
        let up = t.up(7).expect("a tracked contact can lift");
        assert_eq!(up.action, ACTION_POINTER_UP);
        assert_eq!(up.contacts.len(), 2, "the departing pointer is reported in its own event");

        // The last one out is a plain up.
        let last = t.up(9).expect("the remaining contact can lift");
        assert_eq!(last.action, ACTION_UP);
        assert_eq!(last.contacts.len(), 1);
    }

    /// A pointer id freed in the middle is handed out again.
    ///
    /// Not tidiness: an engine keeping a slot per pointer id would otherwise
    /// acquire one per contact for the length of a session, and a pinch makes
    /// contacts by the hundred.
    #[test]
    fn a_freed_pointer_id_is_reused_before_a_new_one_is_invented() {
        let mut t = TouchContacts::default();
        t.down(1, 0.0, 0.0, 0);
        t.down(2, 0.0, 0.0, 0);
        t.down(3, 0.0, 0.0, 0);
        assert_eq!(t.snapshot().iter().map(|c| c.id).collect::<Vec<_>>(), vec![0, 1, 2]);
        t.up(2);
        let next = t.down(4, 0.0, 0.0, 0).expect("a fourth contact is accepted");
        assert_eq!(next.pass[0].0.id, 1, "the middle id came free and is reused, not made 3");
        // The pointer *index* of that contact is 2, because it was appended --
        // which is exactly the case where index and id disagree.
        assert_eq!(next.action, ACTION_POINTER_DOWN | (2 << 8));
    }

    /// Events for contacts this side never saw are dropped, not guessed at.
    ///
    /// The compositor delivers touches aimed at GTK's header bar to Cordial's
    /// `wl_touch` as well, and `wayland.rs` forwards only the ones that landed
    /// on the canvas. That leaves the `motion` and `up` of a foreign contact
    /// arriving here with no matching down, and it is this guard -- not a
    /// second table in the backend -- that stops them.
    #[test]
    fn an_event_for_an_untracked_contact_produces_nothing() {
        let mut t = TouchContacts::default();
        assert_eq!(t.motion(5, 1.0, 1.0), None);
        assert_eq!(t.up(5), None);
        assert_eq!(t.cancel(), None, "a cancel with nothing on the glass is not an event");
        t.down(5, 1.0, 1.0, 0);
        assert_eq!(t.down(5, 2.0, 2.0, 0), None, "a second down for the same contact is dropped");
    }

    /// A cancel closes every contact on both paths.
    ///
    /// `nativePassInput` has no cancel action anything here has established, so
    /// the per-contact half reports each one up. Saying nothing instead would
    /// leave the engine holding fingers no later event could ever close, which
    /// is the stuck-camera shape `pointer_leave` already had to fix once for
    /// mouse buttons.
    #[test]
    fn a_cancel_closes_every_contact() {
        let mut t = TouchContacts::default();
        t.down(1, 0.0, 0.0, 0);
        t.down(2, 0.0, 0.0, 0);
        let c = t.cancel().expect("two contacts are on the glass");
        assert_eq!(c.action, ACTION_CANCEL);
        assert_eq!(c.contacts.len(), 2);
        assert_eq!(c.pass.len(), 2);
        assert!(c.pass.iter().all(|(_, action)| *action == TOUCH_UP));
        assert_eq!(t.snapshot(), vec![], "nothing is left tracked");
    }

    #[test]
    fn a_reported_snapshot_does_not_require_a_change_to_reflect_state() {
        // `text_buffer_snapshot` exists precisely because `edit_text_buffer`
        // only reports when something changed; the preedit splice needs the
        // current state unconditionally, including when nothing has been
        // typed into this field yet.
        let mut f = TextField::new();
        f.seed("draft".into());
        assert_eq!((f.text.clone(), f.caret as i32), ("draft".to_string(), 5));
    }

    // Distinct codes per test on purpose: `FORWARDED_PRESSES` is a process
    // static and these run in parallel in one binary, so sharing a code between
    // two tests would make them flaky in a way that looks like a real bug.

    /// The pairing itself. A release forwards only if its own press did.
    #[test]
    fn a_release_is_paired_to_the_press_that_was_forwarded() {
        let code = 9001;
        assert!(!take_forwarded_press(code), "nothing was pressed yet");
        remember_forwarded_press(code);
        assert!(take_forwarded_press(code), "its press was forwarded");
    }

    /// Consuming, not peeking.
    ///
    /// If the record survived, a release whose press was suppressed could ride
    /// on a stale entry from an earlier press of the same key, which is exactly
    /// the Sober #987 reopening this guard exists to stop.
    #[test]
    fn one_press_releases_exactly_once() {
        let code = 9002;
        remember_forwarded_press(code);
        assert!(take_forwarded_press(code));
        assert!(!take_forwarded_press(code), "the record must not survive its release");
    }

    /// A key held down does not get two records, or the second release would
    /// also be treated as paired.
    #[test]
    fn repeated_presses_of_a_held_key_record_once() {
        let code = 9003;
        remember_forwarded_press(code);
        remember_forwarded_press(code);
        assert!(take_forwarded_press(code));
        assert!(!take_forwarded_press(code));
    }

    /// Keys are tracked independently, so releasing one does not free another.
    #[test]
    fn one_keys_release_does_not_answer_for_another(){
        let (a, b) = (9004, 9005);
        remember_forwarded_press(a);
        assert!(!take_forwarded_press(b), "b was never pressed");
        assert!(take_forwarded_press(a));
    }

    /// The suppression test itself, which decides whether the guard applies at
    /// all. "/" is 53 and has to be in it, because that is the key the whole
    /// bug is about; Escape and Enter must not be, because they are how
    /// somebody leaves a box.
    #[test]
    fn the_guard_covers_character_keys_and_not_the_ways_out_of_a_box() {
        assert!(evdev_is_text_key(53), "slash");
        assert!(evdev_is_text_key(57), "space");
        assert!(evdev_is_text_key(30), "a");
        assert!(!evdev_is_text_key(1), "escape");
        assert!(!evdev_is_text_key(28), "enter");
        assert!(!evdev_is_text_key(15), "tab");
    }
}
