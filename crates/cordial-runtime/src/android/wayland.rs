//! The native Wayland backend.
//!
//! [ADR-011](../../../../docs/adr/ADR-011-wayland-and-libadwaita.md) makes
//! Wayland the target and X11 the diagnostic fallback. `window.rs` dlopens
//! libX11 rather than linking it, so the loader and asset tests still run
//! with no display at all; this file follows the same rule for
//! `libwayland-client.so.0`, `libwayland-egl.so.1` and `libxkbcommon.so.0`.
//!
//! ## One hand-written protocol table is left, and that is one too many
//!
//! `wl_proxy_marshal_flags` needs a `wl_interface` table to know how many
//! arguments a request takes and of what type, and the core protocol's tables
//! are compiled into `libwayland-client.so` and exported as data symbols —
//! `dlsym` reaches them the same way it reaches `XOpenDisplay` in `window.rs`.
//!
//! This file used to hand-write two protocols that nothing exports, because no
//! `wayland-protocols` XML was available to generate from. `xdg_shell` is now
//! gone: GTK owns the `xdg_toplevel`, and the subsurface role this file uses
//! instead is core protocol. `text-input-unstable-v3` is still hand-written,
//! and is the one interface below whose event count is this file's assertion
//! rather than the library's — see the note on its live bug further down, and
//! notice that the failure mode it shows is precisely the failure mode a
//! hand-written table has.
//!
//! **Do not add another.** A signature that is wrong by one argument makes
//! `wl_proxy_marshal_flags` read the wrong number or type of variadic
//! arguments and corrupts the wire, and this file's comments record a crash of
//! exactly that family. If a further protocol is genuinely needed
//! here, generate it — or take it from GTK, which is in the process now.
//!
//! **Two more were added anyway, and this paragraph is the justification owed
//! for it.** `pointer-constraints-unstable-v1` and
//! `relative-pointer-unstable-v1` are what a locked pointer is made of, and
//! neither of the two escapes above was open. Taking them from GTK does not
//! work: `nm -D` on this host's `libgtk-4.so.1` exports no
//! `zwp_pointer_constraints_v1_interface`, and `strings` finds neither
//! interface name anywhere in the library, so GDK4 does not speak either
//! protocol and has no table to borrow. Generating them needs
//! `wayland-protocols`' XML, and this host has no `/usr/share/wayland-protocols`
//! at all — a build-time dependency on a package the developer does not have
//! installed is a worse trade than four small tables.
//!
//! So the mitigation is the one the failure above actually calls for. Every
//! signature below was taken from `wayland-scanner private-code` run over the
//! upstream XML and copied from its output rather than written from the
//! protocol description, all four interfaces are version 1 with no `since`
//! prefixes to get wrong, and `pointer_constraints_tables_match_wayland_scanner`
//! in the tests pins each signature string and every count against what that
//! generator emitted. That is a weaker guarantee than generating at build time
//! and a much stronger one than reading the XML and typing what it seemed to
//! say.
//!
//! ## The engine's surface is a subsurface, not a toplevel
//!
//! This file used to give the engine an `xdg_toplevel` of its own, which made
//! the engine's canvas *the whole window*: no titlebar, no client-side
//! decorations, and nowhere to put anything of Cordial's beside the canvas.
//! ADR-011 already said the window is GTK4 + libadwaita and that the shell's
//! window and this one "are the same window"; a bare toplevel here was the
//! part that had not caught up.
//!
//! So GTK owns the toplevel now — [`cordial_shell::host_window`], the same
//! definition the shell binary uses — and the engine's `wl_surface` is a
//! `wl_subsurface` of it, positioned over the window's content area.
//! Consequences worth knowing before changing anything here:
//!
//! GTK's `wl_display` is the *only* connection in the process. Wayland object
//! ids are scoped to the connection that made them, so a subsurface cannot
//! parent to a surface on another one; `open` takes GDK's display rather than
//! calling `wl_display_connect`, and Mesa is handed the same pointer (see
//! `egl_get_display`, whose comment on the second-connection hazard now
//! applies to GDK's connection rather than to one of this file's own).
//!
//! `wl_subsurface.set_desync` is not optional. A subsurface starts
//! *synchronised*, meaning its commits do not take effect until the parent
//! commits — the engine would present frames that appear only when GTK
//! happened to repaint, which for a static window is never.
//!
//! `set_position` is the mirror image: it *is* latched on the parent's commit,
//! so moving the canvas needs GTK to repaint afterwards
//! (`HostWindow::queue_commit`).
//!
//! Input is filtered by surface. Cordial's `wl_pointer` is a second pointer
//! object on the same seat as GDK's, so it sees `enter` for the header bar
//! too; forwarding those to the engine would have it react to clicks on the
//! window controls. `pointer_enter` records which surface the pointer is on
//! and nothing is delivered unless it is the engine's.
//!
//! ## A web-view dialog is invisible by default, and this is why
//!
//! **Reported as "opening the server browser turns the whole window
//! white/blank", and it is neither white nor blank.** A first pass at this
//! screenshotted a nested compositor and saw the real page, no engine canvas
//! in the way, and wrote up "does not reproduce" -- wrong, and wrong for a
//! reason worth naming: a nested compositor gives every one of
//! its clients its own fresh stacking order, so the bug this file has cannot
//! show up inside one. The tenth instrument fault of this kind recorded here;
//! see AGENTS.md and `docs/adr/ADR-011-wayland-and-libadwaita.md`'s own list.
//! Confirmed instead by the maintainer, on the real desktop, undialled.
//!
//! The actual mechanism is `wl_subsurface` stacking, and it is exactly what
//! this file already says about `get_subsurface`: the engine's canvas is
//! "positioned over the window's content area". A newly created subsurface's
//! default z position is immediately above its parent, and nothing in this
//! file ever moved it -- `grep` for `place_above`/`place_below` before this
//! change found neither. An `AdwDialog` is not a second `xdg_toplevel`; per
//! `cordial_shell::webview`'s own module doc it "draws inside its parent's
//! own surface", meaning inside GTK's *toplevel* `wl_surface` -- the very
//! surface the engine's subsurface sits in front of by default. So the
//! moment a web-view dialog opens, it is painted correctly, into a surface
//! that is there, and then the engine's own canvas -- still compositing
//! above it every frame -- covers it completely. What the user sees is the
//! engine's ordinary rendering, blank because Roblox itself stops drawing
//! its own content once it believes a web view is covering it (the
//! maintainer's own diagnosis, and the reason "blank" rather than "white" is
//! the more precise word).
//!
//! The fix is [`WaylandWindow::webview_dialog_opened`] /
//! [`WaylandWindow::webview_dialog_closed`]: `place_below`/`place_above` the
//! engine's subsurface against `parent_surface` for exactly as long as at
//! least one dialog is open, reference-counted rather than a bare flag so a
//! second dialog opening while the first is still up cannot re-lower an
//! already-lowered canvas, and closing one of two cannot raise it back too
//! early. **Never leave it lowered permanently** -- it is above by default
//! for the ordinary case, which is Roblox running with no dialog open, and a
//! permanently-lowered canvas would be invisible then instead.
//!
//! Like `set_position`, `place_above`/`place_below` are requests on the
//! *subsurface* but are double-buffered on the *parent's* next commit (the
//! Wayland protocol XML says so explicitly for reordering requests, the same
//! paragraph that says it for `set_position`). `set_engine_stacking` follows
//! `sync_canvas_geometry`'s own fix for that -- `HostWindow::queue_commit`
//! plus an explicit `wl_surface.commit` on `parent_surface` right here,
//! because asking GTK to redraw is not the same as GTK having redrawn (see
//! `sync_canvas_geometry`'s "issue #7" comment for the full account of why
//! the direct commit is the one that is not optional).
//!
//! ## `zwp_text_input_v3` had a version-2 event table written to version 1
//!
//! **Correction to what this comment used to say.** It recorded `interface
//! 'zwp_text_input_v3' has no event 8` as a live bug and explained it as
//! "event 8 exists in `zwp_text_input_v2`", a different protocol. That is
//! wrong, and the real explanation is entirely inside this file.
//!
//! `zwp_text_input_v3` **version 2** — which is what GNOME 50's mutter
//! advertises, and what the `bind` below has always asked for — adds three
//! events to the six version 1 has: `action` (6), `language` (7) and
//! `preedit_hint` (8). Event 8 is `preedit_hint`. The table here declared six.
//! An object's version on the wire is inherited from the object that created
//! it, and nothing about passing a smaller number to `wl_proxy_marshal_flags`
//! changes what the compositor believes it may send — so binding the manager
//! at 2 and describing the child at 1 asks for events this file then cannot
//! receive.
//!
//! Measured on the wire, `WAYLAND_DEBUG=1`, before the fix:
//!
//! ```text
//! wl_registry#107.global(26, "zwp_text_input_manager_v3", 2)
//!  -> wl_registry#107.bind(26, "zwp_text_input_manager_v3", 2, new id [unknown]#74)
//!  -> zwp_text_input_manager_v3#74.get_text_input(new id [unknown]#71, wl_seat#103)
//! zwp_text_input_v3#71.enter(wl_surface#47)
//! ```
//!
//! Note the last line: the compositor starts talking to this object as soon as
//! the toplevel takes keyboard focus, with no `enable` sent and no field
//! clicked. There is no window in a session where it is dormant.
//!
//! The failure that follows is *not* a protocol error and this is worth being
//! precise about, because a wrong errno sends the next person somewhere else.
//! Reproduced standalone against this same compositor, by binding `wl_seat` at
//! version 8 behind a deliberately one-event table:
//!
//! ```text
//! interface 'wl_seat' has no event 1
//! roundtrip=-1  wl_display_get_error=11 (Resource temporarily unavailable)
//! ```
//!
//! libwayland refuses the event, puts the *whole display* into a permanent
//! error state, and leaves `errno` at whatever it was — 11, not 71. Every
//! client on the connection stops, which is the freeze. A `wl_display.error`
//! sent by the compositor is the other thing, gives 71 (`EPROTO`), and prints
//! `<interface>#<id>: error <code>: <reason>` first.
//!
//! The rule this file already applied to `wl_pointer`/`wl_keyboard` is the fix,
//! plus its converse: declare the complete event set for the version bound, and
//! never bind above the version whose table is written here. `bind` now takes
//! its version from `TEXT_INPUT_MANAGER_INTERFACE.version` so the two cannot
//! drift apart when wayland-protocols ships a version 3.
//!
//! One thing established earlier and still true, from `WAYLAND_DEBUG=1`:
//! bringing GTK into the process does not add a second text-input object.
//! There is exactly one `get_text_input` on the connection and it is this
//! file's, because GDK creates its own only when a GTK text widget takes
//! focus, and this window has none. That stops being true the moment anything
//! focusable-and-editable is added to the window — two `zwp_text_input_v3`
//! objects on one seat from one client would be a new and much harder bug, and
//! whoever adds an editor widget here has to resolve which of the two speaks
//! for Cordial before doing it.
//!
//! **That happened on 2026-08-25, and the answer is GTK's.** The overlay is now
//! a real `gtk::Text` placed on the focused box, so GDK does create a second
//! `zwp_text_input_v3`, and this file's is no longer enabled while a box has
//! focus -- see `sync_ime_focus`. The widget is the one holding the text, the
//! caret rectangle and the surrounding context, which is everything an input
//! method is given in order to do its job, so it is the one that should have
//! them. This file's object still exists and still receives `enter`; it is
//! simply dormant, and is kept rather than destroyed so that whether two of
//! them on one seat is tolerated stays a question this can answer by running.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use libadwaita as adw;
use gtk4::prelude::GtkWindowExt;

// ------------------------------------------------------------- wire layout
//
// `struct wl_interface`, from `wayland-util.h`. Only the type is needed now,
// as the parameter `wl_proxy_marshal_flags` takes and the type of the tables
// `dlsym` hands back; nothing here declares one. Its layout is part of
// libwayland-client's stable ABI — every language binding for Wayland depends
// on it not changing.

#[repr(C)]
struct WlInterface {
    name: *const c_char,
    version: c_int,
    method_count: c_int,
    methods: *const WlMessage,
    event_count: c_int,
    events: *const WlMessage,
}
// SAFETY: as `WlMessage` below.
unsafe impl Sync for WlInterface {}


// ------------------------------------------------- hand-written wire layout
//
// `struct wl_message`/the null `types` array, from `wayland-util.h`. Needed
// only by the two `zwp_text_input` tables below — every other interface this
// file uses comes out of `libwayland-client.so` itself. `wl_proxy_marshal_
// flags` gets a *new* object's interface from the explicit `interface`
// parameter at the call site rather than from `types[]`, which is read only
// for `WAYLAND_DEBUG` printing and for auto-creating a proxy for an incoming
// `new_id` event argument — `zwp_text_input_v3` has no such event, so the
// null fill costs a `(nil)` in debug output and nothing else.

#[repr(C)]
struct WlMessage {
    name: *const c_char,
    signature: *const c_char,
    types: *const *const WlInterface,
}
// SAFETY: every field is a pointer either to a `'static` C string literal or
// to another `'static` table defined in this file; nothing here is mutated
// after the enclosing `static` is initialised.
unsafe impl Sync for WlMessage {}

#[repr(C)]
struct NullTypes([*const WlInterface; 8]);
// SAFETY: all-null, never mutated.
unsafe impl Sync for NullTypes {}
static NO_TYPES: NullTypes = NullTypes([std::ptr::null(); 8]);

// ------------------------------------------------- zwp_text_input_manager_v3

static TEXT_INPUT_MANAGER_METHODS: [WlMessage; 2] = [
    WlMessage { name: c"destroy".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage {
        name: c"get_text_input".as_ptr(),
        signature: c"no".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
];
static TEXT_INPUT_MANAGER_INTERFACE: WlInterface = WlInterface {
    name: c"zwp_text_input_manager_v3".as_ptr(),
    // Version 2. The manager's own request set is unchanged from 1 — the bump
    // is on `zwp_text_input_v3` — but binding higher is *not* free, and this
    // number is the one that decides what the compositor may send on the child
    // object: a `zwp_text_input_v3` created by a v2 manager is a v2 object
    // however small a version this file passes to `wl_proxy_marshal_flags`.
    // `TEXT_INPUT_EVENTS` below therefore has to be complete for this number,
    // and `bind` reads it from here rather than repeating the literal, so the
    // table and the request cannot drift. `bind` still clamps to what the
    // compositor advertised, so a v1-only compositor works unchanged.
    version: 2,
    method_count: 2,
    methods: TEXT_INPUT_MANAGER_METHODS.as_ptr(),
    event_count: 0,
    events: std::ptr::null(),
};

const TEXT_INPUT_MANAGER_GET_TEXT_INPUT: u32 = 1;

// ------------------------------------------------------------ zwp_text_input_v3

static TEXT_INPUT_METHODS: [WlMessage; 11] = [
    WlMessage { name: c"destroy".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage { name: c"enable".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage { name: c"disable".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage {
        name: c"set_surrounding_text".as_ptr(),
        signature: c"sii".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage {
        name: c"set_text_change_cause".as_ptr(),
        signature: c"u".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage {
        name: c"set_content_type".as_ptr(),
        signature: c"uu".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage {
        name: c"set_cursor_rectangle".as_ptr(),
        signature: c"iiii".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage { name: c"commit".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
    // Version 2's three additions. None of them is sent by this file, and
    // `set_available_actions` in particular must not be sent casually — an
    // array containing `none`, or the same action twice, is the interface's own
    // `invalid_action` protocol error and would kill the connection. They are
    // declared so that `method_count` matches the version bound, in the same
    // spirit as the event table below: a table that describes a different
    // protocol version from the one on the wire is the bug this whole section
    // is about.
    WlMessage {
        name: c"set_available_actions".as_ptr(),
        signature: c"2a".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage { name: c"show_input_panel".as_ptr(), signature: c"2".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage { name: c"hide_input_panel".as_ptr(), signature: c"2".as_ptr(), types: NO_TYPES.0.as_ptr() },
];
static TEXT_INPUT_EVENTS: [WlMessage; 9] = [
    WlMessage { name: c"enter".as_ptr(), signature: c"o".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage { name: c"leave".as_ptr(), signature: c"o".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage {
        name: c"preedit_string".as_ptr(),
        signature: c"?sii".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage {
        name: c"commit_string".as_ptr(),
        signature: c"?s".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage {
        name: c"delete_surrounding_text".as_ptr(),
        signature: c"uu".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage { name: c"done".as_ptr(), signature: c"u".as_ptr(), types: NO_TYPES.0.as_ptr() },
    // Version 2, and where the recorded `has no event 8` came from. The
    // leading `2` in each
    // signature is the `since` version, exactly as `wayland-scanner` emits it
    // (`wl_seat`'s own `name` event reads `2s` in the host library); the
    // demarshaller skips it, so it is documentation that cannot go stale.
    WlMessage { name: c"action".as_ptr(), signature: c"2uu".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage { name: c"language".as_ptr(), signature: c"2s".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage { name: c"preedit_hint".as_ptr(), signature: c"2uuu".as_ptr(), types: NO_TYPES.0.as_ptr() },
];
static TEXT_INPUT_INTERFACE: WlInterface = WlInterface {
    name: c"zwp_text_input_v3".as_ptr(),
    // Must equal the manager's, because that is what the compositor gives this
    // object — see the module doc.
    version: 2,
    method_count: 11,
    methods: TEXT_INPUT_METHODS.as_ptr(),
    event_count: 9,
    events: TEXT_INPUT_EVENTS.as_ptr(),
};

const TEXT_INPUT_ENABLE: u32 = 1;
const TEXT_INPUT_DISABLE: u32 = 2;
const TEXT_INPUT_SET_SURROUNDING_TEXT: u32 = 3;
const TEXT_INPUT_SET_CONTENT_TYPE: u32 = 5;
const TEXT_INPUT_SET_CURSOR_RECTANGLE: u32 = 6;
const TEXT_INPUT_COMMIT: u32 = 7;


// ------------------------------------------- zwp_pointer_constraints_v1
//
// The locked pointer, which is what a first-person camera and a
// right-button camera drag both are on a desktop: the cursor stops moving,
// stops being able to leave the window, and the client is fed relative motion
// instead. Confinement — the other half of this protocol — keeps the cursor
// visible inside a region and is not what either wants, so `confine_pointer`
// is declared for the method count and never sent.
//
// Every signature here is `wayland-scanner private-code`'s own output over
// `pointer-constraints-unstable-v1.xml`, not a transcription of the XML. See
// the module doc for why this file grew two hand-written protocols after
// telling the reader not to.

static POINTER_CONSTRAINTS_METHODS: [WlMessage; 3] = [
    WlMessage { name: c"destroy".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage {
        name: c"lock_pointer".as_ptr(),
        signature: c"noo?ou".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage {
        name: c"confine_pointer".as_ptr(),
        signature: c"noo?ou".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
];
static POINTER_CONSTRAINTS_INTERFACE: WlInterface = WlInterface {
    name: c"zwp_pointer_constraints_v1".as_ptr(),
    version: 1,
    method_count: 3,
    methods: POINTER_CONSTRAINTS_METHODS.as_ptr(),
    event_count: 0,
    events: std::ptr::null(),
};

const POINTER_CONSTRAINTS_LOCK_POINTER: u32 = 1;

// ---------------------------------------------- zwp_locked_pointer_v1

static LOCKED_POINTER_METHODS: [WlMessage; 3] = [
    WlMessage { name: c"destroy".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage {
        name: c"set_cursor_position_hint".as_ptr(),
        signature: c"ff".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    WlMessage {
        name: c"set_region".as_ptr(),
        signature: c"?o".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
];
static LOCKED_POINTER_EVENTS: [WlMessage; 2] = [
    WlMessage { name: c"locked".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage { name: c"unlocked".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
];
static LOCKED_POINTER_INTERFACE: WlInterface = WlInterface {
    name: c"zwp_locked_pointer_v1".as_ptr(),
    version: 1,
    method_count: 3,
    methods: LOCKED_POINTER_METHODS.as_ptr(),
    event_count: 2,
    events: LOCKED_POINTER_EVENTS.as_ptr(),
};

const LOCKED_POINTER_DESTROY: u32 = 0;
const LOCKED_POINTER_SET_CURSOR_POSITION_HINT: u32 = 1;

/// `zwp_pointer_constraints_v1.lifetime.persistent`. The alternative,
/// `oneshot` (1), makes the compositor destroy the constraint the first time it
/// deactivates — and it deactivates on every alt-tab. Persistent means the lock
/// comes back when the window is focused again, which is what a first-person
/// camera wants; the escape path is Cordial destroying the object, not the
/// compositor happening to.
const POINTER_CONSTRAINT_LIFETIME_PERSISTENT: u32 = 2;

/// `WL_MARSHAL_FLAG_DESTROY`. `wl_proxy_marshal_flags`'s `flags` argument, which
/// every other call in this file passes 0 for. A request declared
/// `type="destructor"` in the XML has to be sent with this or the proxy leaks:
/// the request reaches the compositor either way, and the client-side object is
/// what is left dangling.
const WL_MARSHAL_FLAG_DESTROY: u32 = 1;

// ------------------------------------------ zwp_relative_pointer_manager_v1
//
// A locked pointer stops producing `wl_pointer.motion` entirely — that is the
// point of it — so without this the lock would silence the camera rather than
// free it. `relative_motion` is the replacement, and it is the only place
// pointer movement comes from while the lock is active.

static RELATIVE_POINTER_MANAGER_METHODS: [WlMessage; 2] = [
    WlMessage { name: c"destroy".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() },
    WlMessage {
        name: c"get_relative_pointer".as_ptr(),
        signature: c"no".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
];
static RELATIVE_POINTER_MANAGER_INTERFACE: WlInterface = WlInterface {
    name: c"zwp_relative_pointer_manager_v1".as_ptr(),
    version: 1,
    method_count: 2,
    methods: RELATIVE_POINTER_MANAGER_METHODS.as_ptr(),
    event_count: 0,
    events: std::ptr::null(),
};

const RELATIVE_POINTER_MANAGER_GET_RELATIVE_POINTER: u32 = 1;

static RELATIVE_POINTER_METHODS: [WlMessage; 1] =
    [WlMessage { name: c"destroy".as_ptr(), signature: c"".as_ptr(), types: NO_TYPES.0.as_ptr() }];
static RELATIVE_POINTER_EVENTS: [WlMessage; 1] = [WlMessage {
    name: c"relative_motion".as_ptr(),
    signature: c"uuffff".as_ptr(),
    types: NO_TYPES.0.as_ptr(),
}];
static RELATIVE_POINTER_INTERFACE: WlInterface = WlInterface {
    name: c"zwp_relative_pointer_v1".as_ptr(),
    version: 1,
    method_count: 1,
    methods: RELATIVE_POINTER_METHODS.as_ptr(),
    event_count: 1,
    events: RELATIVE_POINTER_EVENTS.as_ptr(),
};

// ----------------------------------------------------- subsurface opcodes
//
// `wl_subcompositor`/`wl_subsurface` are core protocol, so their
// `wl_interface` tables come out of `libwayland-client.so` itself like every
// other interface this file touches — see `WlClient::load`. Only the opcode
// numbers, fixed by `wayland.xml`, need naming.
const WL_SUBCOMPOSITOR_GET_SUBSURFACE: u32 = 1;
const WL_SUBSURFACE_SET_POSITION: u32 = 1;
// `place_above`/`place_below` take the sibling to reorder against as their
// one argument -- see `WaylandWindow::set_engine_stacking` for why the
// argument passed is always `parent_surface` rather than another sibling:
// this window has exactly one subsurface, so the parent is the only other
// member of the stack there is to reorder against.
const WL_SUBSURFACE_PLACE_ABOVE: u32 = 2;
const WL_SUBSURFACE_PLACE_BELOW: u32 = 3;
const WL_SUBSURFACE_SET_DESYNC: u32 = 5;

// wl_compositor/wl_display/wl_registry/wl_seat/wl_pointer/wl_surface opcodes.
// Their `wl_interface` tables come from the library itself (dlsym'd below),
// so only the opcode numbers — fixed by `wayland.xml`, the core protocol —
// need naming here.
const WL_DISPLAY_GET_REGISTRY: u32 = 1;
const WL_REGISTRY_BIND: u32 = 0;
const WL_COMPOSITOR_CREATE_SURFACE: u32 = 0;
const WL_POINTER_SET_CURSOR: u32 = 0;
const WL_SURFACE_COMMIT: u32 = 6;
/// `wl_surface.set_opaque_region`. Sent by Cordial directly, not through GDK --
/// see `set_engine_stacking`.
const WL_SURFACE_SET_OPAQUE_REGION: u32 = 4;
const WL_SEAT_GET_POINTER: u32 = 0;
const WL_SEAT_GET_KEYBOARD: u32 = 1;
const WL_SEAT_GET_TOUCH: u32 = 2;

/// `wl_seat.capabilities` bits.
const WL_SEAT_CAPABILITY_POINTER: u32 = 1;
const WL_SEAT_CAPABILITY_KEYBOARD: u32 = 2;
const WL_SEAT_CAPABILITY_TOUCH: u32 = 4;

/// What the seat said it has, filled in by [`seat_capabilities`] before any
/// device is asked for.
static SEAT_CAPS: AtomicU32 = AtomicU32::new(0);

/// `wl_seat`'s events. Two slots, not one: `name` arrives at version 2 and the
/// seat is bound at 1, so it should never fire -- but the module doc records a
/// freeze caused by a listener with fewer slots than the compositor had events
/// for, and an unused slot costs nothing while a missing one is a wild jump.
#[repr(C)]
struct SeatListener {
    capabilities: unsafe extern "C" fn(*mut c_void, *mut c_void, u32),
    name: unsafe extern "C" fn(*mut c_void, *mut c_void, *const std::ffi::c_char),
}

/// `wl_seat.capabilities`, which arrives once during `open`'s roundtrip and
/// again whenever the seat's set of devices changes.
///
/// **The later ones are the point.** This used to store and stop, and `open`
/// read the atomic exactly once to decide which devices to ask for -- so a
/// touchscreen (or a mouse, or a keyboard) plugged in after startup updated a
/// number nothing looked at again, and was silently ignored for the life of the
/// session. Touch is where that stops being theoretical: a tablet whose
/// keyboard cover is folded back, or a screen on a USB-C dock, arrives long
/// after the window is up.
///
/// Only touch is picked up late here, and that is a stated limit rather than an
/// oversight. Binding a `wl_pointer` from this callback would mean the pointer
/// listener, the enter/leave canvas tracking and the pointer-lock objects all
/// coming into existence halfway through a frame, and `open` currently owns
/// that whole graph; a keyboard means an xkb state to build from a keymap event
/// that has not arrived. Touch needs one proxy and one listener, and nothing
/// downstream of it caches a null. So: **a touchscreen connected after startup
/// works, a mouse or keyboard connected after startup still does not**, and the
/// line printed below is how a user finds that out rather than wondering.
unsafe extern "C" fn seat_capabilities(_data: *mut c_void, seat: *mut c_void, caps: u32) {
    let previous = SEAT_CAPS.swap(caps, Ordering::SeqCst);
    if previous == caps {
        return;
    }
    // `current()` is `None` for the first of these events, which arrives inside
    // `open`'s own roundtrip before the window is published -- and that is the
    // one case that needs no help, because `open` reads the atomic immediately
    // afterwards and binds from it.
    let _ = seat;
    let Some(w) = current() else { return };
    if caps & WL_SEAT_CAPABILITY_TOUCH != 0 {
        w.bind_touch();
        // Reported for consistency and not because anything reads it: the
        // engine took `isTouchDevice` during startup and this build exposes no
        // native by which a platform revises it. Said plainly here rather than
        // left to be discovered -- a touchscreen plugged in now works as an
        // input device and is invisible as a declared one. See
        // `input::report_touchscreen`.
        super::input::report_touchscreen(true);
    }
    // A withdrawn touch capability leaves the `wl_touch` object in place, and
    // that is a stated limit rather than a considered release policy:
    // `wl_touch.release` arrives in `wl_seat` version 3 and this file binds the
    // seat at 1, so there is no request to send and no `wl_proxy_destroy`
    // wired here. A screen unplugged and plugged back in therefore reuses the
    // same object. Whether a compositor resumes delivering to it is **not
    // tested** -- nobody here has a touchscreen to unplug -- and if one does
    // not, touch would stay dead until the client restarts.
    //
    // What must not be left in place is the engine's idea of what is on the
    // glass, so anything still down is cancelled. A contact nothing ever closes
    // is a finger the engine believes is held for the rest of the session.
    else if previous & WL_SEAT_CAPABILITY_TOUCH != 0 {
        eprintln!("[android] wayland: the seat withdrew its touch capability");
        let (cw, ch, _) = w.geometry();
        super::input::touch_cancel(w.active_handle.load(Ordering::Relaxed), (cw, ch), w.now_ms());
    }
}

unsafe extern "C" fn seat_name(_data: *mut c_void, _seat: *mut c_void, _name: *const std::ffi::c_char) {}

static SEAT_LISTENER: SeatListener = SeatListener { capabilities: seat_capabilities, name: seat_name };

// --------------------------------------------------------------- dlopen'd API

/// `wl_proxy_marshal_flags`'s C signature is variadic — the fixed prefix is
/// typed, and each call site below supplies however many trailing arguments
/// that message's signature actually needs. This is the same function
/// `wayland-scanner`'s generated inline wrappers call; there is no separate
/// "send a request" primitive underneath it.
type ProxyMarshalFlags = unsafe extern "C" fn(
    *mut c_void,
    u32,
    *const WlInterface,
    u32,
    u32,
    ...
) -> *mut c_void;

struct WlClient {
    get_fd: unsafe extern "C" fn(*mut c_void) -> c_int,
    flush: unsafe extern "C" fn(*mut c_void) -> c_int,
    dispatch_pending: unsafe extern "C" fn(*mut c_void) -> c_int,
    prepare_read: unsafe extern "C" fn(*mut c_void) -> c_int,
    read_events: unsafe extern "C" fn(*mut c_void) -> c_int,
    cancel_read: unsafe extern "C" fn(*mut c_void) -> c_int,
    roundtrip: unsafe extern "C" fn(*mut c_void) -> c_int,
    marshal_flags: ProxyMarshalFlags,
    add_listener: unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int,
    /// What a proxy's version *actually* is, rather than what the call site
    /// that made it guessed. A child object inherits its parent's version, so
    /// this is the only honest source for the number `wl_proxy_marshal_flags`
    /// should be given when creating one — see the text-input section of the
    /// module doc for what a guess cost here.
    get_version: unsafe extern "C" fn(*mut c_void) -> u32,
    /// Set once the connection is unusable. Non-zero means every later request
    /// is discarded and every dispatch fails, so a run that reaches this is
    /// over whatever it does next; `pump` reports it rather than letting the
    /// process die with only GDK's `Error %d ... dispatching to Wayland
    /// display` to go on, which names neither the object nor the reason.
    get_error: unsafe extern "C" fn(*mut c_void) -> c_int,
    get_protocol_error:
        unsafe extern "C" fn(*mut c_void, *mut *const WlInterface, *mut u32) -> u32,

    registry_interface: *const WlInterface,
    compositor_interface: *const WlInterface,
    subcompositor_interface: *const WlInterface,
    subsurface_interface: *const WlInterface,
    surface_interface: *const WlInterface,
    seat_interface: *const WlInterface,
    pointer_interface: *const WlInterface,
    keyboard_interface: *const WlInterface,
    touch_interface: *const WlInterface,
}
// SAFETY: every field is either a function pointer (inherently `Send + Sync`
// — it is a code address, not aliased state) or a pointer into a host shared
// library that is dlopen'd once and never closed, exactly like `Xlib` in
// `window.rs`.
unsafe impl Send for WlClient {}
unsafe impl Sync for WlClient {}

struct WlEgl {
    create: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> *mut c_void,
    resize: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int),
}
unsafe impl Send for WlEgl {}
unsafe impl Sync for WlEgl {}

struct Xkb {
    context_new: unsafe extern "C" fn(u32) -> *mut c_void,
    context_unref: unsafe extern "C" fn(*mut c_void),
    keymap_new_from_string: unsafe extern "C" fn(*mut c_void, *const c_char, u32, u32) -> *mut c_void,
    keymap_unref: unsafe extern "C" fn(*mut c_void),
    keymap_mod_get_index: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
    state_new: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    state_unref: unsafe extern "C" fn(*mut c_void),
    // Deliberately no `xkb_state_update_key`: `wl_keyboard.modifiers` already
    // carries the compositor's own authoritative depressed/latched/locked
    // mask for every key event, applied via `state_update_mask` below.
    // Re-deriving it per keystroke with `state_update_key` would be redundant
    // for ordinary keys and actively wrong for modifier keys themselves,
    // double-applying a toggle the server already accounted for.
    state_update_mask: unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, u32) -> u32,
    state_key_get_one_sym: unsafe extern "C" fn(*mut c_void, u32) -> u32,
    state_key_get_utf8: unsafe extern "C" fn(*mut c_void, u32, *mut c_char, usize) -> c_int,
    state_mod_index_is_active: unsafe extern "C" fn(*mut c_void, u32, u32) -> c_int,
}
unsafe impl Send for Xkb {}
unsafe impl Sync for Xkb {}

const RTLD_NOW: c_int = 2;

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn close(fd: c_int) -> c_int;
}

impl WlClient {
    fn load() -> Result<Self, String> {
        // SAFETY: a literal soname; the handle is never closed, matching
        // every other host library this runtime dlopen's.
        let lib = unsafe { dlopen(c"libwayland-client.so.0".as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            return Err("libwayland-client.so.0 is not available".into());
        }
        macro_rules! sym {
            ($name:literal) => {{
                let name = CString::new($name).unwrap();
                // SAFETY: the handle is open and the name is one of
                // libwayland-client's documented exports (function or data
                // symbol), so the transmuted type is the one that name has.
                let p = unsafe { dlsym(lib, name.as_ptr()) };
                if p.is_null() {
                    return Err(format!("libwayland-client has no {}", $name));
                }
                unsafe { std::mem::transmute(p) }
            }};
        }
        Ok(WlClient {
            // Deliberately no `wl_display_connect`: the one connection in this
            // process is GTK's, and opening a second would give the engine a
            // surface whose buffers can never be attached to the window's. See
            // the module doc.
            get_fd: sym!("wl_display_get_fd"),
            flush: sym!("wl_display_flush"),
            dispatch_pending: sym!("wl_display_dispatch_pending"),
            prepare_read: sym!("wl_display_prepare_read"),
            read_events: sym!("wl_display_read_events"),
            cancel_read: sym!("wl_display_cancel_read"),
            roundtrip: sym!("wl_display_roundtrip"),
            marshal_flags: sym!("wl_proxy_marshal_flags"),
            add_listener: sym!("wl_proxy_add_listener"),
            get_version: sym!("wl_proxy_get_version"),
            get_error: sym!("wl_display_get_error"),
            get_protocol_error: sym!("wl_display_get_protocol_error"),
            registry_interface: sym!("wl_registry_interface"),
            compositor_interface: sym!("wl_compositor_interface"),
            subcompositor_interface: sym!("wl_subcompositor_interface"),
            subsurface_interface: sym!("wl_subsurface_interface"),
            surface_interface: sym!("wl_surface_interface"),
            seat_interface: sym!("wl_seat_interface"),
            pointer_interface: sym!("wl_pointer_interface"),
            keyboard_interface: sym!("wl_keyboard_interface"),
            touch_interface: sym!("wl_touch_interface"),
        })
    }
}

impl WlEgl {
    fn load() -> Result<Self, String> {
        // SAFETY: as `WlClient::load`.
        let lib = unsafe { dlopen(c"libwayland-egl.so.1".as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            return Err("libwayland-egl.so.1 is not available".into());
        }
        macro_rules! sym {
            ($name:literal) => {{
                let name = CString::new($name).unwrap();
                let p = unsafe { dlsym(lib, name.as_ptr()) };
                if p.is_null() {
                    return Err(format!("libwayland-egl has no {}", $name));
                }
                unsafe { std::mem::transmute(p) }
            }};
        }
        Ok(WlEgl { create: sym!("wl_egl_window_create"), resize: sym!("wl_egl_window_resize") })
    }
}

/// `XKB_CONTEXT_NO_FLAGS`.
const XKB_CONTEXT_NO_FLAGS: u32 = 0;
/// `XKB_KEYMAP_FORMAT_TEXT_V1` — the only format `wl_keyboard.keymap` sends.
const XKB_KEYMAP_FORMAT_TEXT_V1: u32 = 1;
/// `XKB_STATE_MODS_EFFECTIVE` — "is this modifier affecting keysym
/// translation right now", as opposed to merely depressed/latched/locked.
const XKB_STATE_MODS_EFFECTIVE: u32 = 1 << 3;

impl Xkb {
    fn load() -> Result<Self, String> {
        // SAFETY: as `WlClient::load`.
        let lib = unsafe { dlopen(c"libxkbcommon.so.0".as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            return Err("libxkbcommon.so.0 is not available".into());
        }
        macro_rules! sym {
            ($name:literal) => {{
                let name = CString::new($name).unwrap();
                let p = unsafe { dlsym(lib, name.as_ptr()) };
                if p.is_null() {
                    return Err(format!("libxkbcommon has no {}", $name));
                }
                unsafe { std::mem::transmute(p) }
            }};
        }
        Ok(Xkb {
            context_new: sym!("xkb_context_new"),
            context_unref: sym!("xkb_context_unref"),
            keymap_new_from_string: sym!("xkb_keymap_new_from_string"),
            keymap_unref: sym!("xkb_keymap_unref"),
            keymap_mod_get_index: sym!("xkb_keymap_mod_get_index"),
            state_new: sym!("xkb_state_new"),
            state_unref: sym!("xkb_state_unref"),
            state_update_mask: sym!("xkb_state_update_mask"),
            state_key_get_one_sym: sym!("xkb_state_key_get_one_sym"),
            state_key_get_utf8: sym!("xkb_state_key_get_utf8"),
            state_mod_index_is_active: sym!("xkb_state_mod_index_is_active"),
        })
    }
}

// ------------------------------------------------------------------ listeners
//
// `wl_proxy_add_listener` takes a pointer to an array of function pointers,
// one per event opcode in that interface's table, plus an opaque userdata
// pointer handed back on every call. A `#[repr(C)]` struct of function
// pointers in opcode order *is* that array — no different from how
// `wayland-scanner`'s generated `wl_xxx_listener` structs are defined, just
// written by hand. Function pointers are `Send + Sync` unconditionally (they
// are code addresses, not aliased data), so unlike `WlInterface` above these
// need no manual `unsafe impl`.

#[repr(C)]
struct RegistryListener {
    global: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *const c_char, u32),
    global_remove: unsafe extern "C" fn(*mut c_void, *mut c_void, u32),
}

// `wl_pointer_interface`/`wl_keyboard_interface` (below) are `dlsym`'d from
// the host's real `libwayland-client.so`, not hand-written like the
// `text-input` tables above — so their `event_count` is whatever the *host's*
// library version really declares, not whatever this file happens to have a
// listener field for. `dispatch_event` indexes the listener array
// `wl_proxy_add_listener` was given by the wire opcode with no bounds check of
// its own, so every one of wl_seat's core-protocol interfaces needs its
// *complete, current* event set declared here regardless of which `wl_seat`
// version this file requests.
//
// The crash this prevents was measured, on a listener struct that is no longer
// here to point at: the `xdg_toplevel` one, back when this file owned the
// toplevel. It was two fields long against an interface Mutter sent a fifth
// event on, and `wl_closure_invoke` jumped to address `0xe0` — a small garbage
// address, which is what reading past the end of a listener array looks like.
// The lesson outlived the code: "the compositor will not send events past the
// version I bound" did not hold on GNOME Shell. `PointerListener` was previously missing
// `frame`/`axis_source`/`axis_stop`/`axis_discrete`/`axis_value120`/
// `axis_relative_direction` (added in `wl_pointer` v5, v5, v5, v5, v8, v9);
// `KeyboardListener` below was missing `repeat_info` (`wl_keyboard` v4). Every
// new field here is a genuine no-op — none of scroll-wheel batching, event
// framing, or key-repeat timing is implemented — but the slot has to exist.
#[repr(C)]
struct PointerListener {
    enter: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, i32, i32),
    leave: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void),
    motion: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, i32, i32),
    button: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, u32, u32),
    axis: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, i32),
    frame: unsafe extern "C" fn(*mut c_void, *mut c_void),
    axis_source: unsafe extern "C" fn(*mut c_void, *mut c_void, u32),
    axis_stop: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32),
    axis_discrete: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, i32),
    axis_value120: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, i32),
    axis_relative_direction: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32),
}

/// `struct wl_array` (`wayland-util.h`): `{ size_t size; size_t alloc; void
/// *data; }`. Only the layout matters here — `wl_keyboard.enter`'s pressed-key
/// array is received and ignored, since it does not change what Cordial does
/// with a key.
#[repr(C)]
struct WlArray {
    size: usize,
    alloc: usize,
    data: *mut c_void,
}

/// `wl_touch`'s events. **Seven slots, though `wl_seat` is bound at version 1
/// and version 1 of `wl_touch` has five.**
///
/// The reasoning is `PointerListener`'s, and it is the same measured crash:
/// `dispatch_event` indexes this array by the wire opcode with no bounds check
/// of its own, `wl_touch_interface` is `dlsym`'d from the host's real
/// `libwayland-client.so` and so declares whatever event count *that* library
/// was built with, and a listener shorter than the interface is a jump through
/// whatever follows it in memory -- which on `xdg_toplevel` came out as address
/// `0xe0`. `shape` and `orientation` are version 6 and will not be delivered to
/// an object this file can create today; the slots exist so that the day they
/// are, nothing jumps.
#[repr(C)]
struct TouchListener {
    down: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, *mut c_void, i32, i32, i32),
    up: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, i32),
    motion: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, i32, i32, i32),
    frame: unsafe extern "C" fn(*mut c_void, *mut c_void),
    cancel: unsafe extern "C" fn(*mut c_void, *mut c_void),
    shape: unsafe extern "C" fn(*mut c_void, *mut c_void, i32, i32, i32),
    orientation: unsafe extern "C" fn(*mut c_void, *mut c_void, i32, i32),
}

#[repr(C)]
struct KeyboardListener {
    keymap: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, c_int, u32),
    enter: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const WlArray),
    leave: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void),
    key: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, u32, u32),
    modifiers: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, u32, u32, u32),
    repeat_info: unsafe extern "C" fn(*mut c_void, *mut c_void, i32, i32),
}

/// `zwp_locked_pointer_v1`'s two events, which are how the compositor answers
/// a lock request. **There is no reply that means "refused"** — a compositor
/// that declines simply never sends `locked`, which is why
/// `sync_pointer_lock` times the request out rather than waiting for an error
/// that cannot arrive.
#[repr(C)]
struct LockedPointerListener {
    locked: unsafe extern "C" fn(*mut c_void, *mut c_void),
    unlocked: unsafe extern "C" fn(*mut c_void, *mut c_void),
}

/// The four `wl_fixed_t` arguments are two pairs, not four numbers: the
/// accelerated delta the compositor's pointer profile produced, then the raw
/// unaccelerated one.
///
/// Cordial uses the **unaccelerated** pair by default, and which pair it uses
/// is now a setting -- see [`relative_pointer_motion`]. This comment said the
/// opposite until 2026-08-21, having outlived commit 6cb9ed7 which changed the
/// behaviour and left the description behind. A comment that contradicts the
/// code twenty lines below it costs more than no comment.
#[repr(C)]
struct RelativePointerListener {
    relative_motion: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, i32, i32, i32, i32),
}

/// Nine slots, not six. The last three are `zwp_text_input_v3` version 2's
/// `action`/`language`/`preedit_hint`, and they are here for the same reason
/// `PointerListener` carries slots for scroll events nothing implements: the
/// compositor sends by opcode, `dispatch_event` indexes this array by that
/// opcode with no bounds check of its own, and the version the compositor
/// thinks this object has is the manager's, not the number this file passes
/// around. Leaving them out is what produced `interface 'zwp_text_input_v3'
/// has no event 8` — see the module doc for the measurement.
#[repr(C)]
struct TextInputListener {
    enter: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
    leave: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
    preedit_string: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char, i32, i32),
    commit_string: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char),
    delete_surrounding_text: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32),
    done: unsafe extern "C" fn(*mut c_void, *mut c_void, u32),
    action: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32),
    language: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char),
    preedit_hint: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, u32),
}

// -------------------------------------------------------------------- state

struct Geometry {
    width: i32,
    height: i32,
    format: i32,
}

/// One accumulated `zwp_text_input_v3` double-buffer group — see the module
/// doc's "double-buffered" paragraph. `None` means "this event type did not
/// arrive since the last `done`", which is different from arriving with an
/// empty/null payload — that distinction is exactly what an
/// `Option<Option<_>>` expresses and a plain default-empty value would lose.
#[derive(Default)]
struct PendingImeGroup {
    /// Outer: did `commit_string` arrive this group. Inner: its (nullable)
    /// text.
    commit: Option<Option<String>>,
    /// Outer: did `preedit_string` arrive this group. Inner: its (nullable)
    /// text plus the cursor range *within that preedit text*, in bytes.
    preedit: Option<Option<(String, i32, i32)>>,
    delete: Option<(u32, u32)>,
}

struct ImeState {
    /// The composing string currently shown, spliced into the committed text
    /// at the caret — `None` when nothing is being composed. Kept apart from
    /// `input::edit_text_buffer`'s buffer on purpose: see the module doc.
    preedit: Option<(String, i32, i32)>,
    pending: PendingImeGroup,
    /// Whether `enable()` has been sent for the currently focused box, so
    /// enable/disable only fire on an actual focus transition rather than on
    /// every input-pump tick.
    enabled: bool,
    /// The last `textbox_generation()` this was synchronised against.
    synced_generation: Option<u32>,
    /// Whether the input method has produced any text for the current focus
    /// session — set by the first `commit_string` or `preedit_string` to
    /// arrive, cleared on `leave`.
    ///
    /// This is what stops the two paths inserting the same character twice.
    /// Both `wl_keyboard` and `zwp_text_input_v3` can deliver text, and which
    /// one actually does depends entirely on the user's setup: with no input
    /// source configured — `org.gnome.desktop.input-sources sources` empty,
    /// which is the default on a fresh GNOME — the compositor answers `enable`
    /// with nothing but `done`, and every character arrives through
    /// `wl_keyboard`. Configure an engine such as ibus typing-booster and the
    /// same keystrokes arrive as preedit and commits instead, with the
    /// compositor free to also forward the raw key.
    ///
    /// So neither path can be the only one, and neither can be trusted to be
    /// silent. The keyboard path inserts text only while this is false; once
    /// an input method speaks for this session it owns the text and the
    /// keyboard is left to arrows, Enter and shortcuts.
    ime_producing: bool,
}
struct XkbState {
    xkb: Xkb,
    context: *mut c_void,
    keymap: *mut c_void,
    state: *mut c_void,
    shift_idx: u32,
    ctrl_idx: u32,
    alt_idx: u32,
    caps_idx: u32,
}
// SAFETY: only ever touched from the input-pump thread (see the module doc
// on `pump_input_events` never running concurrently with itself); the
// pointers are opaque libxkbcommon handles this runtime owns exclusively.
unsafe impl Send for XkbState {}
unsafe impl Sync for XkbState {}

/// The GTK window the engine's surface hangs under.
///
/// GTK objects are `Rc`-refcounted, not atomically, so touching this from two
/// threads corrupts a refcount rather than failing a lock. Everything that
/// reaches it — `open`, `pump`, the geometry sync — runs on the thread that
/// called `open`, which is the same thread `looper::pump` runs on, which is
/// the thread Android calls the UI thread. Nothing else may touch it, and
/// that is the whole justification for the `unsafe impl` below.
struct HostWindowCell(cordial_shell::host_window::HostWindow);
// SAFETY: see above — main-thread-only by construction, and only reachable
// through `&WaylandWindow`, whose other users (Mesa's EGL/Vulkan paths, from
// the engine's render thread) never call the methods that go through here.
unsafe impl Send for HostWindowCell {}
unsafe impl Sync for HostWindowCell {}

pub struct WaylandWindow {
    wl: WlClient,
    egl: Option<WlEgl>,
    display: *mut c_void,
    host: HostWindowCell,
    // Kept named and typed even though only `surface`/`subsurface` are read
    // again after construction — the rest are still owned proxies for the
    // life of this one-window-per-process runtime (the same scope
    // `window.rs`'s `HostWindow` has), and naming them documents the object
    // graph a future teardown or diagnostic would need, rather than letting
    // it go unrecorded because nothing currently reads it back.
    #[allow(dead_code)]
    compositor: *mut c_void,
    #[allow(dead_code)]
    subcompositor: *mut c_void,
    surface: *mut c_void,
    subsurface: *mut c_void,
    /// GTK's own toplevel `wl_surface` — the subsurface's parent, and the
    /// surface `wl_keyboard`/`wl_pointer` report focus against for everything
    /// that is not the canvas.
    parent_surface: *mut c_void,
    /// The seat itself, kept because a device can still be asked for after
    /// `open` has returned -- see [`WaylandWindow::bind_touch`], which is what
    /// makes a touchscreen plugged in mid-session work.
    seat: *mut c_void,
    #[allow(dead_code)]
    pointer: *mut c_void,
    /// GDK's `wl_pointer`, borrowed for pointer constraints and relative
    /// motion. GDK owns and destroys it; Cordial must never attach its core
    /// pointer listener to this object or release it.
    capture_pointer: *mut c_void,
    #[allow(dead_code)]
    keyboard: *mut c_void,
    /// The seat's `wl_touch`, or null on a seat with no touchscreen.
    ///
    /// An atomic rather than a plain pointer like `pointer` and `keyboard`
    /// beside it, because this is the one device that can appear *after*
    /// `open` has finished: [`seat_capabilities`] binds it from a later
    /// `capabilities` event, by which time `WaylandWindow` is behind a
    /// `OnceLock` and immutable. Read only to answer "have we got one already",
    /// so `Relaxed` is enough -- every write happens on the pump thread, which
    /// is also the only reader.
    touch: std::sync::atomic::AtomicPtr<c_void>,
    text_input: *mut c_void,
    /// `zwp_pointer_constraints_v1`, or null on a compositor that has none.
    /// Null is not an error: everything except pointer capture works without
    /// it, exactly as text entry works without `zwp_text_input_manager_v3`.
    pointer_constraints: *mut c_void,
    /// The `zwp_relative_pointer_v1` for GDK's pointer, created once and kept
    /// for the process's life. It delivers `relative_motion` whenever the
    /// pointer has focus, lock or no lock; `dispatch_relative_motion` is what
    /// decides to act on it, so there is nothing to create and destroy per
    /// lock.
    relative_pointer: *mut c_void,
    /// The live `zwp_locked_pointer_v1`, or null when the pointer is free.
    /// Destroying this object *is* the release — there is no "unlock" request
    /// — so this being null and the pointer being free are the same statement.
    locked_pointer: Mutex<*mut c_void>,
    /// When the current lock was asked for, so a compositor that silently
    /// declines can be reported once instead of leaving the camera dead with no
    /// explanation. `None` while no request is outstanding.
    lock_requested_at: Mutex<Option<std::time::Instant>>,
    /// When the compositor last deactivated a lock Cordial still wants, or
    /// `None` while the lock is active. See [`WaylandWindow::sync_pointer_lock`]
    /// -- a deactivation that is never reversed is otherwise permanent.
    lock_inactive_since: Mutex<Option<std::time::Instant>>,
    conn_fd: c_int,

    buffers: Mutex<Geometry>,
    /// Where the canvas currently sits inside the parent surface. Compared
    /// against the content widget's allocation every pump so a resize or a
    /// header-bar height change moves the subsurface exactly once, rather than
    /// re-sending `set_position` on every tick.
    placed_at: Mutex<(i32, i32)>,
    egl_window: Mutex<*mut c_void>,

    xkb: Mutex<Option<XkbState>>,
    /// Canvas-local pointer position. This is read on every motion and on
    /// each relative report while locked, so keep the two coordinates in one
    /// atomic word rather than taking a mutex on the hottest input path.
    pointer_pos: AtomicU64,
    pointer_buttons: AtomicI32,
    down_time_ms: AtomicI64,
    clock: std::time::Instant,

    ime: Mutex<ImeState>,

    /// The `GameActivity` handle `pump_input_events` was last called with.
    /// AGDK callbacks (`surface_resized` in particular) need this, but they
    /// run from inside `wl_display_dispatch_pending`, invoked from listener
    /// callbacks that have no handle parameter of their own — the protocol's
    /// event signatures are fixed, not something this file can extend. `0` is
    /// "no handle observed yet", which is never a real `GameActivity` handle.
    active_handle: AtomicI64,

    /// How many web-view dialogs are currently open. `place_below`/
    /// `place_above` are only issued on the 0-to-1 and 1-to-0 edges (see
    /// [`Self::webview_dialog_opened`]) so a second dialog opening while the
    /// first is still up does not re-lower an already-lowered subsurface, and
    /// closing one of two leaves the engine correctly hidden until the last
    /// one goes.
    open_web_view_dialogs: AtomicI32,
    text_overlay_visible: AtomicBool,
    text_overlay_cache:
        Mutex<Option<(u32, u64, String, i32, cordial_linker_sys::game_activity::RawTextBoxInfo)>>,
    /// What `nativeGetTextBoxInfo` last said, for the focus generation it said
    /// it about. See [`WaylandWindow::polled_textbox_info`] — this exists so
    /// the ticks between polls keep drawing the editor where the last poll put
    /// it, rather than dropping back to the fallback bar and flickering
    /// between the two at half the pump rate.
    polled_textbox_info: Mutex<Option<PolledTextBoxInfo>>,
    /// See [`WaylandWindow::resolve_textbox_geometry`].
    last_placement: Mutex<Option<LastPlacement>>,
}

/// Where the editor was last actually put, for anything outside the pump that
/// needs to know -- which today is the development control surface.
///
/// Published rather than recomputed because the answer is not derivable from
/// outside: `focused_textbox_info` is the engine's *volunteered* spec, and for
/// the search modal that is legitimately `0x0` for about a second while the
/// editor sits correctly on the box from a polled answer. A test that read the
/// spec would conclude the editor was nowhere, which is what happened on
/// 2026-08-25 before this existed.
pub(crate) static LAST_EDITOR_RECT: Mutex<Option<(f32, f32, f32, f32, &'static str)>> =
    Mutex::new(None);

/// Which source put the editor where it is.
///
/// Three, not two, because the middle one is the difference between a smooth
/// focus and a visible flinch. See [`WaylandWindow::resolve_textbox_geometry`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Placed {
    /// The engine said where this box is, and it is there.
    Engine,
    /// The engine has not said yet, so the editor is being held where the last
    /// box was until it does.
    Carried,
    /// Nothing has ever said, so the editor is in a bar Cordial positions.
    Fallback,
}

impl Placed {
    /// For the log, where a sentence reads better than a word.
    fn source(self) -> &'static str {
        match self {
            Placed::Engine => "engine geometry",
            Placed::Carried => "the previous box, while this one lays out",
            Placed::Fallback => "fallback bar",
        }
    }

    /// For the control socket, whose replies are whitespace-separated fields
    /// and cannot carry a sentence.
    fn token(self) -> &'static str {
        match self {
            Placed::Engine => "engine",
            Placed::Carried => "carried",
            Placed::Fallback => "fallback",
        }
    }
}

/// The last placement the engine actually vouched for, and when.
///
/// Kept so a focus whose geometry has not arrived yet has somewhere better to
/// put the editor than the bottom of the window. See
/// [`WaylandWindow::resolve_textbox_geometry`].
struct LastPlacement {
    info: cordial_linker_sys::game_activity::RawTextBoxInfo,
    at: std::time::Instant,
}

/// One focus generation's worth of polling state for
/// [`WaylandWindow::polled_textbox_info`].
struct PolledTextBoxInfo {
    /// Which focused box this is about. A new generation resets everything;
    /// handles are reused, generations are not.
    generation: u32,
    /// When the engine was last asked, so the rate limit has something to
    /// measure against.
    asked: std::time::Instant,
    /// The last answer good enough to place an editor from, if there has been
    /// one. `None` while the engine is still saying null or still mid-layout.
    usable: Option<cordial_linker_sys::game_activity::RawTextBoxInfo>,
}
// SAFETY: every raw pointer field is either a `libwayland-client` proxy (only
// ever touched from the single input-pump thread, matching the file-level
// "must never block" constraint `window.rs` documents for X11) or a host
// library handle from a library this runtime never closes.
unsafe impl Send for WaylandWindow {}
unsafe impl Sync for WaylandWindow {}

static WINDOW: OnceLock<WaylandWindow> = OnceLock::new();

/// TEMPORARY INSTRUMENTATION -- not for commit. `CORDIAL_INSTR=1`.
fn instr_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_INSTR").is_some())
}

/// The control for the redraw nudge in [`WaylandWindow::apply_resize`], so the
/// change can be shown to move the number in the same session rather than
/// across two builds.
fn redraw_nudge_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("CORDIAL_NO_REDRAW_NUDGE").is_some())
}

// -------------------------------------------------------------- construction

/// Collected while walking `wl_registry`'s globals. A plain struct on the
/// stack rather than anything in `WaylandWindow`, because at this point in
/// `open()` there is no window yet to attach it to — this is the same
/// chicken-and-egg `window.rs` does not have to solve (X11 resource ids are
/// valid the moment they are allocated; Wayland globals have to be *told
/// about* before they can be bound).
#[derive(Default)]
struct Globals {
    compositor: Option<(u32, u32)>,
    subcompositor: Option<(u32, u32)>,
    seat: Option<(u32, u32)>,
    text_input_manager: Option<(u32, u32)>,
    pointer_constraints: Option<(u32, u32)>,
    relative_pointer_manager: Option<(u32, u32)>,
}

unsafe extern "C" fn registry_global(
    data: *mut c_void,
    _registry: *mut c_void,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    let globals = &mut *(data as *mut Globals);
    // SAFETY: `wl_registry.global`'s `interface` argument is a NUL-terminated
    // string per the protocol.
    let iface = CStr::from_ptr(interface).to_string_lossy();
    match iface.as_ref() {
        "wl_compositor" => globals.compositor = Some((name, version)),
        "wl_subcompositor" => globals.subcompositor = Some((name, version)),
        "wl_seat" => globals.seat = Some((name, version)),
        "zwp_text_input_manager_v3" => globals.text_input_manager = Some((name, version)),
        "zwp_pointer_constraints_v1" => globals.pointer_constraints = Some((name, version)),
        "zwp_relative_pointer_manager_v1" => globals.relative_pointer_manager = Some((name, version)),
        _ => {}
    }
}

unsafe extern "C" fn registry_global_remove(_data: *mut c_void, _registry: *mut c_void, _name: u32) {
    // A global disappearing mid-session (compositor restart, seat unplug) is
    // not handled — the same scope Cordial's X11 backend keeps: one window,
    // fixed for the process's life. Noting rather than silently ignoring, in
    // case a future bug report starts here.
    super::trace(format_args!("wayland: a global was removed; not handled"));
}

static REGISTRY_LISTENER: RegistryListener =
    RegistryListener { global: registry_global, global_remove: registry_global_remove };

pub fn open(width: u32, height: u32, title: &str) -> Result<&'static WaylandWindow, String> {
    if let Some(w) = WINDOW.get() {
        return Ok(w);
    }
    let wl = WlClient::load()?;

    // ---- the window itself. GTK opens the connection, owns the
    // `xdg_toplevel`, draws the header bar and answers configure/ack; this
    // file's job starts at the content area and stops there. See the module
    // doc for why the engine's surface cannot live on a connection of its own.
    cordial_shell::host_window::init_wayland()?;
    let host = cordial_shell::host_window::HostWindow::with_canvas(title, width as i32, height as i32);

    // Size and maximised state from the last session on this profile, applied
    // before the window is presented so it maps at the remembered geometry
    // rather than snapping to it a frame later. Everything about the file --
    // where it lives, why the launcher's record is a separate one, and why the
    // saved fullscreen is written but not restored here -- is in
    // `cordial_shell::window_state`, which is also where the handlers that keep
    // it current are wired.
    //
    // **This is the only thing in this file that decides the surface's initial
    // extent, and that is deliberate.** The engine learns the size through the
    // ordinary path: the compositor configures the toplevel, `apply_resize`
    // runs, and `config::set_screen` is called from inside it before the EGL
    // resize -- the same sequence a fullscreen toggle mid-session goes through.
    // A restored size that bypassed that would be the letterboxing bug this
    // file's own history is full of.
    cordial_shell::window_state::remember(
        host.window(),
        crate::profile::active(),
        cordial_shell::window_state::Which::Game,
    );

    host.present();
    host.wait_until_mapped(std::time::Duration::from_secs(5))?;

    let display = host.wl_display().ok_or_else(|| {
        "GTK's display is not a Wayland display (GDK_BACKEND?)".to_string()
    })?;
    let parent_surface =
        host.wl_surface().ok_or_else(|| "GTK's window has no wl_surface".to_string())?;
    // This must be GDK's pointer, not another `wl_seat.get_pointer` object.
    // GDK owns the desktop cursor visible over this GTK window; a constraint
    // on Cordial's separate event pointer can be acknowledged as locked while
    // leaving that real cursor free, which is the observed escape bug.
    let capture_pointer = host.wl_pointer().unwrap_or(std::ptr::null_mut());
    if !capture_pointer.is_null() {
        eprintln!(
            "[android] wayland: pointer capture uses GDK's wl_pointer on the GTK toplevel \
             (KWin subsurface workaround)"
        );
    }
    let (cx, cy, cw, ch) =
        host.content_rect().ok_or_else(|| "GTK's window has no content allocation".to_string())?;

    // ---- registry: walk every global, remembering the ones this backend
    // needs, then roundtrip so the walk is known to be complete before
    // anything tries to bind from it.
    //
    // This runs on GDK's connection and its default queue, so the roundtrip
    // also dispatches whatever GTK had waiting. That is fine — it is the same
    // thread GTK's main loop runs on — but it is why nothing here may assume
    // it is the only code touching the connection.
    let registry = unsafe {
        (wl.marshal_flags)(
            display,
            WL_DISPLAY_GET_REGISTRY,
            wl.registry_interface,
            1,
            0,
            std::ptr::null_mut::<c_void>(),
        )
    };
    if registry.is_null() {
        return Err("wl_display_get_registry failed".into());
    }
    // Leaked on purpose, and this is not tidiness that got away.
    //
    // The registry proxy is never destroyed, so its listener stays registered
    // for the process's whole life, and `wl_registry.global` fires again
    // whenever the compositor adds one — a monitor hotplug, a seat appearing.
    // A `&mut` to a local here would have `open()`'s stack frame under it,
    // long since returned and reused, and `registry_global` writes through
    // that pointer. That is the *exact* shape of §1a's bug 2, where a
    // stack-local `XdgSurfaceListener` stayed registered and the first
    // configure that arrived afterwards jumped through reused stack bytes; the
    // only difference is that this one writes rather than calls, so it would
    // corrupt something instead of crashing where it happened. Not observed to
    // fire — a global has to arrive that this file matches on — which is why
    // it is worth fixing now rather than after it does.
    let globals: &'static mut Globals = Box::leak(Box::new(Globals::default()));
    unsafe {
        (wl.add_listener)(
            registry,
            &REGISTRY_LISTENER as *const RegistryListener as *const c_void,
            globals as *mut Globals as *mut c_void,
        );
        if (wl.roundtrip)(display) < 0 {
            return Err("wl_display_roundtrip failed while enumerating globals".into());
        }
    }

    let Some((compositor_name, compositor_ver)) = globals.compositor else {
        return Err("compositor advertises no wl_compositor".into());
    };
    let Some((subcompositor_name, subcompositor_ver)) = globals.subcompositor else {
        // Every desktop compositor has one — it is core protocol, not an
        // extension — but saying so plainly beats a null-pointer failure three
        // calls later if one ever does not.
        return Err("compositor advertises no wl_subcompositor; the engine's surface cannot be embedded".into());
    };
    let Some((seat_name, seat_ver)) = globals.seat else {
        return Err("compositor advertises no wl_seat".into());
    };
    // The text-input manager is the whole point of choosing Wayland (see the
    // module doc), but its absence should not make the window fail to open —
    // a compositor with no `zwp_text_input_v3` support still renders and
    // takes mouse/keyboard input correctly through everything else here, and
    // failing outright would make "no IME protocol" look like "no Wayland at
    // all". Text entry simply will not compose through an IME on such a
    // compositor, which is reported once, not hidden.
    let text_input_manager_global = globals.text_input_manager;
    if text_input_manager_global.is_none() {
        eprintln!(
            "[android] wayland: compositor advertises no zwp_text_input_manager_v3; \
             text entry will not have IME composition (see ADR-011)"
        );
    }

    let bind = |name: u32, want_version: u32, target_ver: u32, interface: &WlInterface, iface_name: &str| unsafe {
        let version = want_version.min(target_ver);
        let iface_c = CString::new(iface_name).unwrap();
        (wl.marshal_flags)(
            registry,
            WL_REGISTRY_BIND,
            interface as *const WlInterface,
            version,
            0,
            name,
            iface_c.as_ptr(),
            version,
            std::ptr::null_mut::<c_void>(),
        )
    };

    // SAFETY (this whole block): every proxy bound below comes from a global
    // this same roundtrip just confirmed exists, at a version clamped to
    // what the compositor actually advertised. Binding a global GTK has also
    // bound is ordinary — a global may be bound any number of times, and the
    // resulting objects are independent.
    let compositor = bind(compositor_name, 1, compositor_ver, unsafe { &*wl.compositor_interface }, "wl_compositor");
    if compositor.is_null() {
        return Err("binding wl_compositor failed".into());
    }
    let subcompositor = bind(
        subcompositor_name,
        1,
        subcompositor_ver,
        unsafe { &*wl.subcompositor_interface },
        "wl_subcompositor",
    );
    if subcompositor.is_null() {
        return Err("binding wl_subcompositor failed".into());
    }
    let seat = bind(seat_name, 1, seat_ver, unsafe { &*wl.seat_interface }, "wl_seat");
    if seat.is_null() {
        return Err("binding wl_seat failed".into());
    }
    // **Ask the seat what it has before asking it for anything.**
    //
    // `wl_seat.get_pointer` on a seat with no pointer capability is a protocol
    // error -- `missing_capability` -- and the compositor answers it by
    // disconnecting the client, not by returning null. Every desktop
    // compositor's seat has both a pointer and a keyboard, so this went
    // unnoticed until Cordial was run under a compositor whose seat has
    // neither: wlroots' headless backend has no libinput behind it and
    // advertises zero capabilities. The client died on startup with
    //
    //     wl_seat#56: error 0: wl_seat.get_pointer called when no pointer
    //     capability has existed
    //     Gdk-Message: Error flushing display: Protocol error
    //
    // which is the whole reason headless runs were impossible. The roundtrip is
    // what makes the answer usable here rather than three frames later.
    unsafe {
        (wl.add_listener)(
            seat,
            &SEAT_LISTENER as *const SeatListener as *const c_void,
            std::ptr::null_mut(),
        );
        if (wl.roundtrip)(display) < 0 {
            return Err("wl_display_roundtrip failed while asking wl_seat for its capabilities".into());
        }
    }

    // The version asked for comes from the table, not from a literal repeated
    // here. Those two numbers being allowed to disagree is the whole of the
    // freeze recorded in the module doc: this call said 2, `TEXT_INPUT_EVENTS`
    // described 1, and the compositor sent event 8 to a six-slot listener.
    let text_input_manager = text_input_manager_global.and_then(|(name, ver)| {
        let want = TEXT_INPUT_MANAGER_INTERFACE.version as u32;
        let m = bind(name, want, ver, &TEXT_INPUT_MANAGER_INTERFACE, "zwp_text_input_manager_v3");
        (!m.is_null()).then_some(m)
    });

    // ---- the canvas: a plain `wl_surface` given the subsurface role against
    // GTK's toplevel. No `xdg_surface`, no configure handshake, no ack — a
    // subsurface has no size of its own to negotiate, it is whatever its
    // buffer is, wherever its parent says.
    let surface = unsafe {
        (wl.marshal_flags)(
            compositor,
            WL_COMPOSITOR_CREATE_SURFACE,
            wl.surface_interface,
            1,
            0,
            std::ptr::null_mut::<c_void>(),
        )
    };
    if surface.is_null() {
        return Err("wl_compositor.create_surface failed".into());
    }
    let subsurface = unsafe {
        (wl.marshal_flags)(
            subcompositor,
            WL_SUBCOMPOSITOR_GET_SUBSURFACE,
            wl.subsurface_interface,
            1,
            0,
            std::ptr::null_mut::<c_void>(),
            surface,
            parent_surface,
        )
    };
    if subsurface.is_null() {
        return Err("wl_subcompositor.get_subsurface failed".into());
    }
    unsafe {
        // Desync is what makes the engine's own commits take effect when the
        // engine makes them. A subsurface is created *synchronised*, meaning
        // every commit waits for the parent's — and GTK commits only when it
        // repaints, which for a window nobody is touching is never. Leave this
        // out and the canvas shows one frame per accidental GTK redraw.
        (wl.marshal_flags)(subsurface, WL_SUBSURFACE_SET_DESYNC, std::ptr::null(), 1, 0);
        (wl.marshal_flags)(subsurface, WL_SUBSURFACE_SET_POSITION, std::ptr::null(), 1, 0, cx, cy);
        (wl.marshal_flags)(surface, WL_SURFACE_COMMIT, std::ptr::null(), 1, 0);
    }
    // `set_position` is latched on the *parent's* next commit, so the canvas
    // sits at (0,0) — under the header bar — until GTK draws again.
    host.queue_commit();
    host.pump();

    println!("[android] wayland: canvas {cw}x{ch} at ({cx},{cy}) as a subsurface of the libadwaita window");

    // ---- wl_seat: pointer + keyboard. Cordial takes input from the seat
    // directly rather than through GTK, because what the engine wants is
    // Android `MotionEvent`/`KeyEvent` shapes and GTK's controllers would only
    // be a second translation of the same evdev data. GDK has its own pointer
    // and keyboard on this seat; both clients see every event, and
    // `pointer_enter` below is what keeps this one from acting on the ones
    // aimed at the header bar.
    //
    // Both are null when the seat does not advertise them, which the rest of
    // this file already copes with -- every use downstream is behind an
    // `is_null` guard, because a compositor can withdraw a capability at
    // runtime and both were already optional in practice. A headless seat
    // therefore loses input and keeps rendering, which is exactly what an
    // agent's run wants and is a great deal better than dying at startup.
    let caps = SEAT_CAPS.load(Ordering::SeqCst);
    let pointer = if caps & WL_SEAT_CAPABILITY_POINTER != 0 {
        unsafe {
            (wl.marshal_flags)(seat, WL_SEAT_GET_POINTER, wl.pointer_interface, 1, 0, std::ptr::null_mut::<c_void>())
        }
    } else {
        eprintln!("[android] wayland: the seat advertises no pointer; running without one");
        std::ptr::null_mut()
    };
    let keyboard = if caps & WL_SEAT_CAPABILITY_KEYBOARD != 0 {
        unsafe {
            (wl.marshal_flags)(seat, WL_SEAT_GET_KEYBOARD, wl.keyboard_interface, 1, 0, std::ptr::null_mut::<c_void>())
        }
    } else {
        eprintln!("[android] wayland: the seat advertises no keyboard; running without one");
        std::ptr::null_mut()
    };
    // The touchscreen, on the same terms and for the same protocol reason:
    // `wl_seat.get_touch` on a seat with no touch capability is a
    // `missing_capability` error and the compositor answers it by disconnecting
    // the client. The capability check is not politeness.
    //
    // Bound here but *listened to* below, alongside the pointer and keyboard
    // listeners, because `bind_touch` -- which is what a hot-plugged screen
    // goes through -- needs the window to exist and this does not yet.
    let touch = if caps & WL_SEAT_CAPABILITY_TOUCH != 0 && !super::input::no_touch() {
        unsafe {
            (wl.marshal_flags)(seat, WL_SEAT_GET_TOUCH, wl.touch_interface, 1, 0, std::ptr::null_mut::<c_void>())
        }
    } else {
        // Worth saying out loud in both directions. A machine with no
        // touchscreen is the ordinary case and this line is how a developer
        // reading a log knows the touch path was never exercised rather than
        // exercised and broken -- which is the ambiguity that made the keyboard
        // take days.
        if super::input::no_touch() {
            eprintln!("[android] wayland: CORDIAL_NO_TOUCH=1; not asking the seat for a touchscreen");
        } else {
            eprintln!("[android] wayland: the seat advertises no touchscreen; running without one");
        }
        std::ptr::null_mut()
    };
    // What the engine will be told about this machine, decided here because
    // here is where the seat's answer exists and because `open()` runs before
    // `cordial_appbridge_init` builds the params. `report_touchscreen` owns
    // what `CORDIAL_INPUT_TOUCH` and `CORDIAL_NO_TOUCH` do to the seat's
    // answer; this only supplies the answer.
    super::input::report_touchscreen(caps & WL_SEAT_CAPABILITY_TOUCH != 0);

    // ---- pointer capture. Both halves are optional and independent of each
    // other only in principle: a lock with no relative pointer is a pointer
    // that has stopped moving and reports nothing, which is worse than no lock
    // at all. So the constraints manager is only kept when the relative-pointer
    // manager is there too, and the pair is treated as one capability.
    let relative_pointer = globals.relative_pointer_manager.and_then(|(name, ver)| {
        let mgr = bind(name, 1, ver, &RELATIVE_POINTER_MANAGER_INTERFACE, "zwp_relative_pointer_manager_v1");
        if mgr.is_null() || capture_pointer.is_null() {
            return None;
        }
        // SAFETY: `mgr` is a live proxy and `capture_pointer` is GDK's live,
        // borrowed pointer on this same connection. The argument list matches
        // `get_relative_pointer`'s "no" signature.
        let rp = unsafe {
            (wl.marshal_flags)(
                mgr,
                RELATIVE_POINTER_MANAGER_GET_RELATIVE_POINTER,
                &RELATIVE_POINTER_INTERFACE,
                1,
                0,
                std::ptr::null_mut::<c_void>(),
                capture_pointer,
            )
        };
        (!rp.is_null()).then_some(rp)
    });
    let pointer_constraints = match (globals.pointer_constraints, relative_pointer) {
        (Some((name, ver)), Some(_)) => {
            bind(name, 1, ver, &POINTER_CONSTRAINTS_INTERFACE, "zwp_pointer_constraints_v1")
        }
        _ => {
            if capture_pointer.is_null() {
                eprintln!(
                    "[android] wayland: GDK exposed no wl_pointer for its default seat; \
                     pointer capture is disabled rather than attaching a false lock to \
                     Cordial's secondary pointer"
                );
            } else {
                eprintln!(
                    "[android] wayland: this compositor advertises no \
                     zwp_pointer_constraints_v1/zwp_relative_pointer_manager_v1 pair; \
                     the pointer cannot be captured, so first person and camera drags \
                     will let the cursor leave the window"
                );
            }
            std::ptr::null_mut()
        }
    };

    // ---- text-input-v3: created against the seat, listener wired once the
    // window exists (below), since its handlers use `current()`.
    let text_input = text_input_manager.and_then(|mgr| {
        // The child's version is the manager's, read back rather than assumed:
        // this used to pass a literal 1 while the manager was bound at 2, which
        // made every version check on this side answer for a protocol the
        // compositor was not speaking.
        // SAFETY: `mgr`/`seat` are live proxies bound above.
        let version = unsafe { (wl.get_version)(mgr) };
        let ti = unsafe {
            (wl.marshal_flags)(
                mgr,
                TEXT_INPUT_MANAGER_GET_TEXT_INPUT,
                &TEXT_INPUT_INTERFACE,
                version,
                0,
                std::ptr::null_mut::<c_void>(),
                seat,
            )
        };
        (!ti.is_null()).then_some(ti)
    });

    let conn_fd = unsafe { (wl.get_fd)(display) };

    let egl = match WlEgl::load() {
        Ok(e) => Some(e),
        Err(e) => {
            // Not fatal here — the caller only needs a window and Vulkan does
            // not go through `wl_egl_window` at all (see `vulkan.rs`). A
            // GLES-only host without `libwayland-egl.so.1` (unlikely, but not
            // impossible on a minimal install) still gets a working Vulkan
            // path this way.
            eprintln!("[android] wayland: {e}; GLES window surfaces will not be available");
            None
        }
    };

    let host = WaylandWindow {
        wl,
        egl,
        display,
        host: HostWindowCell(host),
        compositor,
        subcompositor,
        surface,
        subsurface,
        parent_surface,
        seat,
        pointer,
        capture_pointer,
        keyboard,
        touch: std::sync::atomic::AtomicPtr::new(touch),
        text_input: text_input.unwrap_or(std::ptr::null_mut()),
        pointer_constraints,
        relative_pointer: relative_pointer.unwrap_or(std::ptr::null_mut()),
        locked_pointer: Mutex::new(std::ptr::null_mut()),
        lock_requested_at: Mutex::new(None),
        lock_inactive_since: Mutex::new(None),
        conn_fd,
        buffers: Mutex::new(Geometry { width: cw, height: ch, format: super::window::WINDOW_FORMAT_RGBA_8888 }),
        placed_at: Mutex::new((cx, cy)),
        egl_window: Mutex::new(std::ptr::null_mut()),
        xkb: Mutex::new(None),
        pointer_pos: AtomicU64::new(pack_pointer_position(0.0, 0.0)),
        pointer_buttons: AtomicI32::new(0),
        down_time_ms: AtomicI64::new(0),
        clock: std::time::Instant::now(),
        ime: Mutex::new(ImeState {
            preedit: None,
            pending: PendingImeGroup::default(),
            enabled: false,
            synced_generation: None,
            ime_producing: false,
        }),
        active_handle: AtomicI64::new(0),
        open_web_view_dialogs: AtomicI32::new(0),
        text_overlay_visible: AtomicBool::new(false),
        text_overlay_cache: Mutex::new(None),
        polled_textbox_info: Mutex::new(None),
        last_placement: Mutex::new(None),
    };
    let host = WINDOW.get_or_init(|| host);

    // Roblox's own font for the editor, out of the APK the user supplied. Done
    // here rather than at window construction because it needs the asset
    // manager, which is configured by the time the engine asks for a window.
    // A `None` is not an error: the editor falls back to Pango's choice and
    // only looks slightly wrong, where refusing to draw it would make typing
    // invisible.
    host.host.0.set_editor_font_family(super::editor_font::install().map(str::to_owned));

    // **The other direction: what the user typed, back to the engine.**
    //
    // The editor widget owns the text now, so this is the only path by which a
    // keystroke reaches Roblox -- `dispatch_key` deliberately stops short of
    // the buffer while a box has focus, because GDK has already delivered the
    // same key to the widget through its own `wl_keyboard`.
    //
    // Installed here rather than where the window is built because it needs
    // `WINDOW`, and `WINDOW` is what `get_or_init` above has just produced.
    // The closure captures nothing and reads the static, so it cannot outlive
    // what it points at.
    host.host.0.connect_editor_changed(|text, caret| {
        let Some(w) = WINDOW.get() else { return };
        // No focused box means no editor, and a change arriving anyway is the
        // widget being cleared on blur. Nothing to tell the engine about.
        let Some(which) = cordial_linker_sys::game_activity::focused_textbox() else { return };
        // The buffer becomes a mirror of the widget before anything reads it:
        // `send_current_text` takes its text from `text_buffer_snapshot`, so
        // this has to happen first or the engine is told the previous value.
        super::input::adopt_editor_text(text, caret);
        let handle = w.active_handle.load(Ordering::Relaxed);
        if handle != 0 {
            // AGDK's own text-input state, kept in step for the same reason
            // `dispatch_key` keeps it in step -- this is the path that used to
            // do it, and dropping it here would be a silent behaviour change
            // rather than a decision.
            let _ = cordial_linker_sys::game_activity::text_input(handle, text, caret, caret);
        }
        w.send_current_text(which);
        if handle != 0 {
            super::input::deliver_surface_redraw(handle);
        }
    });

    // Listeners that dereference `current()` can only be installed now.
    unsafe {
        if !pointer.is_null() {
            (host.wl.add_listener)(pointer, &POINTER_LISTENER as *const PointerListener as *const c_void, std::ptr::null_mut());
            // The cursor is hidden from `pointer_enter`, not here — see
            // `hide_pointer`. Sending it at setup was wrong twice over: there
            // is no valid serial to send yet, and the request has to be
            // repeated on every enter regardless.
        }
        if !keyboard.is_null() {
            (host.wl.add_listener)(keyboard, &KEYBOARD_LISTENER as *const KeyboardListener as *const c_void, std::ptr::null_mut());
        }
        if !touch.is_null() {
            (host.wl.add_listener)(touch, &TOUCH_LISTENER as *const TouchListener as *const c_void, std::ptr::null_mut());
            println!("[android] wayland: the seat has a touchscreen; wl_touch bound");
        }
        if !host.relative_pointer.is_null() {
            (host.wl.add_listener)(
                host.relative_pointer,
                &RELATIVE_POINTER_LISTENER as *const RelativePointerListener as *const c_void,
                std::ptr::null_mut(),
            );
        }
        if !host.text_input.is_null() {
            (host.wl.add_listener)(
                host.text_input,
                &TEXT_INPUT_LISTENER as *const TextInputListener as *const c_void,
                std::ptr::null_mut(),
            );
        }
        (host.wl.flush)(display);
    }

    Ok(host)
}

impl WaylandWindow {
    /// A resize GTK has already settled.
    ///
    /// The configure/ack handshake ADR-011 chose Wayland *for* still happens;
    /// it just happens in GTK now, which is the point of handing it the
    /// toplevel. By the time the content widget's allocation changes, the
    /// compositor and GTK have agreed on the new size, so this is the
    /// downstream half only: resize the EGL window if one exists, and tell the
    /// engine. Mirrors `window.rs::dispatch_configure`.
    fn apply_resize(&self, width: i32, height: i32) {
        // TEMPORARY INSTRUMENTATION -- not for commit. `CORDIAL_INSTR=1`.
        //
        // The early return below is the thing issue #7 needs measured, and
        // until now it was silent: the existing `[instr] surface_resized` lines
        // sit *after* it, so "apply_resize was never called with the fullscreen
        // size" and "it was called and returned without doing anything" looked
        // identical in a log. Those want opposite fixes.
        if instr_on() {
            eprintln!("[instr] apply_resize(entry) requested {width}x{height}");
        }
        if width <= 0 || height <= 0 {
            if instr_on() {
                eprintln!("[instr] apply_resize(reject) non-positive {width}x{height}");
            }
            return;
        }
        let format = {
            let mut g = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
            if g.width == width && g.height == height {
                if instr_on() {
                    eprintln!(
                        "[instr] apply_resize(early-return) already {}x{}; geometry() stays stale",
                        g.width, g.height
                    );
                }
                return;
            }
            if instr_on() {
                eprintln!(
                    "[instr] apply_resize(accept) {}x{} -> {width}x{height}",
                    g.width, g.height
                );
            }
            g.width = width;
            g.height = height;
            g.format
        };
        // `config::set_screen` is otherwise only called once, right after the
        // window first opens (`load.rs`), so `AConfiguration` kept answering
        // the launch size through every later resize -- fullscreen included --
        // while the render surface below tracked the real one. Same gap as
        // `window.rs::dispatch_configure`'s X11 path, closed the same way;
        // see that function's doc comment for what this does and does not
        // fix. Deliberately before the EGL resize, matching `load.rs`'s own
        // ordering of "tell the engine what the screen is" before "hand it
        // the surface".
        super::config::set_screen(width, height);
        let egl_win = *self.egl_window.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some(egl), false) = (&self.egl, egl_win.is_null()) {
            // SAFETY: `egl_win` was created by `wl_egl_window_create` and is
            // still live (never destroyed for the process's lifetime, same
            // as everything else in this single-window runtime).
            unsafe { (egl.resize)(egl_win, width, height, 0, 0) };
        }
        let handle = self.active_handle.load(Ordering::Relaxed);
        if instr_on() {
            eprintln!("[instr] surface_resized -> {width}x{height}");
        }
        if handle != 0 {
            if let Err(e) = cordial_linker_sys::game_activity::surface_resized(handle, format, width, height) {
                super::trace(format_args!("wayland: surface resize failed: {e}"));
            }
        }
        if instr_on() {
            eprintln!("[instr] surface_resized {width}x{height} returned");
        }
        // Nudge the engine to repaint *now*, because a resized surface it has
        // not drawn into yet is visible, not merely stale.
        //
        // `set_position` latches on the parent commit above and takes effect
        // immediately; the engine's buffer only changes size when the engine
        // next renders. Between the two the compositor shows a buffer of the
        // old size at the new position, and nothing clips a subsurface to its
        // parent. Reported symptom, restoring from fullscreen: "it restores
        // position first, which means it takes up that space on the second
        // monitor, then redraw kicks in and puts the size back in its place."
        // The lag is the measured part. On this run the engine
        // re-queried surface caps 3 log lines after an accepted resize but did
        // not recreate the swapchain for another 5 pumps, and on one transition
        // not for 20 — roughly 250ms to a second at the idle pump interval.
        //
        // This is `onSurfaceRedrawNeededNative`, which `sync_canvas_geometry`
        // records as having been tried and not helped. That attempt called it
        // every pump, and judged it on `vkQueuePresentKHR` counts over an idle
        // 240-second run — the one measurement this project has established is
        // an idle throttle rather than a frame rate, so it could not have shown
        // a difference either way. Here it fires only on a resize the engine
        // has just been told about, and the measurement is the pump distance
        // from accept to swapchain recreate, with `CORDIAL_NO_REDRAW_NUDGE=1`
        // as the control.
        //
        // **INFERRED.** That the nudge shortens the gap is reasoning, not a
        // result: the control has not been run yet, so this may turn out to be
        // as inert as the last attempt and should be deleted if it is. What is
        // measured is the gap itself, quoted above. The parent commit in
        // `sync_canvas_geometry` is the part that is confirmed.
        if handle != 0 && !redraw_nudge_disabled() {
            match cordial_linker_sys::game_activity::surface_redraw_needed(handle) {
                Ok(Some(())) => {}
                // The engine does not always have a native to call here; that
                // is not an error, it just means the nudge is unavailable.
                Ok(None) => {}
                Err(e) => super::trace(format_args!("wayland: redraw nudge failed: {e}")),
            }
            if instr_on() {
                eprintln!("[instr] redraw_needed sent after {width}x{height}");
            }
        }
    }

    /// Keep the canvas over the window's content area.
    ///
    /// GTK owns the layout, so this reads the content widget's allocation
    /// rather than tracking a configure sequence of its own — the header bar's
    /// height, the window's CSD inset and every resize all arrive through the
    /// same one number pair. Called every pump; both halves are no-ops unless
    /// something actually moved, because `set_position` costs a parent repaint
    /// and `apply_resize` costs the engine a surface-changed callback.
    fn sync_canvas_geometry(&self) {
        let Some((x, y, w, h)) = self.host.0.content_rect() else {
            // Say so, once per run of them. `content_rect` returns `None` for a
            // zero or negative allocation, and a compositor sending a 0x0
            // configure — "you choose your own size", which is legal — would
            // land here and leave the canvas at whatever it was, which is
            // exactly the reported fullscreen symptom: content painted at the
            // pre-fullscreen size in a fullscreen slot. Eight scripted
            // transitions across two runs never produced one, so if a hand-made
            // fullscreen does, this line is the difference and it is worth more
            // than another round of theorising about which call was missed.
            if !NO_CONTENT_RECT.swap(true, Ordering::Relaxed) {
                println!("[android] wayland: no content rectangle to place the canvas by");
            }
            return;
        };
        // Punch the canvas out of the parent's opaque and input regions, every
        // time the geometry moves. GTK recomputes both on a resize, so setting
        // them once at startup would be quietly undone by the first one -- and
        // the symptom of that is a canvas that stops taking clicks with nothing
        // on screen to say why.
        self.host.0.set_canvas_cutout(x, y, w, h);

        // The other half of the same signal: a rectangle that came back after
        // there was none, and the size it came back with.
        if NO_CONTENT_RECT.swap(false, Ordering::Relaxed) {
            println!("[android] wayland: content rectangle is back, {w}x{h} at ({x}, {y})");
        }
        let moved = {
            let mut placed = self.placed_at.lock().unwrap_or_else(|e| e.into_inner());
            let moved = *placed != (x, y);
            *placed = (x, y);
            moved
        };
        // **The first placement has to happen even when nothing moved**, and
        // leaving that out rendered a white window for a whole session.
        //
        // `moved` compares against `placed_at`, which is this side's
        // bookkeeping of where the subsurface sits. The compositor has been
        // told nothing at that point: a `wl_subsurface` that has never been
        // `set_position`ed and never committed does not display, whatever this
        // struct believes about it. So a run whose geometry happens to match
        // the initial bookkeeping value on the very first sync skips the only
        // call that would have put the canvas on screen, and skips it for ever
        // -- every later sync also sees no movement.
        //
        // Observed 2026-08-22 on a hand-run `cordial-run`: the engine drew
        // 20,868 frames, the swapchain was created twice, and the stall
        // detector reported `rect=Some((25, 71, 1280, 721)) placed=(25, 71)
        // setpos=0 qcommit=0` -- our bookkeeping agreeing with itself while
        // Wayland had never heard of it.
        //
        // The shell path only ever worked by accident. It fits the window to a
        // monitor during bring-up, so the geometry changes, `moved` goes true
        // and the placement lands as a side effect of the resize. That is why
        // this survived: the launcher everybody uses papers over it, and only a
        // direct run whose geometry never changes shows it.
        let first = !EVER_PLACED.swap(true, Ordering::Relaxed);
        if moved || first {
            INSTR_SET_POSITIONS.fetch_add(1, Ordering::Relaxed);
            INSTR_QUEUE_COMMITS.fetch_add(1, Ordering::Relaxed);
            if instr_on() {
                eprintln!("[instr] set_position({x}, {y}) size={w}x{h}");
            }
            // SAFETY: `self.subsurface` is a live proxy for the process's
            // lifetime and `set_position`'s signature is "ii".
            unsafe {
                (self.wl.marshal_flags)(
                    self.subsurface,
                    WL_SUBSURFACE_SET_POSITION,
                    std::ptr::null(),
                    1,
                    0,
                    x,
                    y,
                );
            }
            // Latched on the parent's commit, not ours — see `HostWindow::
            // queue_commit`.
            self.host.0.queue_commit();
            // ...and then commit the parent here as well, because *asking* GTK
            // to repaint is not the same as GTK repainting. `queue_draw` marks
            // the widget dirty; GTK4 still renders through GSK, and a frame
            // whose render node is unchanged produces no damage, no attach and
            // no `wl_surface.commit`. On a fullscreen transition nothing in
            // GTK's own content changes — the header bar draws identically —
            // so the request can be honoured with no commit at all, and the
            // `set_position` above stays pending indefinitely.
            //
            // That is issue #7. Everything Cordial records looks right, because
            // all of it is bookkeeping on this side: `content_rect` returns the
            // new rectangle, `placed_at` updates, `set_position` is marshalled,
            // `apply_resize` accepts and the swapchain follows. The one step
            // that is not ours — the parent commit that makes the position take
            // effect — is the one that does not happen. The reporter's
            // "alt-tabbing fixes it" is the proof: focus changes the header
            // bar's backdrop styling, which *is* real damage, so GTK commits
            // and the compositor applies a position that had been sitting
            // pending since the transition.
            //
            // Safe from here specifically because this runs on the thread that
            // ran `gtk_init`, immediately after `host.pump()` returned, so GTK
            // is between frames rather than part-way through staging one. A
            // commit with nothing attached applies pending subsurface state and
            // changes nothing else.
            //
            // SAFETY: `self.parent_surface` is GTK's toplevel `wl_surface`,
            // live for the process's lifetime, and `commit` takes no arguments.
            unsafe {
                (self.wl.marshal_flags)(
                    self.parent_surface,
                    WL_SURFACE_COMMIT,
                    std::ptr::null(),
                    1,
                    0,
                );
            }
        }
        // `onSurfaceRedrawNeededNative` was tried here, on both halves of this
        // function, and **did not help** — see docs/NEXT.md §1e. The reasoning
        // was that `window.rs` drives that native from the final X11 `Expose`
        // and this backend drives it from nowhere, so an idle engine has nothing
        // telling it the canvas moved. The measurement says the engine already
        // repaints on `surface_resized` by itself: over the idle fullscreen
        // cycle of two otherwise identical 240-second runs, presents totalled
        // ~75 without the call and ~74 with it. Left out rather than kept as a
        // plausible-sounding no-op.
        //
        // **It is called again now, and this paragraph must not be read as
        // saying otherwise.** `apply_resize`, two lines down, sends it once per
        // accepted resize rather than once per pump, and the comment there says
        // what is different about that and admits the control has not been run.
        // The sentence above is about the attempt that was withdrawn; someone
        // reading it as "nothing drives this native" would be wrong.
        self.apply_resize(w, h);
    }

    /// A web-view dialog just opened; make sure the engine's canvas is not
    /// compositing over it.
    ///
    /// See the module doc's "A web-view dialog is invisible by default"
    /// section for the mechanism. Reference-counted rather than a flag: two
    /// dialogs can be open at once (the protocol has no rule against it), and
    /// the second one opening while the first is already up must not re-issue
    /// `place_below` -- doing so costs nothing functionally (the subsurface is
    /// already below), but it is not idempotent to *observe*, and a call site
    /// that fires on every open rather than only the 0-to-1 edge is a call
    /// site that will eventually be trusted to mean "just lowered it", which
    /// would be false the second time.
    pub fn webview_dialog_opened(&self) {
        if self.open_web_view_dialogs.fetch_add(1, Ordering::SeqCst) == 0
            && !self.text_overlay_visible.load(Ordering::SeqCst)
        {
            self.set_engine_stacking(false);
        }
        // **Claim the whole window for GTK while the dialog is up.**
        //
        // The parent's input region normally has the canvas rectangle punched
        // out of it, which is what lets a click over the game reach the engine
        // at all -- `host_window::refresh_input_region` has the measurement.
        // An `AdwDialog` draws inside that same toplevel and is centred over
        // the canvas, so without this its buttons sit in the engine's
        // rectangle and every click on them misses GTK entirely. That is the
        // reported "I cant click on the webview's items".
        //
        // Unconditional rather than gated on the 0-to-1 edge, and cheap: the
        // setter returns immediately when the flag has not changed.
        self.host.0.set_dialog_up(true);
        // A `relative_motion` sample can already be sitting in
        // `PENDING_UNLOCKED_DELTA`, waiting for the `wl_pointer.motion` that
        // drains it, at the exact moment this runs -- this is invoked from a
        // GTK-thread closure answering the engine's own `openWindow`
        // (`load.rs`), asynchronous to whatever the pointer was doing.
        // `relative_pointer_motion`'s own `dialog_in_front` check stops
        // anything *more* from accumulating once the dialog is up, but does
        // nothing about a sample that got there first -- left alone, it
        // survives the whole dialog and is handed to the first real report
        // after `webview_dialog_closed`, applying movement from before the
        // dialog opened to a cursor position from after it. Called
        // unconditionally rather than gated on the 0-to-1 edge like the
        // stacking call above: a second dialog opening while the first is
        // still up finds nothing to forget, since the same early-return has
        // been stopping accumulation since the first one opened, so gating
        // this would add a condition with no observable effect.
        super::input::forget_pending_unlocked_delta();
    }

    /// The mirror of [`Self::webview_dialog_opened`]: call once a dialog has
    /// closed. Only the last-close (1-to-0) edge raises the canvas back —
    /// with two dialogs open, closing one must leave the engine hidden behind
    /// whichever is still up.
    pub fn webview_dialog_closed(&self) {
        if self.open_web_view_dialogs.fetch_sub(1, Ordering::SeqCst) == 1
            && !self.text_overlay_visible.load(Ordering::SeqCst)
        {
            self.set_engine_stacking(true);
        }
        // Only on the last close. With two dialogs up, closing one must leave
        // the window claimed for the other -- the same edge the restack above
        // uses, and for the same reason.
        if self.open_web_view_dialogs.load(Ordering::SeqCst) == 0 {
            self.host.0.set_dialog_up(false);
        }
        // Nothing should have accumulated while a dialog was in front — see
        // `webview_dialog_opened` — but a last dialog closing is the same
        // kind of pointer-meaning boundary `pointer_enter`, `pointer_leave`
        // and the lock transitions already treat as one, and forgetting here
        // too costs nothing rather than trusting that invariant forever.
        super::input::forget_pending_unlocked_delta();
    }

    fn update_text_overlay(
        &self,
        generation: u32,
        revision: u64,
        text: &str,
        caret: i32,
        info: cordial_linker_sys::game_activity::RawTextBoxInfo,
        placed: Placed,
    ) {
        let mut cache = self.text_overlay_cache.lock().unwrap_or_else(|e| e.into_inner());
        let unchanged = cache.as_ref().is_some_and(|(g, r, old, old_caret, old_info)| {
            *g == generation
                && *r == revision
                && old == text
                && *old_caret == caret
                && *old_info == info
        });
        if unchanged {
            return;
        }
        *cache = Some((generation, revision, text.to_owned(), caret, info));
        drop(cache);
        *LAST_EDITOR_RECT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((info.x, info.y, info.width, info.height, placed.token()));
        // **Which font this box is in, if the engine named one.**
        //
        // A synthesised spec is skipped rather than read: `fallback_textbox_info`
        // leaves every int at zero, and zero is a value the shipped table has no
        // row for. Reading it would report "id 0 is unresolvable" about a
        // number the engine never said, which is a stub lying about where its
        // input came from.
        let face = match (placed, super::editor_font::font_id(&info)) {
            (Placed::Fallback, _) | (_, None) => None,
            (_, Some(id)) => match super::editor_font::face_for_id(id) {
                Some(face) => Some((id, face)),
                None => {
                    // An id the shipped table has no row for -- a marketplace
                    // font, or one this build renumbered. Named once and then
                    // drawn in the default, because the alternatives are worse
                    // in both directions: drawing nothing makes typing
                    // invisible, and drawing silently leaves a wrong font
                    // looking like a rendering bug with no number to report.
                    let fallback = super::editor_font::default_face();
                    super::editor_font::log_unresolved(id, fallback);
                    fallback.map(|face| (id, face))
                }
            },
        };

        if super::input::trace_text() {
            // Which of the three sources won, and the numbers it won with.
            // Placement is the question this whole path exists to answer, and
            // it was previously only answerable by photographing the window --
            // `showKeyboard`'s spec is in the log but is not what gets used
            // when the getter or the fallback supplies the geometry instead.
            //
            // **All five candidate ints, on the same line as the family that
            // was resolved from whichever one is selected.** `xAlign`/`yAlign`
            // (slots 6/7) and the font slot (9) are confirmed rather than
            // candidates now -- mocktail's `NativeTextBoxInfo` constructor
            // field order, credited in `RawTextBoxInfo`'s own doc comment --
            // but `i10`/`i11` stay printed numerically alongside the resolved
            // family for the same reason they always were: with the candidates
            // and the outcome side by side, whichever int moved *and* changed
            // the family is the field, and this line is the whole experiment.
            // Split across two lines or two sources it would need correlating
            // by hand, which is how the last five candidates survived a
            // capture each.
            let (slot, id, family, ratio) = match (super::editor_font::font_slot(), face.as_ref()) {
                (Some(slot), Some((id, face))) => {
                    (slot.to_string(), id.to_string(), face.family.clone(), face.from_rbx_font_ratio)
                }
                (Some(slot), None) => {
                    (slot.to_string(), "n/a".to_owned(), "(process default)".to_owned(), 1.0)
                }
                (None, _) => ("off".to_owned(), "n/a".to_owned(), "(process default)".to_owned(), 1.0),
            };
            eprintln!(
                "[cordial] text editor placed from {} x={} y={} w={} h={} \
                 xAlign={} yAlign={} i9={} i10={} i11={} fontSlot={slot} fontId={id} family={family:?} \
                 fontSize={} fromRbxFontRatio={ratio} drawnFontSize={}",
                placed.source(),
                info.x, info.y, info.width, info.height,
                info.x_alignment, info.y_alignment, info.i9, info.i10, info.i11,
                info.font_size, info.font_size * ratio,
            );
        }

        // **The size is corrected here, once, rather than left for
        // `host_window.rs` to guess at.** The engine's `fontSize` is
        // denominated in whichever font *it* would have drawn the box in, and
        // `fromRbxFontRatio` is that font's own manifest row saying how its
        // metrics relate to Android's -- so the correction belongs beside the
        // font lookup that resolved a face at all, not beside the Pango
        // attribute that only knows a number arrived. Left at 1.0 when no
        // per-box face was resolved, which is every id this build has no row
        // for and the synthesised fallback bar -- multiplying by an unknown
        // ratio would be a second guess, not a correction.
        let font_size = match face.as_ref() {
            Some((_, f)) => info.font_size * f.from_rbx_font_ratio,
            None => info.font_size,
        };

        self.host.0.set_text_overlay(Some(cordial_shell::host_window::TextOverlay {
            text,
            caret_chars: caret,
            x: info.x,
            y: info.y,
            width: info.width,
            height: info.height,
            font_size,
            text_color: info.text_color as u32,
            font_family: face.as_ref().map(|(_, f)| f.family.as_str()),
            font_weight: face.as_ref().map_or(400, |(_, f)| f.weight),
            font_italic: face.as_ref().is_some_and(|(_, f)| f.italic),
            password: matches!(info.i10, 5 | 9 | 10),
            // Only the placed bar draws its own chrome. An editor held at the
            // previous box's place is still sitting on a real field and must
            // not suddenly grow a background.
            fallback: placed == Placed::Fallback,
            // Passed straight through rather than mapped to a GTK type here:
            // `host_window.rs` is where every other Android-to-Pango
            // conversion in this spec already lives (`pango_weight`, the CSS
            // colour string), and `gtk_xalign`/`vertical_placement` follow the
            // same pattern. See `RawTextBoxInfo::x_alignment` for why these
            // two ints are confirmed and not a guess.
            x_alignment: info.x_alignment,
            y_alignment: info.y_alignment,
        }));
        if !self.text_overlay_visible.swap(true, Ordering::SeqCst)
            && self.open_web_view_dialogs.load(Ordering::SeqCst) == 0
        {
            self.set_engine_stacking(false);
        }
    }

    /// Where to draw the editor when the engine never told us where the box is.
    ///
    /// A bar across the lower third of the canvas, which is where an on-screen
    /// keyboard's own text would sit on the device this engine was built for.
    /// Deliberately not the exact box: we do not know where that is, and
    /// pretending to would put a correctly-styled editor in the wrong place,
    /// which reads as a rendering bug. A bar that is obviously a bar reads as
    /// what it is.
    fn fallback_textbox_info(&self) -> cordial_linker_sys::game_activity::RawTextBoxInfo {
        let (w, h) = match self.host.0.content_rect() {
            Some((_, _, w, h)) => (w as f32, h as f32),
            // No allocation yet is not a reason to draw nothing either; a
            // plausible default is still legible and still beats invisible.
            None => (1280.0, 720.0),
        };
        let width = (w * 0.6).max(240.0);
        let height = 44.0;
        cordial_linker_sys::game_activity::RawTextBoxInfo {
            x: (w - width) / 2.0,
            y: (h * 0.72).min(h - height - 8.0).max(0.0),
            width,
            height,
            font_size: 18.0,
            // White, which is legible against the engine's own dark chrome and
            // against most places a text box appears. The real spec carries the
            // box's colour and is preferred whenever it exists.
            text_color: 0x00FF_FFFFu32 as i32,
            // Explicit, not left to `Default::default()`'s zero. A synthesised
            // box has no real `yAlignment` to report, and `0` is `Top` --
            // `vertical_placement`'s Top branch would shrink this pill to one
            // line's height and anchor it at the top rather than centring it,
            // which is a regression in the one placement this project has
            // actually verified (`tools/text-input-e2e.py`, 2026-08-25).
            // `x_alignment`'s default of `0` (Left) needs no such override:
            // that already matches what this bar drew before alignment was
            // read at all.
            y_alignment: 1,
            ..Default::default()
        }
    }

    /// Ask `nativeGetTextBoxInfo` where the focused box is, at most ten times a
    /// second, and remember the last answer worth using.
    ///
    /// Only called when the spec `showKeyboard` volunteered is unusable, so on
    /// the common path this costs nothing at all.
    ///
    /// **Ten a second, and the number is a compromise between two measured
    /// things.** `sync_text_overlay` runs off `looper::pump`, which comes round
    /// roughly twenty times a second, and a JNI call that constructs a Java
    /// object on every tick is a cost on the one thread this project defends.
    /// Against that, the search modal's geometry went from unusable to correct
    /// somewhere between 0 and 1 second after focus, so a poll interval near a
    /// second would be a visible lag before the editor jumped onto the field.
    /// 100 ms is half the pump rate -- at most every other tick -- and latches
    /// on within a tenth of a second of the engine having an answer.
    ///
    /// Polling continues after a usable answer arrives, because the answer
    /// moves: the modal's field measured `w=592` and then `w=564` once there
    /// was text in it. Following that costs one overlay rebuild, not one per
    /// tick, because `sync_text_overlay`'s cache compares the geometry it drew
    /// with the geometry it is about to draw and returns early when they match.
    fn polled_textbox_info(
        &self,
        generation: u32,
    ) -> Option<cordial_linker_sys::game_activity::RawTextBoxInfo> {
        /// Half the pump rate. See the doc comment.
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

        let native = super::input::textbox_info_native();
        if native.is_null() {
            return None;
        }
        let mut state = self.polled_textbox_info.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        match state.as_ref() {
            // Same box, asked recently enough: reuse whatever the last poll
            // established rather than asking again.
            Some(p) if p.generation == generation => {
                if now.duration_since(p.asked) < POLL_INTERVAL {
                    return p.usable;
                }
            }
            // A different box, or the first one. Nothing carries over --
            // reusing another box's numbers is the thing `android_classes.cpp`
            // refuses by design.
            _ => {
                *state = Some(PolledTextBoxInfo { generation, asked: now, usable: None });
            }
        }
        let carried = state.as_ref().and_then(|p| p.usable);
        let answer = match cordial_linker_sys::game_activity::textbox_info_now(native) {
            // **A zero height is the trap this call brings with it.** Asked on
            // the same pump tick as `showKeyboard`, the search modal answered
            // `x=596 y=10 w=42 h=0` -- caught mid-animation, expanding out of
            // the header search bar it replaces. Non-zero x, y and width make
            // it look like an answer; the zero height makes it invisible. Same
            // test as the remembered spec gets, for the same reason.
            Ok(Some(i)) if i.width > 0.0 && i.height > 0.0 => Some(i),
            // Null is ordinary, not a failure: it is what the whole sign-in
            // page answers. Keep whatever the last poll found rather than
            // dropping an editor that is currently placed correctly.
            Ok(_) => carried,
            Err(e) => {
                if super::input::trace_text() {
                    eprintln!("[cordial] nativeGetTextBoxInfo failed: {e}");
                }
                carried
            }
        };
        *state = Some(PolledTextBoxInfo { generation, asked: now, usable: answer });
        answer
    }

    /// Where the focused box is, from the best source that will answer.
    ///
    /// One function because there are two painters -- `sync_text_overlay` on
    /// the pump and `send_current_text` on every keystroke -- and they used to
    /// resolve this separately. When the `nativeGetTextBoxInfo` source was
    /// added only the first learned about it, so the editor sat correctly on
    /// the search modal until the user typed, at which point the other one
    /// re-placed it in the fallback bar from the same zeroed spec. Two callers
    /// deciding the same thing differently is the bug; one function is the fix.
    fn resolve_textbox_geometry(
        &self,
        generation: u32,
    ) -> (cordial_linker_sys::game_activity::RawTextBoxInfo, Placed) {
        /// How long the editor may be held at the last box's place.
        ///
        /// Long enough to cover the search modal, which is the case this
        /// exists for: it focuses with a zeroed spec and `nativeGetTextBoxInfo`
        /// answers about a second later. Short enough that a box which will
        /// never report geometry still reaches the fallback bar promptly,
        /// rather than leaving the editor stranded on a field that is no longer
        /// on screen.
        const CARRY_OVER: std::time::Duration = std::time::Duration::from_millis(1_500);

        let found = match cordial_linker_sys::game_activity::focused_textbox_info() {
            Some(info) if info.width > 0.0 && info.height > 0.0 => Some(info),
            _ => self.polled_textbox_info(generation),
        };
        let mut last = self.last_placement.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(info) = found {
            *last = Some(LastPlacement { info, at: std::time::Instant::now() });
            return (info, Placed::Engine);
        }
        // **Hold still rather than jump.** Clicking the home search bar opens a
        // modal, and the engine focuses that modal before it has laid out: the
        // spec is `x=0 y=0 w=0 h=0` and the real numbers arrive about a second
        // afterwards. Falling straight through to the placed bar sent the
        // editor to the bottom of the window for that second and then back up
        // to a field ten pixels from where it started -- measured on
        // 2026-08-25, two fallback placements in one focus, both of them
        // visible. The modal replaces the search bar in the same place at the
        // same height, so staying put is very nearly right and is never as
        // wrong as the bottom of the screen.
        if let Some(p) = last.as_ref() {
            if p.at.elapsed() < CARRY_OVER {
                return (p.info, Placed::Carried);
            }
        }
        (self.fallback_textbox_info(), Placed::Fallback)
    }

    fn sync_text_overlay(&self) {
        let Some(_which) = cordial_linker_sys::game_activity::focused_textbox() else {
            if self.text_overlay_visible.swap(false, Ordering::SeqCst) {
                *self.text_overlay_cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
                *LAST_EDITOR_RECT.lock().unwrap_or_else(|e| e.into_inner()) = None;
                self.host.0.set_text_overlay(None);
                if self.open_web_view_dialogs.load(Ordering::SeqCst) == 0 {
                    self.set_engine_stacking(true);
                }
            }
            return;
        };
        let generation = cordial_linker_sys::game_activity::textbox_generation();
        let revision = super::input::text_buffer_revision();
        // **A missing spec is not a reason to draw nothing.**
        //
        // The engine does not paint a focused TextBox's own text -- established
        // in docs/NEXT.md §1 from the dex and confirmed by experiment -- so if
        // this returns early the characters are invisible while the box has
        // focus. Reported exactly that way: "characters are still invisible
        // till unfocus".
        //
        // **"and the engine finally draws them" is measured, and something has
        // since broken it.** Reported 2026-08-31: "when you have a text box,
        // you click off the text box, the text becomes invisible", on Intel
        // graphics under Mutter.
        //
        // That is a regression, not the engine's ordinary behaviour. §"Why the
        // text is invisible while you type" in docs/NEXT.md has the table, from
        // 2026-08-03: at t=44 the password field is clicked, the username box
        // blurs, and the window shows `abc`. Refocusing empties it again and
        // typing `d` gives `xyzd` on the next blur. The engine holds the text
        // the whole time and withholds only the drawing, and only while the
        // box has focus.
        //
        // **It is also not Sober's NVIDIA text-renderer bug**, which is what
        // this comment said for one commit. Sober #1845 and #1026 are real and
        // are a different fault: NVIDIA on Sway, DWL, Niri or Hyprland, fine on
        // KDE Plasma on the same system, answered by Sober's own maintainer
        // with an opt-in `SOBER_FORCE_NEW_TEXT_RENDERER`. Intel under Mutter is
        // on the working side of every one of those correlations, so citing
        // them here was an attribution to the first neighbouring bug that
        // looked similar. Left written down because it is the kind of wrong
        // answer that is easy to reach twice.
        //
        // What changed between the 2026-08-03 measurement and the report is
        // this overlay: back then nothing was drawn during editing and the
        // engine's blur-time rendering was the only rendering there was. The
        // suspicion is therefore that something on the blur path clears the
        // engine's copy -- `connect_editor_changed` guards against exactly that
        // by returning when no box has focus, so if it is that, the guard is
        // losing a race rather than missing.
        //
        // Unmeasured. `CORDIAL_TRACE_TEXT=1` across focus, type, blur settles
        // it: an empty `syncTextbox` going out at blur means the text is being
        // cleared, and no such call means the engine has it and stopped
        // drawing it.
        //
        // The spec is the geometry Android would style a real EditText with, and
        // it arrives either as `showKeyboard`'s NativeTextBoxInfo or from a
        // remembered `<init>`.
        //
        // **This used to say "on the sign-in page neither fires", and that is
        // no longer true.** It was true when it was written, and it stopped
        // being true when the `<init>` hook was corrected to the dex's real
        // fifteen-argument signature: the spec was always being constructed,
        // Cordial was simply not capturing it, so `spec_known` was false and
        // the page looked as though it volunteered nothing. Measured on
        // 2026-08-25 on a fresh signed-out profile, the username field arrives
        // here as `x=470 y=264 w=340 h=22` and the password field as
        // `x=470 y=330 w=304 h=22`, both from `showKeyboard`, both correct on
        // a composited screenshot. The old sentence survived the fix that
        // falsified it, which is the ordinary way a comment starts lying.
        //
        // **Asking the engine directly helps on some pages and not others, and
        // this used to say only the half of that which was measured first.**
        // `nativeGetTextBoxInfo` returned null for the whole of the sign-in
        // page over three runs -- taken before that hook fix, and not re-run
        // since, because `showKeyboard` now answers first there and the getter
        // is never reached. Treat it as history rather than as a current
        // reading. For the search modal,
        // the box that arrives here with `w=0 h=0`, the same call returns real
        // geometry about a second later: `x=596 y=10 w=42 h=0` on the pump tick
        // after focus, then `x=332 y=10 w=592 h=36` at one second and for the
        // rest of the box's life, twice out of two runs on 2026-08-25 with
        // identical numbers. So the engine has the geometry; what it does not
        // have is the geometry at the moment it volunteers a spec.
        //
        // Hence the order below: `showKeyboard` first because it is exact and
        // free, the getter second and only when the first is unusable, the
        // fallback bar third and only when neither answered. See
        // [`Self::polled_textbox_info`] for the rate limit and why it is 10 Hz.
        //
        // **The fallback below is now a safety net nothing currently lands
        // in, and it stays.** Every box tried on 2026-08-25 -- the home search
        // bar, the search modal it opens, the sign-in username and password
        // fields -- is placed from real geometry by one of the two sources
        // above. No box is known that supplies neither. That is a reason to
        // keep the net rather than to remove it: it has been reached before,
        // the set of boxes here is not the set of boxes Roblox ships, and the
        // failure it catches is invisible typing, which is the bug this whole
        // path exists for. It is untested as of that date and should be
        // treated as such.
        //
        // So when there is no spec, place the editor ourselves rather than
        // abandoning it. It will not sit exactly over the box, and that is the
        // honest trade: a legible bar in a fixed place beats invisible typing,
        // and the alternative on offer -- reusing the previous box's numbers --
        // is the one `android_classes.cpp` already refuses, because an editor
        // styled from a stale spec looks like a layout bug rather than a missing
        // value.
        // **A zero-sized spec is not geometry, and it is worse than none.**
        //
        // Roblox hands out both. Clicking the home search bar gives
        // `x=516 y=10 w=358 h=36`, which is exactly where that bar is; the
        // search modal it opens then reports `x=0 y=0 w=0 h=0`. Taking the
        // second at face value places a 0x0 editor -- present, correct, and
        // invisible -- and, worse, it looks like a real spec so the fallback
        // never runs. Found by `tools/text-entry-check.sh` on its first pass,
        // which is the whole reason that script exists.
        let (info, placed) = self.resolve_textbox_geometry(generation);
        if self
            .text_overlay_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|(g, r, _, _, old_info)| {
                *g == generation && *r == revision && *old_info == info
            })
        {
            return;
        }
        let (text, caret) = super::input::text_buffer_snapshot();
        self.update_text_overlay(
            generation,
            super::input::text_buffer_revision(),
            &text,
            caret,
            info,
            placed,
        );
    }

    /// Reorder the engine's subsurface relative to `parent_surface` — the
    /// only other member of this window's subsurface stack, and so the only
    /// valid reference for `place_above`/`place_below` to name (see the
    /// opcode constants' own comment).
    ///
    /// `above = true` restores the default the subsurface was created with —
    /// engine compositing over GTK's content, correct for the ordinary case
    /// of no dialog open. `above = false` is the fix: GTK's content,
    /// including anything an `AdwDialog` drew into it, shows through instead.
    ///
    /// Follows `sync_canvas_geometry`'s own fix for the same double-buffering
    /// hazard `set_position` has: the reorder is pending on the *subsurface*
    /// but does not take effect until the *parent's* next commit, and asking
    /// GTK to redraw (`queue_commit`) is not the same as GTK having redrawn
    /// (see that function's "issue #7" comment) — so this commits
    /// `parent_surface` directly as well, the same belt-and-braces the
    /// geometry sync already needed.
    fn set_engine_stacking(&self, above: bool) {
        // The window has to stop painting its own background over the canvas
        // for a lowered engine to be visible at all -- punching the opaque
        // region is not enough, because GTK's pixels are still there. Measured:
        // region punched, background opaque, engine lowered, canvas completely
        // black. Paired here so the two can never disagree, and so the window
        // is only ever see-through while something is painting underneath it.
        // **Order matters, and getting it wrong costs a visible frame.**
        //
        // Going *down*, the background must already be transparent when the
        // restack lands: a CSS class change is honoured on GTK's next frame
        // while the restack and the parent commit below go out immediately, so
        // doing them in the written order hands the compositor "the canvas is
        // underneath now" together with a parent buffer that is still opaque.
        // The engine vanishes for exactly one frame and comes back when GTK
        // next paints -- reported as "once you press it, roblox disappears for
        // a frame then reappears".
        //
        // Going *up* the same reasoning runs backwards: restack first, and the
        // canvas is opaque and covering before the background stops being
        // transparent, so there is no frame where the desktop shows through.
        if !above {
            self.host.0.set_canvas_see_through(true);
            self.host.0.repaint_now();
        }
        let opcode = if above { WL_SUBSURFACE_PLACE_ABOVE } else { WL_SUBSURFACE_PLACE_BELOW };
        // SAFETY: `self.subsurface` is a live proxy for the process's
        // lifetime; `self.parent_surface` is the only valid sibling reference
        // for a window with exactly one subsurface, and is itself a live
        // proxy for the process's lifetime. `place_above`/`place_below`'s
        // signature is "o" — one existing object argument, no new proxy
        // created, hence the null interface.
        unsafe {
            (self.wl.marshal_flags)(
                self.subsurface,
                opcode,
                std::ptr::null(),
                1,
                0,
                self.parent_surface,
            );
        }
        self.host.0.queue_commit();
        if !above {
            // **Say "nothing here is opaque" ourselves, in the same commit as
            // the restack.**
            //
            // Cordial already asks GDK for this through
            // `set_opaque_region(None)`, and the game still went black in an
            // experience with every other explanation eliminated -- GTK
            // painting (every descendant forced transparent), the CSS class
            // (instrumented identical), a stale region (fixed), frame
            // starvation (200ms of pumping). What none of those could rule out
            // is GDK recomputing its own region and committing it after ours,
            // because that happens inside GTK where this code cannot see.
            //
            // Sending it on the wire here removes the question. The request is
            // double-buffered like everything else, so landing it immediately
            // before the commit below means the compositor applies an empty
            // opaque region and the `place_below` together, with no window in
            // which the parent claims to be opaque over a lowered canvas.
            //
            // SAFETY: `parent_surface` is GTK's toplevel `wl_surface`, live for
            // the process's lifetime. `set_opaque_region`'s signature is "?o" --
            // one nullable object -- and a null region is the protocol's own
            // spelling of "empty".
            unsafe {
                (self.wl.marshal_flags)(
                    self.parent_surface,
                    WL_SURFACE_SET_OPAQUE_REGION,
                    std::ptr::null(),
                    1,
                    0,
                    std::ptr::null_mut::<c_void>(),
                );
            }
        }
        // SAFETY: `self.parent_surface` is GTK's toplevel `wl_surface`, live
        // for the process's lifetime, and `commit` takes no arguments.
        unsafe {
            (self.wl.marshal_flags)(self.parent_surface, WL_SURFACE_COMMIT, std::ptr::null(), 1, 0);
        }
        // The other half of the ordering above: the canvas is back on top and
        // covering, so the background can stop being see-through without a
        // frame of desktop showing through behind it.
        if above {
            self.host.0.set_canvas_see_through(false);
        }
        println!(
            "[android] wayland: engine subsurface placed {} the host window (web-view dialogs open: {})",
            if above { "above" } else { "below" },
            self.open_web_view_dialogs.load(Ordering::SeqCst),
        );
    }

    // **An empty input region on the canvas was tried here, and reverted.**
    //
    // 07564e2 gave the engine's subsurface an empty `wl_surface.set_input_region`
    // while a web-view dialog was up, on the theory that the canvas was eating
    // pointer events aimed at the dialog. What that produced was worse than
    // what it was meant to fix: a click anywhere over the canvas fell through
    // Cordial's window entirely and raised whatever was behind it -- reported
    // as "as soon as we press anything it selects the window behind and focuses
    // it ... my terminal gets focused".
    //
    // That is what an empty input region means. It does not say "give this to
    // the parent", it says "this surface is not present for input", and when
    // nothing else of the window claims that point the compositor gives the
    // click to the next window down. Lowering the canvas below the parent in
    // `set_engine_stacking` is already what routes a dialog's clicks to GTK.
    //
    // **The premise was never measured either.** "The canvas eats the clicks"
    // was inferred from a report and from reading this file. The likelier
    // reading now is that the original "I cant click on the webview's items"
    // was the invisible cursor -- fixed separately in the same commit -- making
    // it impossible to see what was being pointed at.
    //
    // Before reaching for this again, answer the question that was skipped:
    // does a click over a dialog reach GTK at all with the canvas merely
    // lowered? That is one `CORDIAL_TRACE_MOUSE=1` session.
    //
    // Nothing here is the text editor's path. `update_text_overlay` uses
    // `set_engine_stacking` and never touched this, so none of the above
    // reaches typing.

    pub fn geometry(&self) -> (i32, i32, i32) {
        let g = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        (g.width, g.height, g.format)
    }

    /// The `adw::Window` `HostWindowCell` wraps.
    ///
    /// Two callers were both blocked on this and neither invented a second
    /// way to reach the window: `load.rs`'s `wire_refresh_rate` names it in
    /// its own doc comment as the one thing missing before
    /// `gdk::Display::monitor_at_surface` can say which output the window is
    /// actually on, and `cordial_runtime::webview`'s presenter (installed
    /// from `load.rs`, right after `webview::arm`) needs exactly this to
    /// attach an opened web window to. Reading it is safe under the same
    /// rule `HostWindowCell`'s own `unsafe impl Send`/`Sync` already states:
    /// everything that reaches this window runs on the thread that called
    /// `open`, and both callers above only ever do so from inside a
    /// `glib::MainContext` closure that main thread itself runs.
    pub fn window(&self) -> &adw::Window {
        self.host.0.window()
    }

    pub fn connection_fd(&self) -> c_int {
        self.conn_fd
    }

    /// The pointer `eglGetPlatformDisplay`/`eglCreateWindowSurface` need —
    /// see `egl_get_display`/`egl_create_window_surface` in [`overrides`] for
    /// why the engine's own plain `eglGetDisplay(EGL_DEFAULT_DISPLAY)` call
    /// cannot be left to Mesa's own auto-connect.
    pub fn wl_display(&self) -> *mut c_void {
        self.display
    }

    pub fn wl_surface(&self) -> *mut c_void {
        self.surface
    }

    /// The `wl_egl_window*` EGL surfaces are created against, creating it on
    /// first use (the engine may call `eglCreateWindowSurface` more than once
    /// across its lifetime in principle; `wl_egl_window_create` must only run
    /// once per `wl_surface`).
    fn egl_window(&self) -> Option<*mut c_void> {
        let mut slot = self.egl_window.lock().unwrap_or_else(|e| e.into_inner());
        if !slot.is_null() {
            return Some(*slot);
        }
        let egl = self.egl.as_ref()?;
        let (w, h, _) = self.geometry();
        // SAFETY: `self.surface` is a live `wl_surface` proxy on this
        // connection; `wl_egl_window_create` is documented to accept it
        // directly.
        let win = unsafe { (egl.create)(self.surface, w, h) };
        if win.is_null() {
            return None;
        }
        *slot = win;
        Some(win)
    }
}

pub fn current() -> Option<&'static WaylandWindow> {
    WINDOW.get()
}

// ------------------------------------------------------------------ pointer
//
// `wl_fixed_t` is a 24.8 fixed-point number; `/256.0` is the exact inverse of
// how the compositor encoded it, per `wayland-util.h`'s own
// `wl_fixed_to_double`.
fn fixed_to_f32(v: i32) -> f32 {
    v as f32 / 256.0
}

fn pack_pointer_position(x: f32, y: f32) -> u64 {
    ((x.to_bits() as u64) << 32) | y.to_bits() as u64
}

fn unpack_pointer_position(position: u64) -> (f32, f32) {
    (f32::from_bits((position >> 32) as u32), f32::from_bits(position as u32))
}

/// Linux `input-event-codes.h` `BTN_*` values, which is what
/// `wl_pointer.button` reports — X11 numbers buttons 1/2/3 in click order,
/// Wayland reports the evdev code directly. `window.rs`'s equivalent table
/// has the fuller explanation of the primary/secondary/tertiary mismatch this
/// mirrors.
fn linux_button_to_android(button: u32) -> Option<i32> {
    const BTN_LEFT: u32 = 0x110;
    const BTN_RIGHT: u32 = 0x111;
    const BTN_MIDDLE: u32 = 0x112;
    const BTN_SIDE: u32 = 0x113;
    const BTN_EXTRA: u32 = 0x114;
    match button {
        BTN_LEFT => Some(super::input::BUTTON_PRIMARY),
        BTN_RIGHT => Some(super::input::BUTTON_SECONDARY),
        BTN_MIDDLE => Some(super::input::BUTTON_TERTIARY),
        BTN_SIDE => Some(super::input::BUTTON_BACK),
        BTN_EXTRA => Some(super::input::BUTTON_FORWARD),
        _ => None,
    }
}

impl WaylandWindow {
    fn now_ms(&self) -> i64 {
        self.clock.elapsed().as_millis() as i64
    }

    fn set_pointer_position(&self, x: f32, y: f32) {
        self.pointer_pos.store(pack_pointer_position(x, y), Ordering::Release);
    }

    fn pointer_position(&self) -> (f32, f32) {
        unpack_pointer_position(self.pointer_pos.load(Ordering::Acquire))
    }

    fn dispatch_pointer_motion(&self, x: f32, y: f32) {
        self.set_pointer_position(x, y);
        let handle = self.active_handle.load(Ordering::Relaxed);
        let buttons = self.pointer_buttons.load(Ordering::Relaxed);
        let down_time = self.down_time_ms.load(Ordering::Relaxed);
        let now = self.now_ms();
        let action =
            if buttons != 0 { super::input::ACTION_MOVE } else { super::input::ACTION_HOVER_MOVE };
        if handle != 0 {
            super::input::deliver_mouse(handle, action, x, y, buttons, 0, now, down_time);
        }
        super::input::pass_mouse_move(x, y);
    }

    /// Deliver a release for every button this side still thinks is down.
    ///
    /// Called when the pointer leaves the canvas, where no real release will
    /// arrive. Goes through `dispatch_pointer_button` rather than clearing the
    /// mask directly so the engine sees exactly the events it would have seen
    /// had the user released on the canvas -- a bitmask cleared behind the
    /// engine's back leaves the two disagreeing, which is the same bug one
    /// layer down.
    fn release_held_buttons(&self) {
        let held = self.pointer_buttons.load(Ordering::Relaxed);
        if held == 0 {
            return;
        }
        for button in [
            super::input::BUTTON_PRIMARY,
            super::input::BUTTON_SECONDARY,
            super::input::BUTTON_TERTIARY,
            super::input::BUTTON_BACK,
            super::input::BUTTON_FORWARD,
        ] {
            if held & button != 0 {
                if super::input::trace_mouse() {
                    eprintln!("[cordial] pointer left the canvas holding {button}; releasing it");
                }
                self.dispatch_pointer_button(button, false);
            }
        }
    }

    fn dispatch_pointer_button(&self, android_button: i32, press: bool) {
        let (x, y) = self.pointer_position();
        let handle = self.active_handle.load(Ordering::Relaxed);
        let now = self.now_ms();

        if press {
            let before = self.pointer_buttons.fetch_or(android_button, Ordering::Relaxed);
            if before == 0 {
                self.down_time_ms.store(now, Ordering::Relaxed);
            }
            let buttons = self.pointer_buttons.load(Ordering::Relaxed);
            let down_time = self.down_time_ms.load(Ordering::Relaxed);
            if handle != 0 {
                super::input::deliver_mouse(handle, super::input::ACTION_DOWN, x, y, buttons, 0, now, down_time);
                super::input::deliver_mouse(
                    handle, super::input::ACTION_BUTTON_PRESS, x, y, buttons, android_button, now, down_time,
                );
            }
        } else {
            self.pointer_buttons.fetch_and(!android_button, Ordering::Relaxed);
            let buttons = self.pointer_buttons.load(Ordering::Relaxed);
            let down_time = self.down_time_ms.load(Ordering::Relaxed);
            if handle != 0 {
                super::input::deliver_mouse(
                    handle, super::input::ACTION_BUTTON_RELEASE, x, y, buttons, android_button, now, down_time,
                );
                super::input::deliver_mouse(handle, super::input::ACTION_UP, x, y, buttons, 0, now, down_time);
            }
        }

        // Every button, not only the primary one. The gate that used to stand
        // here dropped right and middle before they reached Roblox's own input
        // path, and a right-button drag is how a mouse turns the camera.
        super::input::pass_mouse_button(x, y, press, android_button);

        // Do not wait for the next pump to capture a camera drag. Pointer
        // events are dispatched near the end of `pump`, while the periodic
        // lock synchronisation runs near its beginning; that left up to 50ms
        // in which a fast pointer could cross the canvas edge. The resulting
        // `leave` cleared `POINTER_ON_CANVAS`, so the following pump concluded
        // no drag wanted a lock and the real desktop cursor escaped while the
        // engine's drawn cursor remained centred. At this point the button
        // event still proves pointer focus on the canvas, which is exactly
        // when the compositor can honour `lock_pointer`.
        if android_button == super::input::BUTTON_SECONDARY
            || android_button == super::input::BUTTON_TERTIARY
        {
            self.sync_pointer_lock();
        }
    }

    /// One `wl_pointer.axis` event, converted to detents and handed to the
    /// shared wheel path.
    fn dispatch_pointer_axis(&self, axis: u32, value: f32) {
        let Some((hscroll, vscroll)) = axis_to_notches(axis, value) else {
            return;
        };
        let (x, y) = self.pointer_position();
        let handle = self.active_handle.load(Ordering::Relaxed);
        super::input::wheel(handle, x, y, hscroll, vscroll, self.now_ms());
    }
}

/// `wl_pointer.axis`'s (axis, distance) as `(hscroll, vscroll)` in detents with
/// Android's signs, or `None` for an axis this does not know.
///
/// Wayland's positive is down and to the right; Android's two scroll axes are
/// positive *up* and to the right, so the vertical one is negated and the
/// horizontal one is not. Getting that backwards is the "scrolling goes the
/// wrong way" report, which is why the sign lives in its own tested function
/// rather than inline in an event handler no test can reach.
fn axis_to_notches(axis: u32, value: f32) -> Option<(f32, f32)> {
    const AXIS_VERTICAL_SCROLL: u32 = 0;
    const AXIS_HORIZONTAL_SCROLL: u32 = 1;
    let notches = value / WHEEL_AXIS_STEP;
    match axis {
        AXIS_VERTICAL_SCROLL => Some((0.0, -notches)),
        AXIS_HORIZONTAL_SCROLL => Some((notches, 0.0)),
        _ => None,
    }
}

/// How much `wl_pointer.axis` reports for one detent of a mouse wheel.
///
/// `INFERRED`, and the one number on the Wayland scroll path that is not read
/// off the wire. `wl_pointer.axis` carries a distance in surface coordinates;
/// the events that carry a *count* — `axis_discrete` (version 5) and
/// `axis_value120` (version 8) — never arrive here, because `wl_seat` is bound
/// at version 1 (see the `bind` call) and a child object's version is its
/// parent's. Raising that would make this exact, and would also change what
/// `wl_keyboard` sends, which is a separate change with its own testing.
///
/// 10.0 is what mutter and Weston both use as their axis step for a discrete
/// wheel click. A compositor that disagrees makes every notch scroll by the
/// wrong amount but still in the right direction, and `CORDIAL_WHEEL_SCALE`
/// corrects it without a rebuild.
const WHEEL_AXIS_STEP: f32 = 10.0;

/// Whether the pointer is currently over the engine's canvas rather than over
/// the rest of the window.
///
/// Cordial's `wl_pointer` is a second pointer object on the seat GDK also has
/// one on, so the compositor delivers *every* enter, motion and button to both
/// — including the ones aimed at the header bar and the window controls. The
/// engine used to own the whole toplevel and there was nothing else for a
/// click to mean; now there is. Without this the engine reacts to a click on
/// the close button, and the cursor vanishes over the titlebar because
/// `hide_pointer` fired for it.
static POINTER_ON_CANVAS: AtomicBool = AtomicBool::new(false);

/// Whether a web-view dialog of Cordial's own is in front of the engine.
///
/// **Stacking and input focus are decided separately, and only stacking was
/// being handled.** `webview_dialog_opened` lowers the canvas below the parent
/// surface so a dialog is visible at all -- that half works, which is why the
/// verification window can be seen. But Cordial runs its own `wl_pointer`
/// alongside GDK's and forwards to the engine on `POINTER_ON_CANVAS`, a flag
/// set from `pointer_enter`/`pointer_leave`. A dialog that opens while the
/// pointer is already over the canvas changes what is on top without the
/// pointer having moved, so the flag stays true and every motion and click goes
/// on reaching Roblox underneath.
///
/// Reported twice on 2026-08-28, the second time precisely enough to refute the
/// first explanation: "my cursor never hovers over the webview, its stuck
/// underneath the webview clicking on roblox stuff". A cursor that moves and
/// clicks is not a locked pointer -- a locked pointer has no position to move
/// -- so the pointer-lock gate added earlier that day is not this bug, and this
/// comment exists partly to stop somebody concluding it was.
///
/// **The text overlay is deliberately not included.** `text_overlay_visible` is
/// a small editor widget on one TextBox, not a modal covering the window, and
/// treating it the same way would stop every click anywhere else in the game
/// while somebody is typing. `sync_pointer_lock` does consult both, because
/// giving the cursor back to type is right in a way that swallowing all input
/// is not.
///
/// `INFERRED`: reasoned from this file and the pointer protocol, not observed
/// against a live verification dialog. If clicks still land on the game
/// underneath one, this is the first thing to doubt, and
/// `CORDIAL_TRACE_MOUSE=1` now prints when it engages.
fn dialog_in_front(w: &WaylandWindow) -> bool {
    w.open_web_view_dialogs.load(Ordering::SeqCst) > 0
}

/// Whether *anything* of Cordial's own is drawn over the engine — a web-view
/// dialog or the text editor.
///
/// Broader than [`dialog_in_front`] and deliberately a separate question. That
/// one governs whether pointer *input* reaches the engine, and the editor is
/// excluded from it on purpose: it is a widget on one TextBox, not a modal, and
/// swallowing every click in the game while somebody types would be worse than
/// the bug it fixed. This one governs whether the *cursor is drawn*, where the
/// answer for both is the same — if the thing under the pointer is a GTK
/// widget, the user needs to see where they are pointing.
///
/// `sync_pointer_lock` computed this inline and `pointer_enter` did not consult
/// it at all, which is how the cursor came to be hidden over a dialog.
fn cordial_ui_in_front(w: &WaylandWindow) -> bool {
    dialog_in_front(w) || w.text_overlay_visible.load(Ordering::SeqCst)
}

unsafe extern "C" fn pointer_enter(
    _data: *mut c_void,
    pointer: *mut c_void,
    serial: u32,
    surface: *mut c_void,
    x: i32,
    y: i32,
) {
    let Some(w) = current() else { return };
    let ours = std::ptr::eq(surface, w.surface);
    POINTER_ON_CANVAS.store(ours, Ordering::Release);
    if !ours {
        return;
    }
    // Arriving somewhere is not moving there. `pass_mouse_move` reports how far
    // the pointer travelled, and the distance from wherever it was when it last
    // left the canvas is not a movement the user made.
    super::input::reset_mouse_delta();
    super::input::forget_pending_unlocked_delta();
    // **Hide it, and unconditionally.** Roblox draws its own cursor into the
    // canvas, so the host one is a second cursor half a frame behind it.
    //
    // **This is the only place that can hide it, and a detour through GTK
    // established why.** Setting `none` on the canvas *widget* was tried and
    // does nothing: `host_window::refresh_input_region` punches the canvas out
    // of the parent's input region, so over the canvas the compositor gives
    // pointer focus to this subsurface -- which GDK did not create and knows
    // nothing about. GTK's hit testing never runs there, so its widget cursor
    // is never consulted. Removing this line on that reasoning deleted the
    // only thing that worked, and the report came straight back: "I still see
    // the system cursor".
    //
    // No `cordial_ui_in_front` check, and that is deliberate rather than a
    // simplification. While a web-view dialog is up the parent claims the whole
    // surface (see `webview_dialog_opened`), so pointer focus never reaches
    // this subsurface and this never fires -- the dialog gets an ordinary
    // cursor from GTK because GTK genuinely owns the pointer there. Gating
    // this as well would be a second answer to a question already answered one
    // layer down, and the two could disagree.
    //
    // The text editor is the other case GTK really does own: its rectangle is
    // unioned *back into* the parent's input region, so GDK has focus over it
    // and its widget cursor applies. That is why hovering a focused box used
    // to spawn a pointer, and why the fix for that one is on the widget rather
    // than here.
    w.hide_pointer(pointer, serial);
    // Subsurface coordinates are relative to the subsurface, so these are
    // already canvas-local — no offset for the header bar has to be
    // subtracted anywhere, which is the main practical reason to let the
    // compositor do this rather than translating window coordinates by hand.
    w.dispatch_pointer_motion(fixed_to_f32(x), fixed_to_f32(y));
}
unsafe extern "C" fn pointer_leave(_data: *mut c_void, _pointer: *mut c_void, _serial: u32, _surface: *mut c_void) {
    // **Let go of anything still held, before the canvas flag drops.**
    //
    // Wayland sends no button release on leave, and `pointer_button` below
    // ignores events while the pointer is off the canvas -- so a button held
    // as the pointer leaves is a bit in `pointer_buttons` that nothing ever
    // clears. That bit is one of the two things `sync_pointer_lock` locks the
    // pointer for, and it is gated on being back on the canvas, so the next
    // time the pointer comes back the drag-lock engages with no button down
    // and the camera is captured until something happens to clear it.
    //
    // Reported as shift lock that "sometimes won't undo ... so you're kinda
    // stuck with it, then it works": the "then it works" is the next press and
    // release on the canvas clearing the stale bit. The engine is told too,
    // not just this side's bitmask, because it received the press and would
    // otherwise go on believing the button is down -- which is the other half
    // of a camera that will not let go.
    //
    // Synthesising the release is the correct platform behaviour rather than a
    // workaround: the protocol guarantees no real one is coming, and Android's
    // own answer to the same situation is ACTION_CANCEL.
    if let Some(w) = current() {
        w.release_held_buttons();
    }
    POINTER_ON_CANVAS.store(false, Ordering::Release);
    super::input::reset_mouse_delta();
    super::input::forget_pending_unlocked_delta();
}
unsafe extern "C" fn pointer_motion(_data: *mut c_void, _pointer: *mut c_void, _time: u32, x: i32, y: i32) {
    if !POINTER_ON_CANVAS.load(Ordering::Acquire) {
        return;
    }
    if let Some(w) = current() {
        if dialog_in_front(&w) {
            return;
        }
        w.dispatch_pointer_motion(fixed_to_f32(x), fixed_to_f32(y));
    }
}
unsafe extern "C" fn pointer_button(
    _data: *mut c_void,
    _pointer: *mut c_void,
    _serial: u32,
    _time: u32,
    button: u32,
    state: u32,
) {
    if !POINTER_ON_CANVAS.load(Ordering::Acquire) {
        return;
    }
    let Some(w) = current() else { return };
    if dialog_in_front(&w) {
        // Said once per press rather than per motion event, which would be a
        // line per frame while the pointer moves over the dialog.
        if super::input::trace_mouse() {
            eprintln!(
                "[cordial] click withheld from the engine: a web-view dialog is in front"
            );
        }
        return;
    }
    let Some(android_button) = linux_button_to_android(button) else { return };
    w.dispatch_pointer_button(android_button, state == 1);
}
/// The scroll wheel. Filtered by surface like every other pointer event: the
/// header bar is GTK's, and a scroll over it is not the engine's to see.
unsafe extern "C" fn pointer_axis(_data: *mut c_void, _pointer: *mut c_void, _time: u32, axis: u32, value: i32) {
    if !POINTER_ON_CANVAS.load(Ordering::Acquire) {
        return;
    }
    if let Some(w) = current() {
        if dialog_in_front(&w) {
            return;
        }
        w.dispatch_pointer_axis(axis, fixed_to_f32(value));
    }
}
// `frame`/`axis_source`/`axis_stop`/`axis_discrete`/`axis_value120`/
// `axis_relative_direction` — see `PointerListener`'s own comment for why
// these slots must exist. They stay empty even now that scroll works: none of
// them is delivered to a version 1 `wl_pointer`, so an implementation here
// could never be tested and would be a claim rather than a result.
unsafe extern "C" fn pointer_frame(_data: *mut c_void, _pointer: *mut c_void) {}
unsafe extern "C" fn pointer_axis_source(_data: *mut c_void, _pointer: *mut c_void, _axis_source: u32) {}
unsafe extern "C" fn pointer_axis_stop(_data: *mut c_void, _pointer: *mut c_void, _time: u32, _axis: u32) {}
unsafe extern "C" fn pointer_axis_discrete(_data: *mut c_void, _pointer: *mut c_void, _axis: u32, _discrete: i32) {}
unsafe extern "C" fn pointer_axis_value120(_data: *mut c_void, _pointer: *mut c_void, _axis: u32, _value120: i32) {}
unsafe extern "C" fn pointer_axis_relative_direction(
    _data: *mut c_void,
    _pointer: *mut c_void,
    _axis: u32,
    _direction: u32,
) {
}

static POINTER_LISTENER: PointerListener = PointerListener {
    enter: pointer_enter,
    leave: pointer_leave,
    motion: pointer_motion,
    button: pointer_button,
    axis: pointer_axis,
    frame: pointer_frame,
    axis_source: pointer_axis_source,
    axis_stop: pointer_axis_stop,
    axis_discrete: pointer_axis_discrete,
    axis_value120: pointer_axis_value120,
    axis_relative_direction: pointer_axis_relative_direction,
};

// -------------------------------------------------------------------- touch
//
// `wl_touch`, which had never been bound at all: `grep wl_touch` over this
// tree returned nothing before this, so a touchscreen was not partially
// supported or mismapped, it was invisible. Everything above the wire lives in
// `android::input` -- pointer ids, the packed `ACTION_POINTER_DOWN` index, both
// natives -- and this section is only the part that is genuinely Wayland: which
// surface a contact landed on, and the fixed-point conversion.
//
// **There is deliberately no "is this contact ours" table here.** A contact
// that went down on GTK's toplevel is simply not forwarded, so the tracker in
// `input.rs` has never heard of its id and drops the `motion` and `up` that
// follow on their own -- which is the same guard, in the one place that already
// has to have it, rather than a second copy that can disagree with the first.

unsafe extern "C" fn touch_down(
    _data: *mut c_void,
    _touch: *mut c_void,
    _serial: u32,
    _time: u32,
    surface: *mut c_void,
    id: i32,
    x: i32,
    y: i32,
) {
    let Some(w) = current() else { return };
    // Filtered by surface exactly as `pointer_enter` is, and for the same
    // reason: Cordial's devices sit on the seat GDK also has devices on, so a
    // finger on the header bar or the close button arrives here too.
    if !std::ptr::eq(surface, w.surface) {
        return;
    }
    let (cw, ch, _) = w.geometry();
    super::input::touch_down(
        w.active_handle.load(Ordering::Relaxed),
        id as i64,
        fixed_to_f32(x),
        fixed_to_f32(y),
        (cw, ch),
        w.now_ms(),
    );
}

unsafe extern "C" fn touch_up(_data: *mut c_void, _touch: *mut c_void, _serial: u32, _time: u32, id: i32) {
    let Some(w) = current() else { return };
    let (cw, ch, _) = w.geometry();
    super::input::touch_up(w.active_handle.load(Ordering::Relaxed), id as i64, (cw, ch), w.now_ms());
}

unsafe extern "C" fn touch_motion(
    _data: *mut c_void,
    _touch: *mut c_void,
    _time: u32,
    id: i32,
    x: i32,
    y: i32,
) {
    let Some(w) = current() else { return };
    let (cw, ch, _) = w.geometry();
    super::input::touch_motion(
        w.active_handle.load(Ordering::Relaxed),
        id as i64,
        fixed_to_f32(x),
        fixed_to_f32(y),
        (cw, ch),
        w.now_ms(),
    );
}

/// The end of one atomic group of touch events.
///
/// Empty, and that is a choice rather than an omission. Batching would mean
/// holding every contact change until the frame and sending one `MotionEvent`
/// for the lot, which is closer to what Android's InputReader does -- but the
/// per-contact native this also drives has no batched form that anything here
/// has established (`nativePassInputBatch([I[FIIII)V` exists in the dex and has
/// never been called), so a batching implementation could only be tested on one
/// of the two paths. Sending eagerly is correct on both and one event later at
/// worst.
unsafe extern "C" fn touch_frame(_data: *mut c_void, _touch: *mut c_void) {}

/// The compositor has taken the sequence over -- an edge swipe, a system
/// gesture -- and every contact is void.
unsafe extern "C" fn touch_cancel(_data: *mut c_void, _touch: *mut c_void) {
    let Some(w) = current() else { return };
    let (cw, ch, _) = w.geometry();
    super::input::touch_cancel(w.active_handle.load(Ordering::Relaxed), (cw, ch), w.now_ms());
}

// `shape`/`orientation` are `wl_touch` version 6 and cannot reach an object
// created from a version 1 `wl_seat`. The slots exist because the listener
// array is indexed by wire opcode with no bounds check -- see `TouchListener`
// -- not because there is anything to do with an ellipse: `MotionEvent`'s
// `AXIS_TOUCH_MAJOR`/`_MINOR` are not among the axes AGDK enables, so even a
// delivered shape would have nowhere to go.
unsafe extern "C" fn touch_shape(_data: *mut c_void, _touch: *mut c_void, _id: i32, _major: i32, _minor: i32) {}
unsafe extern "C" fn touch_orientation(_data: *mut c_void, _touch: *mut c_void, _id: i32, _orientation: i32) {}

static TOUCH_LISTENER: TouchListener = TouchListener {
    down: touch_down,
    up: touch_up,
    motion: touch_motion,
    frame: touch_frame,
    cancel: touch_cancel,
    shape: touch_shape,
    orientation: touch_orientation,
};

impl WaylandWindow {
    /// Ask the seat for a `wl_touch` and listen to it, unless there is one
    /// already.
    ///
    /// Called from [`seat_capabilities`] when a touchscreen appears after
    /// startup. The idempotence is the load-bearing part: `capabilities` fires
    /// again for every unrelated change to the seat, and a second `get_touch`
    /// would leave the first proxy listening as well, so every contact would be
    /// delivered to the engine twice -- which is the shape of the bug
    /// `CORDIAL_AGDK_KEY` exists to record, arriving by a different route.
    fn bind_touch(&self) {
        if super::input::no_touch() || !self.touch.load(Ordering::Relaxed).is_null() {
            return;
        }
        // SAFETY: `self.seat` is the live `wl_seat` proxy `open` bound on this
        // connection, and the capability has just been advertised -- which is
        // the precondition `get_touch` has, and getting it wrong disconnects
        // the client rather than returning null.
        let touch = unsafe {
            (self.wl.marshal_flags)(
                self.seat,
                WL_SEAT_GET_TOUCH,
                self.wl.touch_interface,
                1,
                0,
                std::ptr::null_mut::<c_void>(),
            )
        };
        if touch.is_null() {
            eprintln!("[android] wayland: wl_seat.get_touch returned nothing");
            return;
        }
        // SAFETY: `touch` is the proxy just created, and `TOUCH_LISTENER` is a
        // `'static` array of function pointers with one slot per event this
        // interface can carry. See `TouchListener` for why the count matters.
        unsafe {
            (self.wl.add_listener)(
                touch,
                &TOUCH_LISTENER as *const TouchListener as *const c_void,
                std::ptr::null_mut(),
            );
            (self.wl.flush)(self.display);
        }
        self.touch.store(touch, Ordering::Relaxed);
        println!("[android] wayland: a touchscreen appeared on the seat; wl_touch bound");
    }
}

// ----------------------------------------------------------- pointer capture
//
// Two things wanted a captured pointer and neither had one. A right-button
// camera drag ran until the cursor reached the edge of the window and then
// stopped turning, with the cursor left sitting on whatever was outside; first
// person had no capture at all, so looking around meant the pointer walking out
// of the window. Both are the same missing mechanism: `zwp_pointer_constraints_v1`
// to stop the cursor moving, and `zwp_relative_pointer_v1` to keep being told
// how far it tried to.
//
// **The escape path is the part to be careful with.** A lock this file takes
// and does not give back is not a bug in a game client, it is the developer's
// own desktop with a cursor they cannot move, and it has to survive Cordial
// misbehaving rather than only Cordial behaving. There are four ways out and
// they are deliberately independent: the button coming up, the engine saying it
// no longer wants it, Escape (which also latches the lock off until whatever
// asked for it stops asking), and the process ending — the compositor releases
// a constraint when the client that made it goes, so even a kill returns the
// cursor.

/// Whether the compositor has answered a lock request with `locked`.
///
/// Distinct from "Cordial asked for one", which is `locked_pointer` being
/// non-null. Activation is the compositor's decision — the surface has to have
/// pointer focus and the compositor may decline entirely — so treating the
/// request as the fact is how a client ends up feeding a camera relative motion
/// that is never going to arrive.
static POINTER_LOCK_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set when the user presses Escape out of a lock, and cleared only once
/// nothing wants the lock any more.
///
/// Without the latch, Escape releases the pointer and the very next pump sees
/// the engine still asking for a locked centre and takes it straight back — an
/// escape hatch that lasts 50ms is not one.
static POINTER_LOCK_SUPPRESSED: AtomicBool = AtomicBool::new(false);

/// `CORDIAL_NO_POINTER_LOCK=1` — never capture the pointer, whatever the engine
/// or the mouse buttons say.
///
/// The control for every claim made about this path: with it set the cursor
/// leaves the window on a camera drag exactly as it did before any of this
/// existed, in the same session, which is the comparison AGENTS.md asks for.
/// It is also the honest answer to "Cordial has my cursor and I want it to stop
/// doing that" without editing anything.
fn no_pointer_lock() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_NO_POINTER_LOCK").is_some())
}

/// `CORDIAL_NO_DRAG_LOCK=1` — leave the engine's own request as the only thing
/// that captures the pointer.
///
/// The drag lock is Cordial's policy, not the engine's: while a mouse button is
/// held over the canvas the pointer is captured, because that is what a camera
/// drag is on every desktop and it is the reported bug. It is separable from the
/// engine-driven lock so that the two can be told apart in a trace rather than
/// guessed at from one combined behaviour.
fn no_drag_lock() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_NO_DRAG_LOCK").is_some())
}

/// `CORDIAL_FORCE_POINTER_LOCK=1` — ask for the lock unconditionally, with no
/// button held and whatever the engine says.
///
/// This exists because the request cannot otherwise be exercised without a
/// human holding a mouse button, and "the wire format of a request nobody has
/// ever sent" is precisely the thing this file has been bitten by twice. With
/// it set, `lock_pointer` and the `set_cursor_position_hint`/`destroy` pair go
/// on the wire on a schedule and the connection either survives them or does
/// not.
///
/// **Do not set this on a session you are using.** It takes the cursor as soon
/// as the pointer is over the canvas and the only ways back are Escape, moving
/// focus away, or the process ending. It is meant for a nested headless
/// compositor, which has no pointer to take. It announces itself loudly for
/// that reason.
fn force_pointer_lock() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("CORDIAL_FORCE_POINTER_LOCK").is_some();
        if on {
            eprintln!(
                "[android] wayland: CORDIAL_FORCE_POINTER_LOCK=1 — the pointer will be \
                 captured as soon as it is over the canvas, with no button held. Escape \
                 releases it."
            );
        }
        on
    })
}

unsafe extern "C" fn locked_pointer_locked(_data: *mut c_void, _lp: *mut c_void) {
    POINTER_LOCK_ACTIVE.store(true, Ordering::Release);
    if let Some(w) = current() {
        *w.lock_requested_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *w.lock_inactive_since.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    // Arriving at a lock is not a movement. Without this the first relative
    // motion after the cursor stops would be added to whatever absolute delta
    // was outstanding. The pending unlocked-cursor accumulator gets the same
    // treatment for the same reason: an accelerated sample from just before
    // the lock engaged belongs to the cursor, not to the camera the lock is
    // about to hand relative motion to, and it has no absolute report left to
    // be drained by.
    super::input::reset_mouse_delta();
    super::input::forget_pending_unlocked_delta();
    if super::input::trace_mouse() {
        eprintln!("[cordial] pointer lock: compositor sent locked");
    }
}

/// `unlocked` — the compositor took the lock away, which it does on focus loss
/// and on anything else it considers reason enough.
///
/// The object stays alive because the lifetime is `persistent`: the compositor
/// may reactivate it when focus returns, and destroying it here would turn a
/// temporary deactivation into a permanent one. What has to happen is only that
/// relative motion stops being treated as camera movement.
unsafe extern "C" fn locked_pointer_unlocked(_data: *mut c_void, _lp: *mut c_void) {
    POINTER_LOCK_ACTIVE.store(false, Ordering::Release);
    if let Some(w) = current() {
        let mut since = w.lock_inactive_since.lock().unwrap_or_else(|e| e.into_inner());
        if since.is_none() {
            *since = Some(std::time::Instant::now());
        }
    }
    super::input::reset_mouse_delta();
    super::input::forget_pending_unlocked_delta();
    if super::input::trace_mouse() {
        eprintln!("[cordial] pointer lock: compositor sent unlocked");
    }
}

static LOCKED_POINTER_LISTENER: LockedPointerListener =
    LockedPointerListener { locked: locked_pointer_locked, unlocked: locked_pointer_unlocked };

unsafe extern "C" fn relative_pointer_motion(
    _data: *mut c_void,
    _rp: *mut c_void,
    _utime_hi: u32,
    _utime_lo: u32,
    _dx: i32,
    _dy: i32,
    dx_unaccel: i32,
    dy_unaccel: i32,
) {
    // Relative motion arrives whenever the seat's pointer has focus, lock or no
    // lock — this object is bound to the seat's one `wl_pointer`, not to a
    // surface, so `POINTER_ON_CANVAS` is the same focus test `wl_pointer.motion`
    // uses and is checked for the identical reason: nothing else here says
    // whether the movement belongs to Cordial's canvas or another window
    // entirely.
    if !POINTER_ON_CANVAS.load(Ordering::Acquire) {
        return;
    }
    let Some(w) = current() else { return };
    // A dialog of Cordial's own in front of the engine — see `dialog_in_front`
    // and `pointer_motion`'s identical check on `wl_pointer.motion` itself.
    // Without this an accelerated sample made while the user is working the
    // dialog would sit in `PENDING_UNLOCKED_DELTA` (unlocked motion no longer
    // returns early below) until the dialog closed and the next real
    // `wl_pointer.motion` drained it, applying somebody else's movement to the
    // cursor's first report afterwards -- `webview_dialog_opened` now also
    // forgets that accumulator directly, which covers a sample that got there
    // *before* this check could see the dialog; this early-return only stops
    // new ones arriving while it stays open.
    //
    // Placed before the `POINTER_LOCK_ACTIVE` branch below rather than inside
    // it, which reads as though camera motion could be silently dropped while
    // locked with a dialog up. That pairing cannot actually happen, and not by
    // luck: `WaylandWindow::pump` runs `self.host.0.pump()` (which is where
    // `webview_dialog_opened`'s GTK-thread closure would run) and then
    // `sync_pointer_lock()` -- which forces `want = false` and calls
    // `release_pointer`, setting `POINTER_LOCK_ACTIVE` false synchronously,
    // the instant `dialog_in_front`/`cordial_is_in_front` reads true --
    // *before* the `prepare_read`/`read_events`/`dispatch_pending` sequence at
    // `pump`'s own tail that is the only thing able to reach this listener.
    // Both steps happen inside that same `pump()` call, in that order, so any
    // `relative_motion` event dispatched for this pump cycle or a later one
    // sees `dialog_in_front() == true` only after `POINTER_LOCK_ACTIVE` has
    // already gone false for it. Checking the order the other way round would
    // read as more careful and change nothing, since the branch it would be
    // "protecting" cannot be entered either way.
    if dialog_in_front(&w) {
        return;
    }
    // `zwp_relative_pointer_v1.relative_motion` carries two pairs: `dx`/`dy`,
    // which the compositor has already run through the desktop's pointer
    // profile (acceleration, "mouse speed"), and `dx_unaccel`/`dy_unaccel`,
    // which have not.
    if POINTER_LOCK_ACTIVE.load(Ordering::Acquire) {
        // Locked: the camera. This used to send the accelerated pair
        // unconditionally, which is right for moving a UI cursor and wrong for
        // a camera: acceleration is superlinear in speed, so a fast sweep turns
        // the camera disproportionately more than a slow one covering the same
        // real distance, and it makes the in-game sensitivity depend on
        // whatever pointer-speed setting the user's desktop happens to have —
        // neither of which a camera look should do. The unaccelerated pair is
        // the one `libinput`'s own documentation and every engine that
        // supports Wayland pointer lock uses for exactly this reason.
        // `INFERRED` that this is what made the reported camera feel
        // over-sensitive and speed up through a fast turn — not run against
        // the engine, only reasoned from what the two fields are documented to
        // mean — but it is not inferred that Cordial was sending the wrong
        // pair: that part is read straight off the protocol's own field names.
        //
        // Which pair is used is a setting rather than a decision, because the
        // argument above is strong for a first-person camera and not
        // universal: a player who has tuned their desktop pointer profile and
        // wants the client to obey it is not wrong, and neither is one who
        // wants raw input. The default is unaccelerated because that is what a
        // camera wants and what the original reported bug was about;
        // `CORDIAL_POINTER_ACCEL=always` restores the accelerated pair, and
        // the settings window offers it as a switch. This env var governs the
        // camera only. The unlocked cursor below has no equivalent switch --
        // not because an honest "off" is impossible for it, the unaccelerated
        // pair is sitting right there in the same event exactly as it is
        // here, but because nobody has asked for a cursor that ignores the
        // desktop's pointer profile, and the report this fix answers was the
        // opposite complaint. See `PointerAcceleration`'s own doc in
        // `shell_config.rs` for that reasoning in full; a `NeverCursor`
        // variant is a small addition if it is ever needed, not a redesign.
        let (dx, dy) = if pointer_acceleration() { (_dx, _dy) } else { (dx_unaccel, dy_unaccel) };
        w.dispatch_relative_motion(fixed_to_f32(dx), fixed_to_f32(dy));
        return;
    }
    // Unlocked: the cursor over Roblox's own interface, which is what the
    // maintainer's report narrowed this to — "it's set on only the cursor, it
    // should work and accelerate in roblox ui. It doesn't" — and their own
    // fix: use the accelerated pair here too, unconditionally, because an
    // unlocked cursor has no camera-style reason to want raw input.
    //
    // This event does not go to the engine directly. `wl_pointer.motion` is
    // still the only source of the *absolute* position Roblox's own interface
    // needs — the relative-pointer protocol has none — so the two are combined
    // in `pass_mouse_move`: this accumulates the accelerated delta and
    // `dispatch_pointer_motion` drains it when the matching absolute report
    // arrives, in place of the arithmetic difference of two absolute positions
    // it used to compute unconditionally. That split, not a lock check here,
    // is what stops one physical movement being counted twice — the hazard
    // this function used to avoid by never acting on unlocked motion at all.
    //
    // Nothing here assumes `wl_pointer.motion` and this event arrive in a
    // particular order. The relative-pointer extension's text does not specify
    // one, so a compositor is free to write either object's event to the wire
    // first for the same physical sample. `accumulate_unlocked_delta` sums
    // rather than overwrites, so a sample that arrives before its absolute
    // counterpart is simply waiting in `PENDING_UNLOCKED_DELTA` when
    // `pass_mouse_move` looks, and is used exactly once.
    //
    // **This is only harmless, not proven harmless, when a relative sample
    // never arrives after its own physical sample's absolute report has
    // already been drained.** If it does -- sample A's `wl_pointer.motion`
    // dispatches, finds nothing pending, and takes the arithmetic fallback;
    // A's own `relative_motion` dispatches afterwards and accumulates; then
    // sample B's `wl_pointer.motion` arrives and drains *A*'s leftover delta
    // instead of computing its own -- A's movement is sent twice (once as its
    // own fallback, once standing in for B) and B's real movement is never
    // sent at all. That is a genuine double count and drop on that one pair
    // of reports, not the "smear" an earlier version of this comment called
    // it: a smear implies the total displacement across reports stays right
    // and only its timing shifts, which is what happens only if *every*
    // sample in a run has its relative event ordered the same way relative to
    // the absolute one, so each report ends up quietly replaying the previous
    // report's delta rather than corrupting an unrelated pair. Nothing here
    // establishes that a real compositor keeps that ordering consistent
    // rather than varying it per sample -- see `input.rs`'s
    // `a_relative_sample_delivered_after_its_own_absolute_report_corrupts_the_next_one`
    // for the failure demonstrated in isolation, and `docs/NEXT.md`'s
    // "Ordering was checked rather than assumed" section, which carries the
    // same correction and the open question of whether this is ever hit in
    // practice. `INFERRED`, in both places, that it has not been.
    //
    // The remaining case — an absolute report with nothing waiting for it at
    // all — covers a warp, a surface enter, and a compositor with no
    // `zwp_relative_pointer_v1` (in which case this function is never called
    // and `PENDING_UNLOCKED_DELTA` is always empty). `resolve_mouse_delta` in
    // `input.rs` falls back to the arithmetic difference of absolute positions
    // for exactly that case, which is what every unlocked report did before
    // this change existed.
    super::input::accumulate_unlocked_delta(fixed_to_f32(_dx), fixed_to_f32(_dy));
}

/// Whether to pass the compositor's accelerated deltas through to the camera.
///
/// Read once. Consulted only from the locked branch of
/// `relative_pointer_motion` — the unlocked cursor takes the accelerated pair
/// unconditionally and never calls this, see that function's own comment for
/// why there is no equivalent switch for it. A locked pointer still reports
/// at the pointer's full rate, though, so an environment lookup on every one
/// of those would be a syscall-free but still needless cost on the hottest
/// input path there is. This function used to be the only consumer of a
/// `relative_motion` event at all, back when the unlocked case returned
/// before reaching here; it no longer is, which is why "consulted on every
/// relative-motion event" would now be the wrong claim to make here.
fn pointer_acceleration() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // "always" is the only value that turns this on. Anything else,
        // including the variable being absent, leaves the camera on raw
        // movement -- the shell sends "unlocked" for that, which names what
        // happens rather than pretending Cordial can disable acceleration for
        // the unlocked cursor, because it cannot.
        matches!(std::env::var("CORDIAL_POINTER_ACCEL").as_deref(), Ok("always"))
    })
}

static RELATIVE_POINTER_LISTENER: RelativePointerListener =
    RelativePointerListener { relative_motion: relative_pointer_motion };

impl WaylandWindow {
    /// One relative movement while the pointer is captured.
    ///
    /// The absolute position stays where the lock caught it, deliberately. A
    /// locked pointer *has* no absolute position — the compositor is not moving
    /// it — and inventing one that drifts would put the engine's idea of the
    /// cursor somewhere the cursor is not, which is what the cursor position
    /// hint on release then has to undo.
    ///
    /// `INFERRED`, and it is the load-bearing inference of this whole path:
    /// that `nativePassMouseMove`'s last two floats are the delta and that the
    /// camera turns on them rather than on the first two. It is the same
    /// inference `pass_mouse_move` already documents and was already relying on
    /// — real deltas are what made the camera turn at all — so a locked pointer
    /// is not a new assumption, it is the existing one with the absolute pair
    /// held still. If a captured camera turns out not to turn, this is the
    /// first thing to doubt and `CORDIAL_TRACE_MOUSE=1` prints every argument.
    fn dispatch_relative_motion(&self, dx: f32, dy: f32) {
        let (x, y) = self.pointer_position();
        // Deliberately not also `deliver_mouse`. AGDK's touch path carries an
        // absolute position and nothing else; while the pointer is locked that
        // position does not change, so every event would say the finger had not
        // moved. The `NativeInputInterface` path is the one that moves anything
        // here anyway — see `input.rs`'s note on why `nativePassMouseMove`
        // rather than `onTouchEventNative` is what the interface reads.
        super::input::pass_mouse_move_delta(x, y, dx, dy);
    }

    /// Whether a lock Cordial holds has been deactivated long enough to treat
    /// as gone rather than paused.
    ///
    /// One second, matching the refusal timeout beside it, and only while the
    /// pointer is over the canvas: a deactivation with the pointer elsewhere is
    /// the compositor doing its job, and re-requesting into that would be a
    /// destroy and a create per second for as long as the user is in another
    /// window.
    ///
    /// `INFERRED`. The mechanism is read off the protocol -- a `persistent`
    /// lock may be deactivated and reactivated at the compositor's discretion,
    /// and nothing obliges it to reactivate -- and off this file's own state
    /// machine, which has no path out of that combination. It is not measured:
    /// exercising it needs a compositor that deactivates a lock without
    /// restoring it, and the headless harness cannot even give Cordial a
    /// pointer (`SEAT_CAPS` is read once at `open`).
    fn lock_went_dead(&self) -> bool {
        if POINTER_LOCK_ACTIVE.load(Ordering::Acquire) || !POINTER_ON_CANVAS.load(Ordering::Acquire)
        {
            return false;
        }
        self.lock_inactive_since
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|at| at.elapsed() > std::time::Duration::from_secs(1))
    }

    /// Take or release the pointer to match what the engine and the mouse are
    /// currently asking for. Called once per pump.
    fn sync_pointer_lock(&self) {
        // Two independent reasons to capture. The engine's own is the one that
        // covers first person, where nothing about the mouse buttons says a
        // camera is being turned; the drag is Cordial's policy and covers the
        // reported bug directly.
        //
        // The engine is asked *before* the control gate deliberately: with
        // `CORDIAL_NO_POINTER_LOCK=1` this still polls and still traces, so the
        // control run answers "what would it have done" rather than only "it
        // did nothing". A control that also turns off the instrumentation is
        // not a control, it is a second unknown.
        let engine_wants = super::input::engine_wants_pointer_lock() == Some(true);
        if self.pointer_constraints.is_null() || no_pointer_lock() {
            return;
        }

        // The right and middle buttons, and emphatically **not the left one**.
        // A left-button drag is how every draggable thing in Roblox's own
        // interface is used — a slider, a scrollbar, a window in Studio — and
        // capturing the pointer for those would freeze the cursor mid-drag and
        // leave the control chasing a delta it cannot show. Right is the camera
        // drag this was reported for and middle is the pan; both are gestures
        // where the cursor is meant to stay put, which is exactly the
        // distinction Roblox's own desktop client draws.
        const CAMERA_BUTTONS: i32 = super::input::BUTTON_SECONDARY | super::input::BUTTON_TERTIARY;
        let dragging = !no_drag_lock()
            && self.pointer_buttons.load(Ordering::Relaxed) & CAMERA_BUTTONS != 0
            && POINTER_ON_CANVAS.load(Ordering::Acquire);
        let asked = engine_wants || dragging || force_pointer_lock();

        // **A dialog on top of the canvas takes the cursor back, whatever the
        // engine wants.** Reported on 2026-08-28: a verification web view opens
        // in-experience -- the one that gates joining a group -- and "while in a
        // game your cursor will always be in the game not the webview... the
        // cursor breaks and goes under into the game", and once unfocused "the
        // mouse will never go over the webview again".
        //
        // Both halves are this. A locked pointer has no absolute position: the
        // compositor stops moving the cursor and sends relative motion instead,
        // so there is no path by which it can travel onto a dialog. In first
        // person or shift lock the engine goes on answering
        // `nativeGetMainWindowIsMouseLockedCenter` with true regardless of what
        // Cordial has drawn over it -- it cannot see the dialog, and asking it
        // to would be the engine introspection ADR-001 rules out -- so `asked`
        // stays true, and each pump re-locks the moment the pointer re-enters
        // the canvas. That is the "never again" half exactly.
        //
        // The signal already existed and only the stacking used it.
        // `webview_dialog_opened` lowers the canvas below the parent surface so
        // the dialog is visible at all, and `update_text_overlay` raises and
        // lowers it for the editor. Reusing the same two conditions here keeps
        // one answer to "is something of Cordial's own in front of the engine",
        // rather than a second rule that can disagree with the first about
        // which frame it is.
        //
        // `INFERRED` that this is the whole of the reported bug: the reasoning
        // is read off the pointer-constraints protocol and this file, and has
        // not been run against a live verification dialog, which needs an
        // experience that opens one. The check is one session: open a group
        // that asks for verification while in first person and see whether the
        // cursor reaches the dialog.
        let asked = asked && !cordial_ui_in_front(self);

        // The Escape latch lifts only when nothing is asking any more, so
        // pressing Escape in first person gives the cursor back for as long as
        // the engine keeps wanting it — until the user leaves first person,
        // at which point the next request is honoured normally.
        if !asked {
            POINTER_LOCK_SUPPRESSED.store(false, Ordering::Release);
        }
        let want = asked && !POINTER_LOCK_SUPPRESSED.load(Ordering::Acquire);

        let held = !self.locked_pointer.lock().unwrap_or_else(|e| e.into_inner()).is_null();
        if want && !held {
            self.lock_pointer();
        } else if !want && held {
            self.release_pointer();
        } else if want && held && self.lock_went_dead() {
            // **A lock the compositor switched off and never switched back
            // on.** `locked_pointer_unlocked` deliberately keeps the object
            // alive, because a `persistent` lock may be reactivated and
            // destroying it on every deactivation would turn a pause into a
            // permanent loss. The gap that left is this branch: if it is never
            // reactivated, `held` stays true, `want` stays true, and neither
            // arm above fires -- so Cordial believes it holds a lock, the
            // compositor has it switched off, relative motion is discarded,
            // and nothing ever asks again. The camera is dead with no way out.
            //
            // Reported on Discord as shift lock that "sometimes won't undo ...
            // then it works", and separately as behaving "like shift lock if
            // it would do nothing" -- by someone on a tiling WM, which is the
            // kind of compositor that activates and deactivates constraints
            // far more readily than GNOME does.
            //
            // Dropping the object makes the next tick request a fresh one.
            // Gated on the pointer being over the canvas so that being
            // alt-tabbed away -- where deactivation is correct and expected --
            // does not churn a destroy and a create every second.
            println!(
                "[cordial] pointer lock: deactivated and not restored; asking again"
            );
            self.release_pointer();
            *self.lock_inactive_since.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }

        // A compositor may decline, and the protocol gives it no way to say so
        // — there is no error and no reply, only the absence of `locked`. Say
        // that plainly once rather than leaving a dead camera to be explained
        // as a bug in the input path.
        let mut requested = self.lock_requested_at.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = *requested {
            if at.elapsed() > std::time::Duration::from_secs(1) {
                *requested = None;
                if !LOCK_REFUSAL_REPORTED.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "[android] wayland: asked the compositor to lock the pointer and it \
                         never answered with `locked`. The lock is the compositor's decision \
                         and it may decline; the cursor stays free and camera drags will run \
                         off the edge of the window."
                    );
                }
            }
        }
    }

/// Whether to constrain the GTK toplevel rather than the engine's subsurface.
///
/// KWin only, and only because of KDE bug 463088 -- see `lock_pointer`. On
/// every other compositor the subsurface is the surface that actually holds
/// pointer focus over the canvas, and constraining anything else is a lock that
/// is granted and never activates.
fn constrain_toplevel() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        match std::env::var("CORDIAL_POINTER_LOCK_SURFACE").as_deref() {
            Ok("toplevel") => return true,
            Ok("canvas") => return false,
            _ => {}
        }
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase();
        let session = std::env::var("XDG_SESSION_DESKTOP").unwrap_or_default().to_ascii_lowercase();
        desktop.contains("kde") || desktop.contains("plasma")
            || session.contains("kde") || session.contains("plasma")
    })
}

    fn lock_pointer(&self) {
        let mut slot = self.locked_pointer.lock().unwrap_or_else(|e| e.into_inner());
        if !slot.is_null() {
            return;
        }
        // **Which surface to constrain is compositor-dependent, and getting it
        // wrong silently does nothing.**
        //
        // `zwp_pointer_constraints` says a lock becomes *active* when the
        // constrained surface has pointer focus. Over the canvas that is the
        // engine's subsurface, not the toplevel -- so constraining the parent
        // leaves mutter with nothing to activate, and the lock never takes.
        // Reported as "his contribution to cursor lock broke my cursor lock" on
        // GNOME, where the KWin path below was being taken unconditionally.
        //
        // The KWin workaround it came from is real and stays: KWin acknowledges
        // a constraint made against a subsurface and, on affected versions,
        // still lets the physical cursor leave it (KDE bug 463088). Native game
        // windows such as Sober's SDL3 window constrain their xdg_toplevel and
        // do not hit that path.
        //
        // So the two halves of 3d67e59 are separated, because only one of them
        // was ever about the surface. Using **GDK's** pointer rather than
        // Cordial's own is the half that fixed the escape -- Cordial's separate
        // event pointer can be acknowledged as locked while the real cursor,
        // which GDK owns, stays free -- and it applies everywhere. Constraining
        // the **toplevel** is the half that is a KWin bug workaround, and it is
        // now gated to KWin instead of imposed on every compositor.
        //
        // `CORDIAL_POINTER_LOCK_SURFACE=toplevel|canvas` overrules the guess,
        // because a compositor list in a binary goes stale and the person
        // hitting it should not need a rebuild.
        let on_toplevel = Self::constrain_toplevel();
        let target = if on_toplevel { self.parent_surface } else { self.surface };
        //
        // SAFETY: `pointer_constraints` and `parent_surface` are live proxies,
        // and `capture_pointer` is GDK's live borrowed pointer on this
        // connection. The argument list matches `lock_pointer`'s "noo?ou"
        // signature, with a null region meaning the whole surface.
        let lp = unsafe {
            (self.wl.marshal_flags)(
                self.pointer_constraints,
                POINTER_CONSTRAINTS_LOCK_POINTER,
                &LOCKED_POINTER_INTERFACE,
                1,
                0,
                std::ptr::null_mut::<c_void>(),
                target,
                self.capture_pointer,
                std::ptr::null_mut::<c_void>(),
                POINTER_CONSTRAINT_LIFETIME_PERSISTENT,
            )
        };
        if lp.is_null() {
            return;
        }
        // SAFETY: `lp` is the proxy just created, and the listener has one slot
        // per event `LOCKED_POINTER_INTERFACE` declares.
        unsafe {
            (self.wl.add_listener)(
                lp,
                &LOCKED_POINTER_LISTENER as *const LockedPointerListener as *const c_void,
                std::ptr::null_mut(),
            );
            (self.wl.flush)(self.display);
        }
        *slot = lp;
        *self.lock_requested_at.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(std::time::Instant::now());
        if super::input::trace_mouse() {
            let (x, y) = self.pointer_position();
            eprintln!("[cordial] pointer lock: requested at ({x}, {y})");
        }
    }

    /// Give the pointer back, and put the cursor where the engine's cursor was.
    ///
    /// `set_cursor_position_hint` is sent before the destroy and is only
    /// honoured then — the protocol says the hint applies when the lock is
    /// lifted, and a hint sent to a destroyed object is a hint sent to nothing.
    /// Without it the cursor reappears wherever it was when the lock was taken,
    /// which after a long camera drag is not where the person looking at the
    /// screen thinks it is.
    fn release_pointer(&self) {
        let mut slot = self.locked_pointer.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_null() {
            return;
        }
        let (x, y) = self.pointer_position();
        // SAFETY: `*slot` is the live locked-pointer proxy; the two calls match
        // `set_cursor_position_hint`'s "ff" and `destroy`'s empty signature,
        // the latter sent with the destroy flag its `type="destructor"`
        // declaration requires.
        unsafe {
            (self.wl.marshal_flags)(
                *slot,
                LOCKED_POINTER_SET_CURSOR_POSITION_HINT,
                std::ptr::null(),
                1,
                0,
                f32_to_fixed(x),
                f32_to_fixed(y),
            );
            (self.wl.marshal_flags)(
                *slot,
                LOCKED_POINTER_DESTROY,
                std::ptr::null(),
                1,
                WL_MARSHAL_FLAG_DESTROY,
            );
            (self.wl.flush)(self.display);
        }
        *slot = std::ptr::null_mut();
        POINTER_LOCK_ACTIVE.store(false, Ordering::Release);
        *self.lock_requested_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // The cursor is about to be somewhere again, and where it went is not a
        // movement the user made with it. Nothing should be sitting in the
        // unlocked-cursor accumulator at this point -- relative motion while
        // locked goes to `dispatch_relative_motion`, not the accumulator -- but
        // clearing it here too costs nothing and keeps this call site the same
        // shape as the compositor-driven unlock in `locked_pointer_unlocked`.
        super::input::reset_mouse_delta();
        super::input::forget_pending_unlocked_delta();
        if super::input::trace_mouse() {
            eprintln!("[cordial] pointer lock: released, cursor hinted to ({x}, {y})");
        }
    }

    /// The Escape hatch, in both senses. Returns whether a lock was actually
    /// released, so the caller can say so.
    fn escape_pointer_lock(&self) -> bool {
        let held = !self.locked_pointer.lock().unwrap_or_else(|e| e.into_inner()).is_null();
        if !held {
            return false;
        }
        POINTER_LOCK_SUPPRESSED.store(true, Ordering::Release);
        self.release_pointer();
        true
    }
}

/// So a compositor that declines to lock says so once rather than every second.
static LOCK_REFUSAL_REPORTED: AtomicBool = AtomicBool::new(false);

/// `wl_fixed_t` from a float — the inverse of [`fixed_to_f32`], and the only
/// place this file sends one rather than receiving it.
fn f32_to_fixed(v: f32) -> i32 {
    (v * 256.0).round() as i32
}

// ----------------------------------------------------------------- keyboard

const MAP_FAILED: *mut c_void = -1isize as *mut c_void;

unsafe extern "C" fn keyboard_keymap(_data: *mut c_void, _kb: *mut c_void, format: u32, fd: c_int, size: u32) {
    let Some(w) = current() else {
        // SAFETY: the fd is Cordial's own now, per the protocol's fd-passing
        // contract, regardless of whether there is anywhere to put the
        // keymap it describes.
        unsafe { close(fd) };
        return;
    };
    if format != XKB_KEYMAP_FORMAT_TEXT_V1 {
        unsafe { close(fd) };
        return;
    }
    // SAFETY: `fd` was just received via `wl_keyboard.keymap`'s documented fd
    // argument, still open and exclusively Cordial's; `size` is the
    // compositor's own claim about its length, mapped read-only/private per
    // the protocol's stated contract for this event.
    let map = unsafe { mmap(std::ptr::null_mut(), size as usize, 1 /* PROT_READ */, 2 /* MAP_PRIVATE */, fd, 0) };
    if map == MAP_FAILED {
        unsafe { close(fd) };
        return;
    }

    let xkb = match Xkb::load() {
        Ok(x) => x,
        Err(e) => {
            super::trace(format_args!("wayland: {e}"));
            unsafe {
                munmap(map, size as usize);
                close(fd);
            }
            return;
        }
    };
    // SAFETY: `map` points at `size` bytes of the compositor-supplied keymap
    // text, which `wl_keyboard.keymap` documents as NUL-terminated.
    let context = unsafe { (xkb.context_new)(XKB_CONTEXT_NO_FLAGS) };
    let keymap = if context.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe {
            (xkb.keymap_new_from_string)(context, map as *const c_char, XKB_KEYMAP_FORMAT_TEXT_V1, 0)
        }
    };
    unsafe {
        munmap(map, size as usize);
        close(fd);
    }
    if keymap.is_null() {
        super::trace(format_args!("wayland: xkb_keymap_new_from_string failed"));
        if !context.is_null() {
            unsafe { (xkb.context_unref)(context) };
        }
        return;
    }
    let state = unsafe { (xkb.state_new)(keymap) };
    if state.is_null() {
        unsafe {
            (xkb.keymap_unref)(keymap);
            (xkb.context_unref)(context);
        }
        return;
    }

    let mod_index = |name: &CStr| unsafe { (xkb.keymap_mod_get_index)(keymap, name.as_ptr()) };
    let new = XkbState {
        shift_idx: mod_index(c"Shift"),
        ctrl_idx: mod_index(c"Control"),
        alt_idx: mod_index(c"Mod1"),
        caps_idx: mod_index(c"Lock"),
        xkb,
        context,
        keymap,
        state,
    };

    let mut slot = w.xkb.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = slot.take() {
        // A keymap change mid-session (layout switch) — release the old
        // one rather than leaking it.
        unsafe {
            (old.xkb.state_unref)(old.state);
            (old.xkb.keymap_unref)(old.keymap);
            (old.xkb.context_unref)(old.context);
        }
    }
    *slot = Some(new);
}

unsafe extern "C" fn keyboard_enter(
    _data: *mut c_void,
    _kb: *mut c_void,
    _serial: u32,
    surface: *mut c_void,
    _keys: *const WlArray,
) {
    // Keyboard focus lands on the *toplevel*, never on the subsurface — a
    // subsurface has no keyboard focus of its own in the protocol — so the
    // surface named here is GTK's, not the canvas. Checking it anyway rather
    // than accepting any surface, because this client now owns more than one
    // window's worth of surfaces (GTK's dialogs, its cursor surfaces) and
    // "some surface of ours has focus" is not the same claim as "the window
    // the engine is in has focus".
    // Remembered unconditionally, because this can arrive before the window
    // exists to compare against. `wl_keyboard.enter` fires on a focus *change*,
    // so a client whose surface is registered while the compositor already
    // considers it focused gets exactly one of these and may get it early —
    // after which no further enter is ever sent, and a flag that missed it stays
    // false for the life of the process.
    //
    // That is the bug where a scripted launch could not walk: every key was
    // dropped by the gate in `keyboard_key`, permanently, because focus was
    // never observed rather than never held. Launching interactively hid it,
    // since clicking the window produces a fresh enter after the window is up.
    LAST_ENTERED_SURFACE.store(surface as usize, Ordering::Release);
    let Some(w) = current() else { return };
    if std::ptr::eq(surface, w.parent_surface) {
        KEYBOARD_FOCUSED.store(true, Ordering::Release);
    }
}

/// The surface `wl_keyboard.enter` last named, whether or not there was a window
/// to match it against at the time. Reconciled by [`reconcile_keyboard_focus`].
static LAST_ENTERED_SURFACE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Settle a focus notification that arrived before the window did.
///
/// Called from the key path rather than the pump so that it costs nothing until
/// a key actually needs the answer, and so a client that never receives a key
/// never touches it.
fn reconcile_keyboard_focus() {
    if KEYBOARD_FOCUSED.load(Ordering::Acquire) {
        return;
    }
    let entered = LAST_ENTERED_SURFACE.load(Ordering::Acquire);
    if entered == 0 {
        return;
    }
    if let Some(w) = current() {
        if entered == w.parent_surface as usize {
            KEYBOARD_FOCUSED.store(true, Ordering::Release);
        }
    }
}
/// Whether the compositor currently gives this surface keyboard focus.
///
/// `wl_keyboard.leave` was an empty stub, so Cordial kept processing every key
/// the seat delivered even after focus moved to another window — a `Ctrl+C`
/// typed into a terminal appeared in Cordial's own trace. That is a real
/// privacy problem and not merely a correctness one: a game client has no
/// business seeing keystrokes aimed at other applications, whatever it does
/// with them afterwards.
///
/// Wayland is not at fault here; the compositor sends `leave` precisely so a
/// client knows to stop. Cordial simply was not listening.
static KEYBOARD_FOCUSED: AtomicBool = AtomicBool::new(false);

/// Keys the compositor has told us are down and has not yet told us are up.
///
/// Exists so [`keyboard_leave`] can release them. See that function for the bug.
static HELD_KEYS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Whether to leave held keys pressed when the window loses focus.
///
/// **Off by default, because the default has to be the correct one**: a key the
/// compositor will never send a release for is a key the engine believes is
/// held for ever, and that is the WASD bug [`keyboard_leave`] describes.
///
/// It is a switch rather than a hard rule because the stranded-key behaviour is
/// load-bearing for somebody: holding W, alt-tabbing away and leaving a
/// character walking is how people AFK. Deleting that outright would fix a bug
/// by removing a feature, which is a poor trade when the two are the same
/// mechanism seen from different ends.
///
/// Read once and cached, in the same spirit as `branding::current` -- the
/// answer cannot change within a run and a per-keystroke `getenv` on the input
/// path would be a cost paid thousands of times for a value that never moves.
fn hold_keys_unfocused() -> bool {
    static HOLD: OnceLock<bool> = OnceLock::new();
    *HOLD.get_or_init(|| {
        let on = std::env::var_os("CORDIAL_HOLD_KEYS_UNFOCUSED").is_some();
        if on {
            println!(
                "[android] wayland: CORDIAL_HOLD_KEYS_UNFOCUSED is set; keys held when the window \
                 loses focus stay held, so a character keeps walking while you are away. Unset it \
                 if movement keys stop responding after alt-tabbing."
            );
        }
        on
    })
}

unsafe extern "C" fn keyboard_leave(_data: *mut c_void, _kb: *mut c_void, _serial: u32, _surface: *mut c_void) {
    // **Release everything still held, before anything else.**
    //
    // Wayland delivers `leave` and then simply stops sending key events -- the
    // compositor never sends the `release` for a key that was down when focus
    // moved away, because as far as it is concerned that key's story is no
    // longer ours to hear. So alt-tabbing mid-stride left the engine believing
    // W was still down for ever, and this is what a user reported as "alt-tab
    // to another window and only the WASD movement keys break": every other key
    // is pressed and released within one focus, so only the ones you hold while
    // switching away can strand.
    //
    // Reported as happening on Roblox's official client too, which is a reason
    // to fix it rather than to match it.
    //
    // The keys are collected and the lock dropped *before* dispatching, rather
    // than dispatching inside the loop under the lock. That is the same
    // discipline `AudioDevice::close` had to learn the hard way in c7215eb:
    // never hold one subsystem's lock while calling into another, because the
    // callee's threading is not yours to reason about.
    // Drained either way. If the keys are being deliberately left held, this
    // side must still forget them: the next `enter` starts a fresh focus, and
    // carrying the old list across would release, on some later alt-tab, a key
    // the user pressed in a different session entirely.
    let stranded: Vec<u32> = {
        let mut held = HELD_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *held)
    };
    if !stranded.is_empty() && !hold_keys_unfocused() {
        if let Some(w) = current() {
            for key in stranded {
                w.dispatch_key(key, false);
            }
        }
    }
    KEYBOARD_FOCUSED.store(false, Ordering::Release);
    // **And forget the surface `enter` named, or the gate above undoes
    // itself.** `reconcile_keyboard_focus` re-asserts focus from
    // `LAST_ENTERED_SURFACE` whenever `KEYBOARD_FOCUSED` is false — which is
    // exactly the state a `leave` has just produced — so the next thing to
    // call it set the flag straight back to true. `keyboard_key` calls it
    // before checking the gate, so the privacy fix the gate exists for was
    // defeated by its own reconciliation: a key delivered after focus moved
    // away re-armed the gate and was then processed.
    //
    // Found while wiring focus through to the engine: `focused()` reported
    // `Some(true)` on every tick of a run in which the window had been
    // minimised twenty seconds earlier and `GdkToplevelState` had long since
    // dropped `FOCUSED`. An `enter` that has been followed by a `leave` is not
    // an unsettled notification, and there is nothing left to reconcile.
    LAST_ENTERED_SURFACE.store(0, Ordering::Release);
    // A window that does not have focus has no business holding the cursor.
    // The compositor deactivates the lock by itself here — that is what
    // `unlocked` is for — but the *request* would survive, and with a
    // `persistent` lifetime it would be honoured again the moment focus came
    // back, even though by then nobody may be dragging anything. Dropping the
    // request is what makes alt-tab a way out rather than a pause.
    if let Some(w) = current() {
        w.release_pointer();
    }
}

unsafe extern "C" fn keyboard_key(_data: *mut c_void, _kb: *mut c_void, _serial: u32, _time: u32, key: u32, state: u32) {
    // Not ours to see. The seat can still deliver events around a focus
    // change; `KEYBOARD_FOCUSED` is what makes that harmless.
    //
    // Reconciled first, because "focus was never observed" and "focus is not
    // held" are different states and only the second should drop a key.
    reconcile_keyboard_focus();
    if !KEYBOARD_FOCUSED.load(Ordering::Acquire) {
        return;
    }
    if let Some(w) = current() {
        let pressed = state == 1;
        // Tracked so `keyboard_leave` can release whatever is still down. Kept
        // here rather than inferred from the engine's own state because the
        // engine has no interface to ask, and a key we never told it about is
        // not one it can strand.
        {
            let mut held = HELD_KEYS.lock().unwrap_or_else(|e| e.into_inner());
            match (pressed, held.iter().position(|&k| k == key)) {
                // Key repeat re-sends press for a key already down; recording it
                // twice would leave a duplicate to release.
                (true, None) => held.push(key),
                (false, Some(i)) => {
                    held.swap_remove(i);
                }
                _ => {}
            }
        }
        w.dispatch_key(key, pressed);
    }
}

unsafe extern "C" fn keyboard_modifiers(
    _data: *mut c_void,
    _kb: *mut c_void,
    _serial: u32,
    depressed: u32,
    latched: u32,
    locked: u32,
    group: u32,
) {
    let Some(w) = current() else { return };
    let guard = w.xkb.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(xk) = guard.as_ref() {
        // SAFETY: `xk.state` is a live `xkb_state` this same struct owns.
        unsafe { (xk.xkb.state_update_mask)(xk.state, depressed, latched, locked, 0, 0, group) };
    }
}

// `repeat_info` — see `KeyboardListener`'s own comment on `PointerListener`
// for why this slot must exist; key-repeat cadence is not implemented, this
// file relies on whatever repeat the host's own input layer already applies
// before events reach `wl_keyboard.key`.
unsafe extern "C" fn keyboard_repeat_info(_data: *mut c_void, _kb: *mut c_void, _rate: i32, _delay: i32) {}

static KEYBOARD_LISTENER: KeyboardListener = KeyboardListener {
    keymap: keyboard_keymap,
    enter: keyboard_enter,
    leave: keyboard_leave,
    key: keyboard_key,
    modifiers: keyboard_modifiers,
    repeat_info: keyboard_repeat_info,
};

impl WaylandWindow {
    /// A physical key event from `wl_keyboard`. Composition (dead keys, CJK,
    /// autocorrect) never reaches here: a compositor with an active input
    /// method routes the keys it wants to compose with directly to that IME
    /// instead of delivering them to this client's `wl_keyboard` at all —
    /// that routing is the compositor's job, not something this file
    /// arbitrates — so a key that *does* arrive here is, by construction,
    /// one the IME either does not exist or chose not to consume. Treating
    /// it exactly like `window.rs`'s X11 `dispatch_key` — direct
    /// keysym-driven insert/backspace/delete/move — is therefore correct
    /// rather than a fallback that risks double-entry against
    /// `zwp_text_input_v3`'s own `commit_string`.
    // Linux evdev codes for the modifier keys, needed because a modifier's own
    // key event arrives before the compositor's `modifiers` describing it --
    // see `dispatch_key`'s correction.
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_RIGHTSHIFT: u32 = 54;
    const KEY_LEFTALT: u32 = 56;
    const KEY_RIGHTCTRL: u32 = 97;
    const KEY_RIGHTALT: u32 = 100;
    const KEY_F11: u32 = 87;

    fn dispatch_key(&self, evdev_key: u32, down: bool) {
        // `xkb_keycode_t` is evdev's own code offset by 8 — XKB reserves the
        // low 8 for historical X11 reasons every xkbcommon consumer has to
        // replicate.
        let xkbcode = evdev_key + 8;

        let (keysym, unicode, text_len, text_buf, meta) = {
            let guard = self.xkb.lock().unwrap_or_else(|e| e.into_inner());
            let Some(xk) = guard.as_ref() else { return };
            // SAFETY: `xk.state` is live for as long as `guard` is held.
            let keysym = unsafe { (xk.xkb.state_key_get_one_sym)(xk.state, xkbcode) } as std::ffi::c_ulong;
            let mut text_buf = [0u8; 8];
            let n = unsafe {
                (xk.xkb.state_key_get_utf8)(xk.state, xkbcode, text_buf.as_mut_ptr() as *mut c_char, text_buf.len())
            };
            let is_active = |idx: u32| -> bool {
                idx != 0xffff_ffff
                    // SAFETY: as above.
                    && unsafe { (xk.xkb.state_mod_index_is_active)(xk.state, idx, XKB_STATE_MODS_EFFECTIVE) } == 1
            };
            let mut meta = 0;
            if is_active(xk.shift_idx) {
                meta |= super::input::META_SHIFT_ON;
            }
            if is_active(xk.ctrl_idx) {
                meta |= super::input::META_CTRL_ON;
            }
            if is_active(xk.alt_idx) {
                meta |= super::input::META_ALT_ON;
            }
            if is_active(xk.caps_idx) {
                meta |= super::input::META_CAPS_LOCK_ON;
            }
            // **A modifier key's own event carries the mask from before
            // itself, and this corrects that one case.**
            //
            // Wayland sends `key` and then `modifiers`, so when this runs for
            // Shift-down the compositor has not yet told us Shift is down.
            // Measured, a Ctrl+Alt+Shift chord as the engine saw it:
            //
            //     down CTRL  -> 0x0000   ctrl missing
            //     down ALT   -> 0x1000   alt missing
            //     down SHIFT -> 0x1002   shift missing
            //     up   SHIFT -> 0x1003   shift still set after release
            //
            // Every event one behind, in both directions. An ordinary key is
            // unaffected -- press F5 while Shift is held and the mask already
            // has Shift from the earlier `modifiers` -- which is why only
            // modifier keys appeared wrong in the trace.
            //
            // The fix is *not* `xkb_state_update_key`: the note beside
            // `state_update_mask`'s binding is right that re-deriving per
            // keystroke double-applies a toggle the server already accounted
            // for. This adjusts only the bit belonging to the key in hand, and
            // leaves xkb's own state to the compositor as before.
            let self_bit = match evdev_key {
                Self::KEY_LEFTSHIFT | Self::KEY_RIGHTSHIFT => super::input::META_SHIFT_ON,
                Self::KEY_LEFTCTRL | Self::KEY_RIGHTCTRL => super::input::META_CTRL_ON,
                Self::KEY_LEFTALT | Self::KEY_RIGHTALT => super::input::META_ALT_ON,
                _ => 0,
            };
            if self_bit != 0 {
                if down {
                    meta |= self_bit;
                } else {
                    meta &= !self_bit;
                }
            }
            let unicode = if n > 0 { text_buf[0] as i32 } else { 0 };
            (keysym, unicode, n.max(0) as usize, text_buf, meta)
        };

        // Escape gives the cursor back, and the key still reaches the engine.
        //
        // Deliberately not a combination nobody would find. The one thing a
        // person tries when an application has taken their pointer is Escape,
        // and a hatch that has to be looked up is a hatch that is not there
        // when it is needed. Escape already means "leave what I am doing" in
        // Roblox, so it is also the key whose ordinary meaning agrees.
        //
        // `keysym` is not used for this: the lock is a physical-key affair and
        // `KEY_ESC` is 1 in evdev, whatever the layout has made of it.
        const KEY_ESC: u32 = 1;
        if down && evdev_key == KEY_ESC && self.escape_pointer_lock() {
            eprintln!("[android] wayland: Escape released the pointer lock");
        }

        // This window has no GtkApplication accelerator group: the launcher's
        // `win.fullscreen` action can never receive a key pressed while the
        // game toplevel owns focus. Consume both halves so Roblox does not see
        // an unmatched function-key event, and let GTK's state notification
        // persist the choice in game-window.json.
        if evdev_key == Self::KEY_F11 {
            if down {
                self.host.0.set_fullscreen(!self.host.0.window().is_fullscreen());
            }
            return;
        }

        let handle = self.active_handle.load(Ordering::Relaxed);
        let now = self.now_ms();

        if super::input::trace_text() {
            // A length, not the character. See `input::trace_text_contents`.
            eprintln!(
                "[cordial] wayland key {} keysym={keysym:#x} text={} keycode={:?} focus={:?}",
                if down { "down" } else { "up" },
                super::input::redacted(
                    std::str::from_utf8(&text_buf[..text_len]).unwrap_or("")
                ),
                super::input::keysym_to_android(keysym),
                cordial_linker_sys::game_activity::focused_textbox(),
            );
        }

        // **`pass_key_event` is deliberately outside the `if let`.** It takes
        // the evdev code, which is always in hand, and needs nothing from the
        // Android table -- the comment below has said so for as long as the
        // call has existed, while the call itself sat inside a branch that
        // required a successful Android mapping anyway.
        //
        // The cost was the entire function row. `keysym_to_android` had no
        // entry for F1..F12, so every one of them took the `else` and reached
        // the engine through neither path. Reported as "combos work, just not
        // the function row", and two `CORDIAL_ANDROID_TRACE` captures contain
        // no F5 and no F11 at all. The `else` branch did say so on every press
        // -- `wayland: unmapped keysym` -- but it is a trace line, and the
        // greps that were run looked for `passKeyEvent`.
        if let Some(keycode) = super::input::keysym_to_android(keysym) {
            if handle != 0 {
                super::input::deliver_key(handle, down, keycode, evdev_key as i32, meta, 0, unicode, now, now);
            } else {
                // **A mapped key with no handle went nowhere and said nothing.**
                //
                // The `else` below reports an unmapped keysym on every press,
                // but this arm -- mapped fine, no activity handle to deliver
                // to -- was silent, so a key that reached xkb, mapped
                // correctly, and was then dropped looked identical in every
                // log to a key that was never pressed. That is the shape this
                // codebase keeps paying for: not a wrong answer, an absent
                // one.
                //
                // Rate-limited to the first occurrence per key, because a held
                // movement key repeats and one stuck line per frame would bury
                // the thing it is trying to show.
                static COMPLAINED: Mutex<Vec<u32>> = Mutex::new(Vec::new());
                let first = {
                    let mut seen = COMPLAINED.lock().unwrap_or_else(|e| e.into_inner());
                    if seen.contains(&evdev_key) {
                        false
                    } else {
                        seen.push(evdev_key);
                        true
                    }
                };
                if first {
                    eprintln!(
                        "[android] wayland: key {evdev_key} (Android {keycode}) mapped but the \
                         activity handle is 0, so it was dropped before reaching the engine. \
                         Every later press of this key is dropped the same way and will not be \
                         reported again."
                    );
                }
            }
        } else {
            super::trace(format_args!("wayland: unmapped keysym {keysym:#x}"));
        }
        // The evdev code, not the Android keycode: this native speaks the
        // platform's own vocabulary. See `pass_key_event`.
        super::input::pass_key_event(down, evdev_key as i32, meta);

        if !down {
            return;
        }
        let Some(which) = cordial_linker_sys::game_activity::focused_textbox() else { return };

        // `CORDIAL_NO_TEXT_BUFFER=1` sends key events only and never text.
        //
        // Cordial keeping a shadow copy of a field Roblox owns is a design
        // error, not a feature: it is why an empty group cleared the box, why
        // characters land at the end of the string regardless of where the
        // caret actually is, and why the caret position is this side's guess
        // rather than the engine's fact. Editing a text field is the input
        // method's job on Android and the engine's job on desktop; it is not
        // the host shim's job in either case.
        //
        // The open question is whether Roblox's engine edits its own TextBox
        // from `nativePassKeyEvent` alone, as it does on desktop, in which case
        // the buffer can be deleted outright rather than repaired. This switch
        // is how that gets answered by running rather than by argument.
        if std::env::var_os("CORDIAL_NO_TEXT_BUFFER").is_some() {
            return;
        }

        // **The editor widget owns the text, so this path must not also edit
        // it.** A `gtk::Text` is placed on the focused box and given keyboard
        // focus, and GDK delivers these same keystrokes to it through its own
        // `wl_keyboard`. Cordial's is a *second* keyboard object on the same
        // seat -- see the module doc -- so both clients see every key, and
        // inserting here as well would put every character in twice: once by
        // the widget, once by this buffer, and the engine would be told the
        // second.
        //
        // Returning here rather than earlier is deliberate. `pass_key_event`
        // above has already run, which is what suppresses text keys from
        // reaching the game while a box is focused; that behaviour is
        // unchanged. What stops is only this side's editing of a buffer that
        // is no longer the authority -- which is what the comment above wanted
        // and could not have until something else was willing to own it.
        if self.editor_owns_text() {
            return;
        }

        // Ctrl+V, and **below the guard above rather than before it**.
        //
        // See `window.rs` for why there is no engine call behind this and why
        // that is right: Cordial is the editor Android would have over the
        // surface, so a paste is an insert on this path rather than something
        // the engine asks for. `paste_into_engine` reads the host selection
        // through the same broker the copy direction uses.
        //
        // **It used to sit forty lines higher, above every guard, and that was
        // a bug of exactly the kind the guard's own comment describes.** GDK
        // delivers Ctrl+V to the `gtk::Text` too, which pastes natively; this
        // branch returned before `editor_owns_text` was ever consulted, so a
        // paste ran twice — once by the widget and once by this path — while
        // every ordinary character was correctly left to the widget alone.
        // Worse than doubled text: `paste_into_engine` pumps the GLib main
        // context for up to 400ms waiting on an async clipboard read, so the
        // two insertions interleave in an order nothing here controls.
        //
        // Below the guard it is what it was meant to be: the paste
        // implementation for the case where no widget owns the field.
        //
        // Note which cases those actually are, because the obvious guess is
        // wrong. `CORDIAL_NO_TEXT_BUFFER` returns *above* the guard, so that
        // switch now skips this too -- which is right, since it means "send
        // key events only and never text" and a paste is text. What is left
        // here is a Wayland session where the editor widget is not up, plus
        // the X11 path, which has its own copy of this branch in `window.rs`
        // and no editor widget at all (ADR-024).
        if super::input::is_paste_shortcut(keysym, meta) {
            if let Err(e) = super::clipboard::paste_into_engine(handle) {
                super::trace(format_args!("wayland: clipboard paste failed: {e}"));
            }
            return;
        }

        // If an input method is producing text for this session, it owns the
        // text and the keyboard must not also insert it — otherwise every
        // character an engine commits arrives twice. Editing keys still go
        // through: an IME consumes the characters it composes, not the arrows.
        let ime_owns_text = {
            let ime = self.ime.lock().unwrap_or_else(|e| e.into_inner());
            ime.ime_producing
        };

        let typed = std::str::from_utf8(&text_buf[..text_len]).unwrap_or("");
        // Same keysym set as `window.rs`'s X11 path — see its comment for why
        // these six are handled as edits rather than as text, and why an
        // unmapped keysym still falls through to `Edit::Insert` instead of
        // being dropped.
        let edit = match keysym {
            0xff08 => super::input::Edit::Backspace, // XK_BackSpace
            0xffff => super::input::Edit::Delete,    // XK_Delete
            0xff51 => super::input::Edit::Move(super::input::Caret::Left),
            0xff53 => super::input::Edit::Move(super::input::Caret::Right),
            0xff50 => super::input::Edit::Move(super::input::Caret::Home),
            0xff57 => super::input::Edit::Move(super::input::Caret::End),
            _ if ime_owns_text => return,
            _ => super::input::Edit::Insert(typed),
        };
        if let Some((contents, caret)) = super::input::edit_text_buffer(edit) {
            if handle != 0 {
                let _ = cordial_linker_sys::game_activity::text_input(handle, &contents, caret, caret);
            }
            self.send_current_text(which);
            if handle != 0 {
                super::input::deliver_surface_redraw(handle);
            }
        }
    }
}

// -------------------------------------------------------------------- IME
//
// See the module doc's "double-buffered" and "preedit and committed text"
// paragraphs before changing anything below.

/// Splice a composing string into committed text at the caret, and report
/// where the caret should now appear to be. A pure function so its several
/// cases (no preedit; preedit with a mid-string cursor; preedit replacing the
/// prior one entirely) are unit-testable without any Wayland state at all —
/// see the tests at the bottom of this file.
fn splice_preedit(committed: &str, committed_caret_chars: i32, preedit: Option<&(String, i32, i32)>) -> (String, i32) {
    let Some((preedit_text, cursor_begin, _cursor_end)) = preedit else {
        return (committed.to_string(), committed_caret_chars);
    };
    let caret_byte = committed
        .char_indices()
        .nth(committed_caret_chars.max(0) as usize)
        .map(|(i, _)| i)
        .unwrap_or(committed.len());

    let mut spliced = String::with_capacity(committed.len() + preedit_text.len());
    spliced.push_str(&committed[..caret_byte]);
    spliced.push_str(preedit_text);
    spliced.push_str(&committed[caret_byte..]);

    // `cursor_begin` is a byte offset *within the preedit text*, per the
    // protocol; -1 means "the IME expresses no cursor position", which is
    // treated as "at the end of the composing text" — a reasonable default
    // and never worse than pinning it to the start.
    let want = if *cursor_begin < 0 { preedit_text.len() } else { (*cursor_begin as usize).min(preedit_text.len()) };
    let boundary = (0..=want).rev().find(|&i| preedit_text.is_char_boundary(i)).unwrap_or(0);
    let preedit_chars_before_cursor = preedit_text[..boundary].chars().count() as i32;

    (spliced, committed_caret_chars + preedit_chars_before_cursor)
}

impl WaylandWindow {
    /// Forward committed-text-with-preedit-spliced-at-the-caret to the
    /// engine — the one place both the hardware-key path and the IME `done`
    /// path funnel through, so they cannot disagree about how a live preedit
    /// is displayed.
    fn send_current_text(&self, which: i64) {
        let (committed, caret) = super::input::text_buffer_snapshot();
        let preedit = self.ime.lock().unwrap_or_else(|e| e.into_inner()).preedit.clone();
        let (text, caret) = splice_preedit(&committed, caret, preedit.as_ref());
        super::input::pass_text(which, &text, caret);
        // The same resolution `sync_text_overlay` uses, and through the same
        // function so the two cannot drift apart again.
        let generation = cordial_linker_sys::game_activity::textbox_generation();
        let (info, placed) = self.resolve_textbox_geometry(generation);
        self.update_text_overlay(
            generation,
            super::input::text_buffer_revision(),
            &text,
            caret,
            info,
            placed,
        );
    }

    /// Whether the `gtk::Text` placed on the focused box is the authority for
    /// the text, rather than this file's buffer.
    ///
    /// It is, whenever a Roblox TextBox has focus, because that is exactly when
    /// the widget is visible and holding GTK's keyboard focus --
    /// `set_text_overlay` shows it and grabs focus on the same signal this
    /// reads. Deliberately the same condition rather than a second flag: two
    /// notions of "is the editor up" that can disagree is how the overlay and
    /// the input region got out of step before.
    fn editor_owns_text(&self) -> bool {
        cordial_linker_sys::game_activity::focused_textbox().is_some()
    }

    /// Drive `enable()`/`disable()` off the same focus signal `input.rs`
    /// already tracks (`focused_textbox`/`textbox_generation`), rather than
    /// this file inventing a second notion of "which box is focused". Cheap
    /// to call every pump tick: an atomic load and a comparison unless focus
    /// actually changed.
    fn sync_ime_focus(&self) {
        if self.text_input.is_null() {
            return;
        }
        let generation = cordial_linker_sys::game_activity::textbox_generation();
        let (was_enabled, just_focused, just_blurred) = {
            let mut ime = self.ime.lock().unwrap_or_else(|e| e.into_inner());
            if ime.synced_generation == Some(generation) {
                return;
            }
            ime.synced_generation = Some(generation);
            let now_focused = cordial_linker_sys::game_activity::focused_textbox().is_some();
            let was_enabled = ime.enabled;
            ime.enabled = now_focused;
            if now_focused != was_enabled {
                ime.preedit = None;
                ime.pending = PendingImeGroup::default();
            }
            (was_enabled, now_focused && !was_enabled, !now_focused && was_enabled)
        };
        let _ = was_enabled;
        // **GTK's input method speaks for Cordial now, and this one stays
        // quiet.** The module doc above says whoever adds a focusable editable
        // widget to this window has to resolve which of the two
        // `zwp_text_input_v3` objects speaks, because GDK creates its own as
        // soon as such a widget takes focus. The widget is here, it owns the
        // text, and it is the one with the caret rectangle and the surrounding
        // context an input method actually needs -- so it wins, and Cordial's
        // object is never enabled while a box is focused.
        //
        // `disable()` on blur is still sent, and must be: an object left
        // enabled from before this change, or from a path that enabled it, is
        // an object still receiving preedit for a box that no longer exists.
        // Sending `disable` to an already-disabled text input is a no-op.
        //
        // Cordial's object is deliberately not destroyed. Whether two of them
        // on one seat is tolerated by every compositor is the open question
        // this change tests, and keeping the object makes the answer visible
        // rather than pre-empting it. If the widget's IME turns out not to
        // work, re-enabling this is one line.
        if just_focused {
            if !self.editor_owns_text() {
                self.ime_enable();
            }
        } else if just_blurred {
            self.ime_disable();
        }
    }

    fn ime_enable(&self) {
        // SAFETY: `self.text_input` is non-null — checked by the only caller,
        // `sync_ime_focus` — and every signature below matches
        // `TEXT_INPUT_METHODS`'s table exactly.
        unsafe {
            (self.wl.marshal_flags)(self.text_input, TEXT_INPUT_ENABLE, std::ptr::null(), 1, 0);
            // hint=0 (none), purpose=0 (normal) — Roblox's own login form
            // does not expose which of its fields is the password field
            // anywhere this backend can read, so no field is marked
            // password-purpose. The practical cost is a candidate window
            // that may show what was composed for a password field, exactly
            // as it would for any other; there is no channel to do better
            // without engine-side support this file does not have.
            (self.wl.marshal_flags)(self.text_input, TEXT_INPUT_SET_CONTENT_TYPE, std::ptr::null(), 1, 0, 0u32, 0u32);
        }
        self.send_surrounding_text();
        self.send_cursor_rectangle();
        unsafe { (self.wl.marshal_flags)(self.text_input, TEXT_INPUT_COMMIT, std::ptr::null(), 1, 0) };
    }

    fn ime_disable(&self) {
        unsafe {
            (self.wl.marshal_flags)(self.text_input, TEXT_INPUT_DISABLE, std::ptr::null(), 1, 0);
            (self.wl.marshal_flags)(self.text_input, TEXT_INPUT_COMMIT, std::ptr::null(), 1, 0);
        }
    }

    /// Tell the IME what the field currently contains, so a predictive
    /// engine's corrections are made against real context rather than
    /// nothing. Sent once per focus gain rather than after every keystroke —
    /// an IME already knows what it itself just committed or deleted, so
    /// re-announcing state after every `done` this file *caused* would only
    /// add commit/serial churn without new information.
    fn send_surrounding_text(&self) {
        let (text, caret_chars) = super::input::text_buffer_snapshot();
        let caret_byte =
            text.char_indices().nth(caret_chars.max(0) as usize).map(|(i, _)| i).unwrap_or(text.len()) as i32;
        // The protocol caps surrounding text at 4000 bytes; Roblox's login
        // fields are nowhere near that, so no truncation is implemented.
        let Ok(cstr) = CString::new(text) else { return };
        unsafe {
            (self.wl.marshal_flags)(
                self.text_input,
                TEXT_INPUT_SET_SURROUNDING_TEXT,
                std::ptr::null(),
                1,
                0,
                cstr.as_ptr(),
                caret_byte,
                caret_byte,
            );
        }
    }

    /// Best-effort candidate-window placement.
    ///
    /// The reverse `showKeyboard` contract `input.rs` answers hands over a
    /// box's handle and contents, not its on-screen bounds (see
    /// `docs/NEXT.md` §1) — there is no engine API this backend can reach
    /// that reports where a text field is drawn. The last pointer position is
    /// used instead: it is where the user just clicked to focus the field,
    /// which is inside or very close to it in practice. That is a stand-in
    /// for real field geometry, not a claim of pixel accuracy.
    ///
    /// The offset is not decoration. `set_cursor_rectangle` is in the
    /// coordinate space of the surface the text input is *entered* on, and
    /// that is GTK's toplevel — a subsurface never takes keyboard focus, so it
    /// is never the entered surface. The pointer position this reads is
    /// canvas-local. Sending it unadjusted would put the candidate window a
    /// header bar and a drop shadow away from where the user is typing.
    fn send_cursor_rectangle(&self) {
        let (x, y) = self.pointer_position();
        let (ox, oy) = *self.placed_at.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            (self.wl.marshal_flags)(
                self.text_input,
                TEXT_INPUT_SET_CURSOR_RECTANGLE,
                std::ptr::null(),
                1,
                0,
                x as i32 + ox,
                y as i32 + oy,
                100i32,
                24i32,
            );
        }
    }

    /// Apply one accumulated `zwp_text_input_v3` double-buffer group. Only
    /// `done` calls this — every other text-input event above only records
    /// into `ImeState::pending`, never touches the committed buffer or the
    /// composing string directly. See the module doc.
    fn apply_ime_done(&self) {
        let Some(which) = cordial_linker_sys::game_activity::focused_textbox() else {
            // Nothing focused to apply this to — a group that arrived after a
            // blur raced `disable()`. Drop it rather than editing a buffer
            // whose focus generation has already moved on.
            let mut ime = self.ime.lock().unwrap_or_else(|e| e.into_inner());
            ime.pending = PendingImeGroup::default();
            return;
        };

        let (delete, commit, preedit_update) = {
            let mut ime = self.ime.lock().unwrap_or_else(|e| e.into_inner());
            let g = std::mem::take(&mut ime.pending);
            (g.delete, g.commit, g.preedit)
        };

        // A `done` carrying nothing is an acknowledgement, not an edit. The
        // compositor sends one in reply to `enable()`, and with no input method
        // configured it sends *only* these — the trace of a working client
        // shows `done(2)` through `done(13)` with no commit_string between
        // them.
        //
        // Treating that as an empty edit is destructive, because what gets
        // pushed to the engine is this side's whole idea of the field's
        // contents, which at focus time is nothing:
        //
        //     textbox focused handle=139983126597760 current=0 bytes
        //     text -> "" caret=0                  <- the field is cleared here
        //     textbox blurred                     <- and the engine drops focus
        //
        // Every keystroke after that logged `focus=None`, because there was no
        // longer a focused box to type into. So: an empty group changes
        // nothing and must not reach the engine at all.
        if delete.is_none() && commit.is_none() && preedit_update.is_none() {
            return;
        }

        // Applied in protocol order: delete relative to the cursor as it
        // stood before this group, then the commit is inserted at the
        // (now-current) cursor, then the new preedit — which may be "no
        // preedit" if the IME sent a null/empty one, a real event per the
        // module doc — replaces whatever was composing before.
        if let Some((before, after)) = delete {
            let _ = super::input::edit_text_buffer(super::input::Edit::DeleteSurrounding {
                before_bytes: before as usize,
                after_bytes: after as usize,
            });
        }
        if let Some(text) = commit {
            let text = text.unwrap_or_default();
            if !text.is_empty() {
                let _ = super::input::edit_text_buffer(super::input::Edit::Insert(&text));
            }
        }
        if let Some(new_preedit) = preedit_update {
            let mut ime = self.ime.lock().unwrap_or_else(|e| e.into_inner());
            ime.preedit = new_preedit;
        }

        let handle = self.active_handle.load(Ordering::Relaxed);
        if handle != 0 {
            let (committed, caret) = super::input::text_buffer_snapshot();
            let _ = cordial_linker_sys::game_activity::text_input(handle, &committed, caret, caret);
        }
        self.send_current_text(which);
        if handle != 0 {
            super::input::deliver_surface_redraw(handle);
        }
    }
}

unsafe extern "C" fn ti_enter(_data: *mut c_void, _ti: *mut c_void, _surface: *mut c_void) {}
unsafe extern "C" fn ti_leave(_data: *mut c_void, _ti: *mut c_void, _surface: *mut c_void) {
    // Focus left this surface: whatever the input method was doing no longer
    // applies, so the keyboard path takes the text back until an input method
    // speaks again.
    if let Some(w) = current() {
        let mut ime = w.ime.lock().unwrap_or_else(|e| e.into_inner());
        ime.ime_producing = false;
        ime.preedit = None;
    }
}

unsafe extern "C" fn ti_preedit_string(
    _data: *mut c_void,
    _ti: *mut c_void,
    text: *const c_char,
    cursor_begin: i32,
    cursor_end: i32,
) {
    let Some(w) = current() else { return };
    // SAFETY: `text` is `zwp_text_input_v3.preedit_string`'s documented
    // nullable, NUL-terminated argument.
    let text = (!text.is_null()).then(|| unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned());
    let mut ime = w.ime.lock().unwrap_or_else(|e| e.into_inner());
    // An input method has spoken for this session, so the keyboard path must
    // stop inserting text — see `ImeState::ime_producing`.
    ime.ime_producing = true;
    // A new preedit_string replaces the previous one entirely — this
    // assignment, not an append, is that rule.
    ime.pending.preedit = Some(text.map(|t| (t, cursor_begin, cursor_end)));
}

unsafe extern "C" fn ti_commit_string(_data: *mut c_void, _ti: *mut c_void, text: *const c_char) {
    let Some(w) = current() else { return };
    // SAFETY: as `ti_preedit_string`.
    let text = (!text.is_null()).then(|| unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned());
    let mut ime = w.ime.lock().unwrap_or_else(|e| e.into_inner());
    ime.pending.commit = Some(text);
}

unsafe extern "C" fn ti_delete_surrounding_text(_data: *mut c_void, _ti: *mut c_void, before: u32, after: u32) {
    let Some(w) = current() else { return };
    let mut ime = w.ime.lock().unwrap_or_else(|e| e.into_inner());
    ime.pending.delete = Some((before, after));
}

unsafe extern "C" fn ti_done(_data: *mut c_void, _ti: *mut c_void, _serial: u32) {
    if let Some(w) = current() {
        w.apply_ime_done();
    }
}

/// `action` (version 2): the input method reports that the user activated the
/// field — the on-screen keyboard's Go/Search key. Nothing is done with it
/// because Cordial never sends `set_available_actions`, so the protocol says
/// no action is available and this cannot legitimately arrive; the slot exists
/// so that a compositor which sends it anyway is ignored rather than fatal.
/// If Roblox's `returnKeyType` is ever wired through (docs/NEXT.md §1), this is
/// where Enter-from-the-IME belongs.
unsafe extern "C" fn ti_action(_data: *mut c_void, _ti: *mut c_void, _action: u32, _serial: u32) {}
/// `language` (version 2): a BCP 47 tag for whatever the input method is
/// currently composing in, sent on creation and on every change. Roblox has no
/// call that takes it, so it is accepted and dropped.
unsafe extern "C" fn ti_language(_data: *mut c_void, _ti: *mut c_void, _language: *const c_char) {}
/// `preedit_hint` (version 2): how a range of the composing string should be
/// styled — underline, selection, spelling error. Cordial does not draw the
/// preedit itself (it splices it into the string the engine renders, see
/// `splice_preedit`), so there is nothing here to style. **This is event 8**,
/// the one whose absence produced the freeze recorded in the module doc.
unsafe extern "C" fn ti_preedit_hint(_data: *mut c_void, _ti: *mut c_void, _start: u32, _end: u32, _hint: u32) {}

static TEXT_INPUT_LISTENER: TextInputListener = TextInputListener {
    enter: ti_enter,
    leave: ti_leave,
    preedit_string: ti_preedit_string,
    commit_string: ti_commit_string,
    delete_surrounding_text: ti_delete_surrounding_text,
    done: ti_done,
    action: ti_action,
    language: ti_language,
    preedit_hint: ti_preedit_hint,
};

impl WaylandWindow {
    /// Hide the host cursor for as long as the pointer is on the canvas.
    ///
    /// `wl_pointer.set_cursor` with a null surface is the protocol's "draw no
    /// cursor", and it is scoped to the enter serial it is sent with -- which
    /// is why this lives in `pointer_enter` and not somewhere it could be
    /// called at leisure.
    ///
    /// `CORDIAL_SHOW_CURSOR=1` restores it, for debugging input.
    fn hide_pointer(&self, pointer: *mut c_void, serial: u32) {
        if pointer.is_null() || std::env::var_os("CORDIAL_SHOW_CURSOR").is_some() {
            return;
        }
        // SAFETY: `pointer` is the live `wl_pointer` this event arrived on, and
        // the argument list matches `set_cursor`'s `uoii` signature.
        unsafe {
            (self.wl.marshal_flags)(
                pointer,
                WL_POINTER_SET_CURSOR,
                std::ptr::null(),
                1,
                0,
                serial,
                std::ptr::null_mut::<c_void>(),
                0i32,
                0i32,
            );
        }
    }
}

// -------------------------------------------------------------------- pump
//
// Mirrors `window.rs`'s X11 pump: must never block, since it runs inside
// `looper::pump`'s bounded timeout loop on the thread that also owns the
// engine's message pump.

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
const POLLIN: i16 = 0x001;

// Same `*mut c_void` signature reasoning as `window.rs`'s own `poll` extern:
// two `extern "C" fn poll` declarations with different signatures anywhere in
// the crate trip `clashing_extern_declarations`, since both ultimately bind
// the one process-wide C symbol bionic's emulated libc also declares.
extern "C" {
    fn poll(fds: *mut c_void, nfds: u64, timeout_ms: c_int) -> c_int;
}

/// Whether the last `sync_canvas_geometry` had no content rectangle to work
/// with — see the message it prints for why that state has its own line.
static NO_CONTENT_RECT: AtomicBool = AtomicBool::new(false);

/// Whether the canvas subsurface has ever actually been placed and committed.
///
/// A static rather than a field for the same reason `NO_CONTENT_RECT` above is
/// one: `WINDOW` is a `OnceLock`, so there is exactly one of these per process
/// and a second copy on the struct would be a second thing to keep in step.
/// See `sync_canvas_geometry`'s use of it for the white window this exists to
/// prevent.
static EVER_PLACED: AtomicBool = AtomicBool::new(false);

// TEMPORARY INSTRUMENTATION -- not for commit. See the session notes.
static INSTR_SET_POSITIONS: AtomicI64 = AtomicI64::new(0);
static INSTR_QUEUE_COMMITS: AtomicI64 = AtomicI64::new(0);

pub fn instr_geometry() -> String {
    let Some(w) = current() else { return "no-wayland-window".into() };
    format!(
        "rect={:?} placed={:?} setpos={} qcommit={}",
        w.host.0.content_rect(),
        *w.placed_at.lock().unwrap_or_else(|e| e.into_inner()),
        INSTR_SET_POSITIONS.load(Ordering::Relaxed),
        INSTR_QUEUE_COMMITS.load(Ordering::Relaxed),
    )
}

/// Close the window from inside the process, for `looper::pump`'s scripted
/// timeline — the only way to test the close-to-exit path without a human
/// clicking, and without going anywhere near the developer's session.
///
/// `gtk_window_close` is a faithful stand-in for the close button rather than a
/// shortcut past it: the compositor's `xdg_toplevel.close` reaches GTK as a
/// `GDK_DELETE` event, GTK turns that into the window's `close-request` signal,
/// and `gtk_window_close` emits *that same signal*. Both end in the default
/// handler destroying the window, which is what `window_closed` observes. What
/// it does not cover is the compositor's half — that the close event arrives at
/// GTK at all — which is GTK's own well-travelled code and not something
/// Cordial could break.
pub fn instr_close_window() {
    use gtk4::prelude::GtkWindowExt;
    if let Some(w) = current() {
        println!("[instr] closing the window");
        w.host.0.window().close();
    }
}

/// Drive fullscreen without a click, from `looper::pump`'s scripted timeline.
/// `gtk_window_fullscreen` is a request to the compositor made by this client
/// about its own window, so it exercises the same configure path a dragged
/// edge does without going anywhere near the developer's session.
pub fn instr_set_fullscreen(on: bool) {
    if let Some(w) = current() {
        w.host.0.set_fullscreen(on);
    }
}

impl WaylandWindow {
    fn pump(&self, handle: i64) {
        self.active_handle.store(handle, Ordering::Relaxed);
        if super::input::keyboard_report_enabled() {
            let (gw, gh, _) = self.geometry();
            super::input::report_keyboard_state((gw, gh));
        }

        // GTK's main loop does not get a thread of its own. It is iterated
        // here, on the thread that ran `gtk_init`, from inside the engine's
        // own message pump — which is what makes the header bar's buttons,
        // hover states and the compositor's resize handshake work at all,
        // since nothing else in this process ever runs a `GMainLoop`.
        //
        // The cost is that GTK's responsiveness is bounded by how often
        // `looper::pump` comes round, currently every 50ms or immediately on
        // any traffic on the display connection — and the display connection
        // is exactly where a click on the header bar arrives, so the idle case
        // is the only one that waits.
        self.host.0.pump();
        self.sync_canvas_geometry();
        self.sync_ime_focus();
        self.sync_text_overlay();
        // Polled rather than driven by an event, because the engine's own
        // request for a locked centre is a *getter* — `nativeGetMainWindow
        // IsMouseLockedCenter` — with nothing that calls out when it changes.
        // Once per pump is roughly 20 times a second, which is a JNI call and
        // an atomic load against a decision a person makes a few times a
        // minute.
        self.sync_pointer_lock();

        // The documented thread-safe idiom for a `wl_display` connection that
        // more than one thread touches — and more than one does: Mesa's own
        // Wayland EGL winsys reads and writes this exact connection from
        // whichever thread calls `eglSwapBuffers`/creates buffers, since
        // `egl_get_display` (below) hands it *this* display rather than
        // letting it open a second, unrelated one. `prepare_read` reserves
        // the right to be the next reader; if something else already holds
        // that reservation, back off to dispatching whatever is already
        // queued rather than contending for the socket.
        //
        // GDK is now a third party to the same connection — it owns it — and
        // uses this same idiom from its own `GSource`. That is why this stays
        // exactly as it was rather than being replaced by "let GTK do the
        // reading": the reservation is what makes two readers safe, and
        // `self.host.0.pump()` above having just run means the usual outcome
        // here is a `prepare_read` that succeeds with nothing left to read.
        //
        // SAFETY: `self.display` is live for the process's lifetime.
        if unsafe { (self.wl.prepare_read)(self.display) } != 0 {
            unsafe { (self.wl.dispatch_pending)(self.display) };
            return;
        }
        unsafe { (self.wl.flush)(self.display) };

        let mut pfd = PollFd { fd: self.conn_fd, events: POLLIN, revents: 0 };
        // SAFETY: `pfd` is a live value for the call; a 0ms timeout makes
        // this a pure non-blocking check, exactly as in `window.rs`.
        let ready = unsafe { poll(&mut pfd as *mut PollFd as *mut c_void, 1, 0) };
        if ready > 0 {
            // SAFETY: `prepare_read` above succeeded, so this is the read it
            // reserved.
            unsafe { (self.wl.read_events)(self.display) };
        } else {
            // SAFETY: as above — cancels the reservation instead of using it.
            unsafe { (self.wl.cancel_read)(self.display) };
        }
        unsafe { (self.wl.dispatch_pending)(self.display) };
        self.report_display_error();
    }

    /// Say what killed the connection, once, in Cordial's own words.
    ///
    /// A session was lost to nothing but this, on a signed-in home page:
    ///
    /// ```text
    /// Gdk-Message: 14:10:43.968: Error 71 (Protocol error) dispatching to Wayland display.
    /// ```
    ///
    /// GDK prints that from `_gdk_wayland_display_queue_events` and then calls
    /// `_exit(1)`, so it is the last line there is. It names an errno and
    /// nothing else — not the interface, not the object, not the reason —
    /// and 71 is `EPROTO`, which means the *compositor* rejected something
    /// this client sent.
    ///
    /// The description of *what* it rejected is recovered elsewhere, by
    /// [`cordial_shell::host_window`]'s GDK-domain log handler; read that
    /// function's comment first, because it is the one that actually works.
    /// This is the second net, and a poor one: whichever side pulls the error
    /// off the socket dispatches it, and when GDK does, it exits before this
    /// ever runs. Measured with a deliberate bad `bind`, GDK won 3 times out of
    /// 3 — so **the absence of this line means nothing**. What it adds when it
    /// does win is the interface and protocol error code in Cordial's own
    /// voice, and coverage of the non-`EPROTO` case, where libwayland itself
    /// gave up on an event and GDK's line would report a meaningless errno.
    ///
    /// Deliberately not a panic. The display is already unusable and every
    /// later request is discarded, so there is nothing to salvage; the point is
    /// only that the next person gets the object and the code.
    fn report_display_error(&self) {
        // SAFETY: `self.display` is live for the process's lifetime, and both
        // calls are pure reads of state libwayland already recorded.
        let err = unsafe { (self.wl.get_error)(self.display) };
        if err == 0 {
            return;
        }
        if DISPLAY_ERROR_REPORTED.swap(true, Ordering::Relaxed) {
            return;
        }
        let mut interface: *const WlInterface = std::ptr::null();
        let mut id: u32 = 0;
        let code = unsafe { (self.wl.get_protocol_error)(self.display, &mut interface, &mut id) };
        // 71 is EPROTO: the compositor sent `wl_display.error`, and then
        // `interface`/`id`/`code` are populated. Any other errno means
        // libwayland itself gave up — a malformed or unknown event, most
        // likely an opcode past the end of one of the hand-written tables at
        // the top of this file — and they are not.
        let name = if interface.is_null() {
            "(none)".to_string()
        } else {
            // SAFETY: a `wl_interface` libwayland owns; `name` is a static C
            // string in whichever table declared it.
            unsafe { CStr::from_ptr((*interface).name) }.to_string_lossy().into_owned()
        };
        eprintln!(
            "[android] wayland: the display connection is dead (errno {err}); \
             compositor error on {name}#{id}, code {code}. \
             The compositor's own description of it is on stderr just above this line."
        );
    }
}

/// So that a dead connection is described once rather than on every pump tick
/// for however many ticks happen before the process goes.
static DISPLAY_ERROR_REPORTED: AtomicBool = AtomicBool::new(false);

pub fn pump_input_events(handle: i64) {
    if let Some(w) = current() {
        w.pump(handle);
    }
}

/// Whether the compositor currently gives this window keyboard focus, for the
/// pump to hand on to the engine.
///
/// The engine was never told. `onWindowFocusChangedNative` reached it exactly
/// twice in a session — `true` inline in `cordial_game_activity_start`, and
/// `false` in `looper::teardown` — so for the whole of the run in between,
/// Roblox believed it was the focused window whatever the user was actually
/// looking at, and kept rendering and simulating at full rate behind whatever
/// they had switched to. That is the "takes away your resources from your
/// other programs when you are unfocused" report.
///
/// **`GdkToplevelState::FOCUSED`, not this file's own `KEYBOARD_FOCUSED`.**
/// The first attempt used the latter and it reported `Some(true)` on every
/// tick of a run in which the window had been minimised twenty seconds
/// earlier: `reconcile_keyboard_focus` was re-asserting focus from a stale
/// `LAST_ENTERED_SURFACE`, which is a bug in its own right and is fixed at
/// [`keyboard_leave`]. Even with that fixed the toplevel's state is the better
/// source — it is what `xdg_toplevel.configure` said, which is what Android's
/// `hasWindowFocus` corresponds to, and it comes from the same place as
/// [`visible`] rather than from a second seat object tracking a related but
/// different question. Measured against the `minimise` script action it drops
/// `FOCUSED` within one pump tick, where `SUSPENDED` takes about four seconds.
///
/// `None` until GTK has a toplevel — see [`visible`] for why that is not
/// `Some(false)`.
pub fn focused() -> Option<bool> {
    current().and_then(|w| w.host.0.focused())
}

/// Whether the compositor still considers this window visible, for the pump's
/// throttle policy.
///
/// `None` is "not known yet" and must not be read as "not visible" — see
/// [`cordial_shell::host_window::HostWindow::visible`], which carries the
/// protocol detail and the note about what was measured rather than assumed.
pub fn visible() -> Option<bool> {
    current().and_then(|w| w.host.0.visible())
}

/// The toplevel's whole state as a string, for the run that established what
/// [`visible`] can see. `CORDIAL_INSTR=1` prints it beside the geometry.
pub fn instr_toplevel_state() -> String {
    match current().and_then(|w| w.host.0.toplevel_state()) {
        Some(s) => format!("{s:?}"),
        None => "no-toplevel".to_string(),
    }
}

/// Minimise or restore from a scripted run. See
/// [`cordial_shell::host_window::HostWindow::set_minimised`] for why this is
/// the only honest way to exercise the visibility path here.
pub fn instr_set_minimised(on: bool) {
    if let Some(w) = current() {
        w.host.0.set_minimised(on);
    }
}

/// Whether the window has been closed.
///
/// **This is what makes closing the window end the process**, and until it
/// existed nothing did. `cordial-run` had no run-until-the-window-closes mode
/// at all: `--run` was a hard timer, the shell passed it a day, and closing the
/// window left the client running headless for the rest of that day — holding
/// the profile's `flock` the whole time, so the launcher refused to start
/// anything and the only way out was `kill`. It was observed in the wild, on a
/// client reparented to systemd after its launcher quit, holding the
/// developer's real profile for half an hour.
///
/// The signal is GTK's `wl_surface` going away. GTK owns the `xdg_toplevel`
/// (see the module doc), so the compositor's `close` event is delivered to GTK,
/// not here; GTK's default handler destroys the window, which unrealizes it and
/// drops its `GdkSurface`. `HostWindow::wl_surface` reads that surface, so
/// `None` is "GTK no longer has a window", which is a stronger and simpler
/// statement than trying to intercept a protocol event that is not this file's
/// to receive.
///
/// Deliberately not a minimise or a hide check: GTK4 keeps a minimised window
/// realized, so this cannot fire for one. A false positive here exits a session
/// somebody was using, which is worse than the bug being fixed, so the
/// condition is the narrow one.
pub fn window_closed() -> bool {
    // Once true, always true. `wl_surface()` is read through GTK, and after
    // teardown has begun there is nothing to be gained from asking it again.
    if WINDOW_CLOSED.load(Ordering::Acquire) {
        return true;
    }
    let Some(w) = current() else { return false };
    if w.host.0.wl_surface().is_none() {
        WINDOW_CLOSED.store(true, Ordering::Release);
        // Say it plainly. A client that ends because its window closed and one
        // that ends because a timer expired look identical from outside, and
        // the difference is the whole of what was just fixed.
        println!("[android] wayland: the window was closed; shutting the engine down");
        return true;
    }
    false
}

static WINDOW_CLOSED: AtomicBool = AtomicBool::new(false);

// ------------------------------------------------------------- ANativeWindow_*
//
// Identical shape to `window.rs`'s X11 implementation — see its comments for
// the reasoning behind each of these; nothing here differs except which
// backend's singleton is read.

fn handle_ptr() -> *mut c_void {
    WINDOW.get().map_or(std::ptr::null_mut(), |w| w as *const WaylandWindow as *mut c_void)
}

fn as_window(p: *mut c_void) -> Option<&'static WaylandWindow> {
    (!p.is_null()).then(|| WINDOW.get()).flatten()
}

extern "C" fn native_window_from_surface(_env: *mut c_void, _surface: *mut c_void) -> *mut c_void {
    let w = handle_ptr();
    super::trace(format_args!("wayland: ANativeWindow_fromSurface -> {w:?}"));
    w
}
extern "C" fn native_window_acquire(_window: *mut c_void) {}
extern "C" fn native_window_release(_window: *mut c_void) {}
extern "C" fn native_window_get_width(window: *mut c_void) -> i32 {
    as_window(window).map_or(0, |w| w.geometry().0)
}
extern "C" fn native_window_get_height(window: *mut c_void) -> i32 {
    as_window(window).map_or(0, |w| w.geometry().1)
}
extern "C" fn native_window_get_format(window: *mut c_void) -> i32 {
    as_window(window).map_or(0, |w| w.geometry().2)
}
extern "C" fn native_window_set_buffers_geometry(window: *mut c_void, width: i32, height: i32, format: i32) -> i32 {
    let Some(w) = as_window(window) else { return -22 }; // -EINVAL
    let mut g = w.buffers.lock().unwrap_or_else(|e| e.into_inner());
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
extern "C" fn native_window_lock(_window: *mut c_void, _buffer: *mut c_void, _dirty: *mut c_void) -> i32 {
    -38 // -ENOSYS — Roblox renders through GLES/Vulkan, never this path.
}
extern "C" fn native_window_unlock_and_post(_window: *mut c_void) -> i32 {
    -38
}

/// `eglCreateWindowSurface`, with the native window substituted for a real
/// `wl_egl_window*` — the Wayland equivalent of `window.rs`'s XID
/// substitution; see that function's doc for why the engine's own argument
/// is discarded rather than translated (there is exactly one window).
extern "C" fn egl_create_window_surface(
    dpy: *mut c_void,
    config: *mut c_void,
    _native_window: *mut c_void,
    attribs: *mut c_void,
) -> *mut c_void {
    crate::android::glcount::CREATE_WINDOW_SURFACE.fetch_add(1, Ordering::Relaxed);
    let name = c"eglCreateWindowSurface";
    // SAFETY: RTLD_DEFAULT; libEGL is in the global scope by the time the
    // engine reaches this call.
    let f = unsafe { dlsym(std::ptr::null_mut(), name.as_ptr()) };
    if f.is_null() {
        return std::ptr::null_mut();
    }
    type Fn_ = extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
    // SAFETY: resolved from the host for exactly this name.
    let f: Fn_ = unsafe { std::mem::transmute(f) };
    let Some(win) = current().and_then(|w| w.egl_window()) else {
        super::trace(format_args!("wayland: eglCreateWindowSurface asked for a wl_egl_window that could not be created"));
        return std::ptr::null_mut();
    };
    f(dpy, config, win, attribs)
}

/// `EGL_PLATFORM_WAYLAND_KHR`, from `EGL/eglext.h`.
const EGL_PLATFORM_WAYLAND_KHR: u32 = 0x31D8;

/// `eglGetDisplay`, redirected to `eglGetPlatformDisplay`/`...EXT` with
/// Cordial's own `wl_display` connection.
///
/// Roblox calls the plain, platform-agnostic `eglGetDisplay(EGL_DEFAULT_DISPLAY)`
/// — Android has no concept of Wayland, so there was never a reason for it to
/// call anything else. Left uninterposed, Mesa's own platform auto-detection
/// sees `$WAYLAND_DISPLAY` and calls `wl_display_connect(NULL)` *itself*,
/// opening a **second, independent connection** to the compositor. That is
/// silently wrong rather than loudly broken: Wayland object ids are scoped to
/// the connection that created them, so the `wl_buffer`s Mesa allocates on
/// its own connection could never be attached to the `wl_surface` this file
/// created on a different one. X11 has no equivalent hazard — resource ids
/// there are valid across any connection to the same server — which is
/// exactly the kind of Wayland-specific sharp edge ADR-011 means when it
/// calls the new backend "substantially more code... not... a second
/// supported configuration [for X11] worth keeping". Forcing the same
/// connection here is not an optimisation; without it, buffer attachment
/// would fail with a protocol error the first time the engine actually swaps.
extern "C" fn egl_get_display(native_display: *mut c_void) -> *mut c_void {
    let plain_get_display = || {
        let name = c"eglGetDisplay";
        // SAFETY: RTLD_DEFAULT; libEGL is in the global scope by this point.
        let f = unsafe { dlsym(std::ptr::null_mut(), name.as_ptr()) };
        if f.is_null() {
            return std::ptr::null_mut();
        }
        type Fn_ = extern "C" fn(*mut c_void) -> *mut c_void;
        // SAFETY: resolved from the host for exactly this name.
        let f: Fn_ = unsafe { std::mem::transmute(f) };
        f(native_display)
    };

    let Some(w) = current() else {
        // No window yet to bind to — behave exactly as the unpatched call
        // would have.
        return plain_get_display();
    };

    for name in [c"eglGetPlatformDisplay", c"eglGetPlatformDisplayEXT"] {
        // SAFETY: as above.
        let f = unsafe { dlsym(std::ptr::null_mut(), name.as_ptr()) };
        if f.is_null() {
            continue;
        }
        type Fn_ = extern "C" fn(u32, *mut c_void, *const c_void) -> *mut c_void;
        // SAFETY: resolved from the host for exactly this name.
        let f: Fn_ = unsafe { std::mem::transmute(f) };
        let d = f(EGL_PLATFORM_WAYLAND_KHR, w.wl_display(), std::ptr::null());
        if !d.is_null() {
            return d;
        }
    }
    super::trace(format_args!(
        "wayland: neither eglGetPlatformDisplay nor ...EXT resolved; falling back to eglGetDisplay, \
         buffer attachment will likely fail on swap"
    ));
    plain_get_display()
}

/// `eglSwapInterval`, forced to 0 — the same override the X11 backend applies,
/// for a worse reason.
///
/// With a non-zero interval Mesa's Wayland EGL will not return from
/// `eglSwapBuffers` until the compositor delivers a `wl_surface.frame`
/// callback. On X11 the equivalent wait was for a vblank source the host could
/// not supply and cost frame rate; here it costs everything, because the
/// callback is delivered on a Wayland event queue and a render thread blocked
/// inside `eglSwapBuffers` is not dispatching one. The first frame never
/// returns, no buffer is ever attached to the surface, and the compositor shows
/// a window with nothing in it — present in the dock and in alt-tab, blank on
/// screen, which is exactly what this looked like.
///
/// Forcing the interval Mesa actually receives to 0 makes `eglSwapBuffers`
/// return as soon as the frame is submitted. The engine still paces itself
/// through its own `RenderJob` timing, so this removes a broken throttle rather
/// than handing it a runaway framerate.
extern "C" fn egl_swap_interval(dpy: *mut c_void, _interval: c_int) -> u32 {
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    let name = std::ffi::CString::new("eglSwapInterval").unwrap_or_default();
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
        f!("eglGetDisplay", egl_get_display),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_positions_and_side_buttons_keep_their_meaning() {
        let position = pack_pointer_position(123.5, -45.25);
        assert_eq!(unpack_pointer_position(position), (123.5, -45.25));

        assert_eq!(linux_button_to_android(0x110), Some(super::super::input::BUTTON_PRIMARY));
        assert_eq!(linux_button_to_android(0x111), Some(super::super::input::BUTTON_SECONDARY));
        assert_eq!(linux_button_to_android(0x112), Some(super::super::input::BUTTON_TERTIARY));
        assert_eq!(linux_button_to_android(0x113), Some(super::super::input::BUTTON_BACK));
        assert_eq!(linux_button_to_android(0x114), Some(super::super::input::BUTTON_FORWARD));
    }

    // The `app_id_matches_the_desktop_entry` test that lived here moved to
    // `cordial_shell::host_window`, which is what sets the app_id now that GTK
    // owns the toplevel. Same test, same ADR-009 reason; it has to sit beside
    // the constant that actually reaches the wire or it pins nothing.

    /// The listener array `wl_proxy_add_listener` is handed must have one
    /// function pointer per event the interface declares, and the interface
    /// must declare every event of the version bound. Getting that wrong is
    /// how `interface 'zwp_text_input_v3' has no event 8` happened: the table
    /// described version 1 while `bind` asked for version 2, and the fatal
    /// gap was three events wide.
    ///
    /// Only the hand-written interface can be checked here. `wl_pointer` and
    /// `wl_keyboard` come from the host's own `libwayland-client.so`, so their
    /// `event_count` is not a constant this crate can see at compile time —
    /// which is exactly why the module doc says the listeners for those must
    /// carry the *complete current* event set rather than the one matching
    /// some version this file picked.
    #[test]
    fn the_text_input_listener_has_a_slot_for_every_event_declared() {
        assert_eq!(TEXT_INPUT_EVENTS.len() as c_int, TEXT_INPUT_INTERFACE.event_count);
        assert_eq!(TEXT_INPUT_METHODS.len() as c_int, TEXT_INPUT_INTERFACE.method_count);
        assert_eq!(
            std::mem::size_of::<TextInputListener>(),
            TEXT_INPUT_EVENTS.len() * std::mem::size_of::<*const c_void>(),
            "TextInputListener must be exactly one function pointer per declared event"
        );
    }

    /// Every string and count of the two pointer-capture protocols, against
    /// what `wayland-scanner private-code` emitted for the upstream XML.
    ///
    /// The module doc owes the reader this: two hand-written protocols were
    /// added after that doc told the next person not to add any. The mitigation
    /// promised there is that nothing below was typed from the protocol
    /// description — it was copied out of the generator's own output — and this
    /// is what stops that staying true only until someone edits a signature by
    /// hand. Compare against `wayland-scanner private-code
    /// pointer-constraints-unstable-v1.xml`:
    ///
    /// ```text
    /// { "lock_pointer", "noo?ou", ... }
    /// { "set_cursor_position_hint", "ff", ... }
    /// { "relative_motion", "uuffff", ... }
    /// ```
    ///
    /// A signature wrong by one argument makes `wl_proxy_marshal_flags` read
    /// the wrong number of variadic arguments off the stack and corrupt the
    /// wire, and an event table shorter than what the compositor sends indexes
    /// past the end of a listener array — the two failures this file has
    /// already paid for once each.
    #[test]
    fn pointer_capture_tables_match_wayland_scanner() {
        fn sig(m: &WlMessage) -> &str {
            // SAFETY: every signature in this file is a `c"..."` literal.
            unsafe { CStr::from_ptr(m.signature) }.to_str().expect("ASCII literal")
        }

        assert_eq!(sig(&POINTER_CONSTRAINTS_METHODS[1]), "noo?ou", "lock_pointer");
        assert_eq!(sig(&POINTER_CONSTRAINTS_METHODS[2]), "noo?ou", "confine_pointer");
        assert_eq!(sig(&LOCKED_POINTER_METHODS[1]), "ff", "set_cursor_position_hint");
        assert_eq!(sig(&LOCKED_POINTER_METHODS[2]), "?o", "set_region");
        assert_eq!(sig(&RELATIVE_POINTER_MANAGER_METHODS[1]), "no", "get_relative_pointer");
        assert_eq!(sig(&RELATIVE_POINTER_EVENTS[0]), "uuffff", "relative_motion");

        // Opcodes are positions in these arrays, so naming one that has moved
        // sends a different request entirely.
        assert_eq!(POINTER_CONSTRAINTS_LOCK_POINTER, 1);
        assert_eq!(LOCKED_POINTER_DESTROY, 0);
        assert_eq!(LOCKED_POINTER_SET_CURSOR_POSITION_HINT, 1);
        assert_eq!(RELATIVE_POINTER_MANAGER_GET_RELATIVE_POINTER, 1);

        for (iface, methods, events) in [
            (&POINTER_CONSTRAINTS_INTERFACE, POINTER_CONSTRAINTS_METHODS.len(), 0),
            (&LOCKED_POINTER_INTERFACE, LOCKED_POINTER_METHODS.len(), LOCKED_POINTER_EVENTS.len()),
            (&RELATIVE_POINTER_MANAGER_INTERFACE, RELATIVE_POINTER_MANAGER_METHODS.len(), 0),
            (&RELATIVE_POINTER_INTERFACE, RELATIVE_POINTER_METHODS.len(), RELATIVE_POINTER_EVENTS.len()),
        ] {
            assert_eq!(iface.method_count, methods as c_int);
            assert_eq!(iface.event_count, events as c_int);
            // All four are version 1 upstream, which is also why no signature
            // above carries a `since` prefix. Raising one means adding that
            // version's events here first — the `zwp_text_input_v3` lesson.
            assert_eq!(iface.version, 1);
        }

        assert_eq!(
            std::mem::size_of::<LockedPointerListener>(),
            LOCKED_POINTER_EVENTS.len() * std::mem::size_of::<*const c_void>(),
        );
        assert_eq!(
            std::mem::size_of::<RelativePointerListener>(),
            RELATIVE_POINTER_EVENTS.len() * std::mem::size_of::<*const c_void>(),
        );
    }

    /// `wl_fixed_t` round-trips, because the cursor position hint is the one
    /// place this file *sends* one and a wrong scale would put the cursor 256
    /// times too far out on release — off the monitor, on a locked pointer,
    /// which is the failure mode with the worst possible manners.
    #[test]
    fn a_cursor_position_hint_survives_the_fixed_point_conversion() {
        assert_eq!(f32_to_fixed(1.0), 256);
        assert_eq!(f32_to_fixed(0.0), 0);
        assert_eq!(fixed_to_f32(f32_to_fixed(640.5)), 640.5);
        assert_eq!(fixed_to_f32(f32_to_fixed(-3.25)), -3.25);
    }

    #[test]
    fn scrolling_down_reports_a_negative_vertical_notch() {
        // Wayland's positive is down; Android's AXIS_VSCROLL positive is away
        // from the user. A sign error here is not subtle to a person — the page
        // goes the wrong way — but it is invisible to a build, so it is pinned.
        let (h, v) = axis_to_notches(0, WHEEL_AXIS_STEP).expect("vertical axis is known");
        assert_eq!((h, v), (0.0, -1.0));
        let (h, v) = axis_to_notches(0, -WHEEL_AXIS_STEP).expect("vertical axis is known");
        assert_eq!((h, v), (0.0, 1.0));
        // Horizontal keeps Wayland's sign, because both call right positive.
        let (h, v) = axis_to_notches(1, WHEEL_AXIS_STEP).expect("horizontal axis is known");
        assert_eq!((h, v), (1.0, 0.0));
        // A third axis is not a thing `wl_pointer` has; inventing a meaning for
        // one would scroll on an event that said nothing about scrolling.
        assert!(axis_to_notches(2, WHEEL_AXIS_STEP).is_none());
    }

    /// A `zwp_text_input_v3` is created by the manager and therefore *is*
    /// whatever version the manager was bound at, whatever number this file
    /// passes to `wl_proxy_marshal_flags`. These two drifting apart is the
    /// whole bug, so they are pinned together rather than left as a comment.
    #[test]
    fn the_text_input_and_its_manager_declare_the_same_version() {
        assert_eq!(TEXT_INPUT_INTERFACE.version, TEXT_INPUT_MANAGER_INTERFACE.version);
        // Version 2 is what GNOME 50's mutter advertises, measured on the wire:
        // `wl_registry#107.global(26, "zwp_text_input_manager_v3", 2)`. Raising
        // this means adding the new version's events to the table above first.
        assert_eq!(TEXT_INPUT_INTERFACE.version, 2);
    }

    #[test]
    fn no_preedit_leaves_committed_text_untouched() {
        let (text, caret) = splice_preedit("hello", 5, None);
        assert_eq!(text, "hello");
        assert_eq!(caret, 5);
    }

    #[test]
    fn preedit_is_spliced_at_the_caret_not_appended() {
        // The caret is mid-string (after "he"), not at the end — a splice
        // that always appended at the end would be indistinguishable from a
        // commit in this test and would miss the actual bug class this
        // guards: composing in the middle of existing text.
        let (text, caret) = splice_preedit("hello", 2, Some(&("XX".to_string(), 2, 2)));
        assert_eq!(text, "heXXllo");
        // Two committed chars before the caret, plus both preedit chars
        // (cursor_begin=2 is the end of a 2-char preedit).
        assert_eq!(caret, 4);
    }

    #[test]
    fn preedit_cursor_can_land_inside_the_composing_text() {
        // A predictive engine can put its cursor partway through what it is
        // suggesting, not only at the end — e.g. showing "ing" appended to a
        // stem with the cursor still after the stem.
        let (text, caret) = splice_preedit("run", 3, Some(&("ning".to_string(), 0, 0)));
        assert_eq!(text, "running");
        // cursor_begin=0 means the preedit's own cursor is at its start, so
        // the displayed caret stays at the committed caret (3), not
        // advanced into "ning".
        assert_eq!(caret, 3);
    }

    #[test]
    fn preedit_replaces_rather_than_appends_to_the_previous_one() {
        // "A new preedit_string replaces the previous one entirely" — the
        // module doc's own rule. This is really documenting `ImeState`'s
        // assignment (`ime.pending.preedit = Some(...)`, not a push/append),
        // but `splice_preedit` only ever sees the current value, so an
        // out-of-date caller passing a stale preedit is exactly the bug this
        // would catch if `apply_ime_done` ever accumulated instead of
        // replacing.
        let after_first = splice_preedit("x", 1, Some(&("a".to_string(), 1, 1)));
        assert_eq!(after_first.0, "xa");
        let after_second = splice_preedit("x", 1, Some(&("ab".to_string(), 2, 2)));
        assert_eq!(after_second.0, "xab");
    }

    #[test]
    fn an_empty_preedit_still_splices_as_a_real_value() {
        // "An empty `preedit_string` clears composition; that is a real
        // event, not a no-op" — `Some(("", ..))` must behave identically to
        // `None` for display purposes (nothing to splice in), which this
        // checks explicitly rather than trusting an empty string to fall out
        // of the general case correctly.
        let (text, caret) = splice_preedit("hi", 2, Some(&(String::new(), 0, 0)));
        assert_eq!(text, "hi");
        assert_eq!(caret, 2);
    }

    #[test]
    fn preedit_splicing_counts_the_committed_caret_in_characters() {
        // The committed side of the splice takes a char index (that is what
        // `text_buffer_snapshot` reports), so a multi-byte character before
        // the caret must not shift where the preedit lands.
        let (text, caret) = splice_preedit("héllo", 2, Some(&("X".to_string(), 0, 0)));
        assert_eq!(text, "héXllo");
        assert_eq!(caret, 2);
    }
}
