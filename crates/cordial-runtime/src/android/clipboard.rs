//! Copy and paste between the host and Roblox.
//!
//! **The engine does not ask for `android.content.ClipboardManager`.** That
//! class, `ClipData`, `ClipData$Item` and `ClipDescription` are all in
//! `docs/analysis/framework-classes.txt`, which is what made this look like a
//! framework class to write. It is not one. That file is the dex's
//! referenced-type table, so it records what Roblox's *Java* code uses, and
//! Cordial does not run Roblox's Java code — it stands in for it. The same
//! distinction is already written down for `WebView` in
//! `docs/analysis/webview-surface.md` §1 and for `Intent` in
//! `docs/analysis/deep-links.md` §1, and it applies unchanged here.
//!
//! So there are two directions and they are not symmetrical.
//!
//! **Out of Roblox.** The engine publishes `ExternalContentSharing.setClipboardText`
//! on its own message bus and expects the Java side to put the text on the
//! system clipboard. `native/clipboard.cpp` holds the evidence for that and the
//! subscription itself; this module is the half that owns the host clipboard
//! and the payload.
//!
//! **Into Roblox.** Nothing is published, requested or exported for this, and
//! that is not a gap. On Android a focused TextBox is edited by a real
//! `android.widget.EditText` laid over the GL surface — see `CordialTextBoxInfo`
//! in `native/android_classes.cpp` — so Android's own editor handles Ctrl+V and
//! the engine only ever sees the resulting text arrive through `gametextinput`.
//! Cordial *is* that editor, in [`super::input`], so a paste is an insert into
//! the focused field followed by the same `syncTextbox`/`text_input` calls a
//! keystroke makes. [`paste_into_engine`] is that, and it touches no JNI the
//! typing path does not already touch.
//!
//! **Nothing here logs a clipboard value.** What is in the host clipboard when
//! Roblox asks may be a password from somebody's manager or a message meant for
//! somebody else, and what leaves Roblox may be a private server link. Byte
//! counts and JSON member *names* are printed; values are not, at any
//! verbosity, behind any flag. `crate::deeplink` sets the same rule for URLs and
//! for the same reason.
//!
//! `CORDIAL_SKIP_CLIPBOARD=1` is the control: the Java classes are still
//! registered and the subscription is still made, so a run with it set differs
//! from one without in exactly whether anything acts on a message.
//! `CORDIAL_TRACE_CLIPBOARD=1` reports transfers by size and direction.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use cordial_linker_sys as linker;

/// The message the engine publishes when something inside it asks for text to
/// be put on the clipboard.
///
/// Spelled out rather than asked for. The four sharing methods each have a
/// native that hands out their id — `JNIExternalContentSharingProtocol`
/// exports `getShareTextId`, `getShareUrlId`, `getShareImageId` and
/// `getShareVideoId` — and `setClipboardText` has no such getter. The string
/// itself is in both the engine (as `ExternalContentSharing` and
/// `setClipboardText`) and the dex (whole, as one string).
const SET_CLIPBOARD_TEXT: &str = "ExternalContentSharing.setClipboardText";

/// A clipboard transfer this process will attempt, in either direction.
///
/// Not a limit the engine imposes and not a limit the clipboard imposes — a
/// bound on what a bridge will move without anybody having asked for more.
/// Roblox's own text fields are short (a username, a chat line, a place link),
/// and a multi-megabyte paste into a login box is far likelier to be a mistake
/// or an attempt at one than a thing somebody wanted.
const MAX_BYTES: usize = 64 * 1024;

/// Whether the bridge acts on what it hears.
///
/// The control switch, and deliberately *only* about acting: registration and
/// the subscription happen either way, so a difference between two runs cannot
/// be confused with the engine failing to resolve the callback. `cookies.rs`
/// and `identity.rs` split the same way.
pub fn enabled() -> bool {
    std::env::var_os("CORDIAL_SKIP_CLIPBOARD").is_none()
}

fn trace() -> bool {
    std::env::var_os("CORDIAL_TRACE_CLIPBOARD").is_some()
}

// SAFETY: all of these live in `native/clipboard.cpp`, linked into this binary
// through `cordial-linker-sys`'s `cordial_jni_shim`.
//
// Generalised names, not `cordial_clipboard_*`: the C++ side keeps a callback
// and a `Connection` per message id now rather than one of each for the whole
// process, so clipboard is one caller of that mechanism and not its owner —
// see the file comment at the top of `native/clipboard.cpp`. `grep -rn
// cordial_clipboard_` before this change turned up nothing outside that file
// and this one, so the rename carries no dangling caller.
unsafe extern "C" {
    fn cordial_messagebus_subscribe(
        f: *mut c_void,
        message_id: *const c_char,
        sink: Option<extern "C" fn(*const c_char)>,
        err: *mut c_char,
        n: usize,
    ) -> c_int;
    fn cordial_messagebus_connection_ptr(message_id: *const c_char) -> i64;
    fn cordial_messagebus_is_connected(
        f: *mut c_void,
        ptr: i64,
        out_connected: *mut c_int,
        err: *mut c_char,
        n: usize,
    ) -> c_int;
}

fn take_err(err: Vec<u8>) -> String {
    let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
    String::from_utf8_lossy(&err[..end]).into_owned()
}

// ------------------------------------------------------------------ the payload
//
// What the engine puts in the JSON is the one thing about this surface that is
// not established, and it cannot be established without a copy happening inside
// a running experience. So it is handled by rule rather than by guess, and the
// rule says out loud which member it took.

/// Member names to look for first, most specific first.
///
/// `content` leads because Roblox's own Java says so: the dex carries the error
/// text "setClipboardText received null content value for clipboard." That is
/// a sentence about a value it calls the content, not a declaration of a field
/// name, so it is a lead and not a fact. **INFERRED**, and the reason
/// [`text_from_payload`] does not stop at this list.
const CANDIDATES: &[&str] = &["content", "text", "value", "clipboardText", "data"];

/// Which member a payload's text came out of, and the text.
///
/// The name is carried out alongside the value so the caller can report it.
/// One real copy from inside an experience settles the key for good, and the
/// only way that ever gets written down is if the run that saw it said which
/// member it used.
pub struct Extracted {
    pub key: String,
    pub text: String,
}

/// A `Debug` that refuses to print the text.
///
/// Hand-written rather than derived, and not as a nicety: `unwrap_err()` in a
/// test prints the `Ok` side, and a derived `Debug` would therefore put a
/// clipboard value into test output the first time anybody wrote an assertion
/// the other way round. `identity.rs` holds the same rule for a username, and
/// `cordial_linker_sys`'s cookie type for a session. The struct is small enough
/// that this is two lines; the reason it exists is the whole of why.
impl std::fmt::Debug for Extracted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Extracted {{ key: {:?}, {} bytes }}", self.key, self.text.len())
    }
}

/// Pull the text out of a `setClipboardText` payload.
///
/// Fails rather than returning an empty string when nothing in the document
/// looks like the text. An empty success here would put an empty clipboard in
/// front of somebody who pressed copy and watched it appear to work, which is
/// the shape AGENTS.md is about: the caller proceeds on an answer that is not
/// true. The error names the members that *were* present, never their values.
pub fn text_from_payload(json: &str) -> Result<Extracted, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("the payload is not JSON ({} bytes): {e}", json.len()))?;

    // A bare string is not what the bus is documented to carry, but it costs
    // nothing to accept and refusing it would be a failure with no cause
    // anybody could see.
    if let Some(s) = value.as_str() {
        return Ok(Extracted { key: "(the whole payload)".into(), text: s.to_string() });
    }

    let Some(object) = value.as_object() else {
        return Err(format!("the payload is {} and carries no members", kind_of(&value)));
    };

    for key in CANDIDATES {
        if let Some(s) = object.get(*key).and_then(|v| v.as_str()) {
            return Ok(Extracted { key: (*key).to_string(), text: s.to_string() });
        }
    }

    // Nothing matched. If the document holds exactly one string, it is not much
    // of a leap that the string is the text — but say which member it was, so
    // that the next person adds it to CANDIDATES from an observation rather
    // than from another guess.
    let mut strings = object.iter().filter(|(_, v)| v.is_string());
    if let (Some((key, v)), None) = (strings.next(), strings.next()) {
        return Ok(Extracted {
            key: key.clone(),
            text: v.as_str().unwrap_or_default().to_string(),
        });
    }

    Err(format!(
        "no member of the payload looks like the text; it has {}",
        member_names(object)
    ))
}

fn kind_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// The member names of a payload, and their types — never their values.
fn member_names(object: &serde_json::Map<String, serde_json::Value>) -> String {
    if object.is_empty() {
        return "no members".into();
    }
    object
        .iter()
        .map(|(k, v)| format!("{k} ({})", kind_of(v)))
        .collect::<Vec<_>>()
        .join(", ")
}

// -------------------------------------------------------------- engine -> host
//
// The engine publishes on whichever thread the copy happened on. GTK is not
// that thread and never is, so nothing here touches GDK: the sink parks the
// text and the looper thread — the one that ran `gtk_init`, and the one every
// other native call in this process is made from — picks it up. That is the
// same split `cordial_app_ready_set_sink` describes in
// `native/android_classes.cpp`, made for the same reason.

/// Text the engine published, waiting for the GTK thread.
static PENDING: Mutex<Option<Extracted>> = Mutex::new(None);

/// The sink `native/clipboard.cpp` calls when the engine publishes.
///
/// Runs on the engine's thread. Parses, parks, and returns — no GDK, no
/// blocking, and nothing that could call back into the engine from inside the
/// engine's own publish.
extern "C" fn on_payload(json: *const c_char) {
    if json.is_null() {
        return;
    }
    // SAFETY: the C side passes a NUL-terminated string that outlives the call.
    let json = unsafe { CStr::from_ptr(json) };
    let Ok(json) = json.to_str() else {
        eprintln!("[clipboard] the engine published something that is not UTF-8; ignored");
        return;
    };
    match text_from_payload(json) {
        Ok(extracted) => {
            if extracted.text.len() > MAX_BYTES {
                eprintln!(
                    "[clipboard] refusing {} bytes from the engine; the limit is {MAX_BYTES}",
                    extracted.text.len()
                );
                return;
            }
            if trace() {
                eprintln!(
                    "[clipboard] engine -> host: {} bytes from the payload's {:?} member",
                    extracted.text.len(),
                    extracted.key
                );
            }
            *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(extracted);
        }
        // Loud, because the alternative is a copy that silently does nothing.
        Err(e) => eprintln!("[clipboard] cannot use the engine's setClipboardText payload: {e}"),
    }
}

/// Hand anything the engine published to the host clipboard.
///
/// **Must be called on the thread that ran `gtk_init`.** [`super::looper::pump`]
/// is that thread; nothing else in this process may call it.
pub fn pump_pending() {
    let Some(extracted) = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    match set_host_text(&extracted.text) {
        Ok(()) => println!(
            "[clipboard] {} bytes onto the host clipboard (from the payload's {:?} member)",
            extracted.text.len(),
            extracted.key
        ),
        Err(e) => println!("[clipboard] could not reach the host clipboard: {e}"),
    }
}

/// The GDK clipboard of the display GTK opened.
///
/// There is exactly one display in this process and the engine's own
/// `wl_surface` is a subsurface on its connection — see `wayland.rs`'s module
/// doc on why a second connection is not an option — so this is the host
/// clipboard, not a second view of one.
fn host_clipboard() -> Result<gtk4::gdk::Clipboard, String> {
    use gtk4::prelude::DisplayExt;
    let display = gtk4::gdk::Display::default()
        .ok_or_else(|| "GTK has no display open (is this the X11 backend?)".to_string())?;
    Ok(display.clipboard())
}

fn set_host_text(text: &str) -> Result<(), String> {
    host_clipboard()?.set_text(text);
    Ok(())
}

// -------------------------------------------------------------- host -> engine

/// Read the host clipboard, blocking this thread until it answers or the wait
/// runs out.
///
/// GDK has no synchronous read and cannot have one: on Wayland the text lives
/// in whichever *other* client owns the selection, and getting it is a pipe and
/// a round trip. `read_text_async` plus a bounded turn of the main loop is the
/// whole of what a synchronous caller can do, and this caller is synchronous
/// because a paste happens inside one turn of the engine's own pump.
///
/// The timeout is short on purpose. A clipboard owner that has gone away, or
/// one that is slow, must not stall the engine's loop — a paste that does not
/// arrive is a paste that did not happen, and that is recoverable in a way that
/// a frozen client is not.
fn host_text(timeout: std::time::Duration) -> Result<String, String> {
    use gtk4::glib;
    use std::cell::RefCell;
    use std::rc::Rc;

    let clipboard = host_clipboard()?;
    let answer: Rc<RefCell<Option<Result<String, String>>>> = Rc::new(RefCell::new(None));
    let sink = answer.clone();
    clipboard.read_text_async(gtk4::gio::Cancellable::NONE, move |r| {
        *sink.borrow_mut() = Some(match r {
            Ok(Some(text)) => Ok(text.to_string()),
            Ok(None) => Err("the host clipboard holds no text".to_string()),
            // The error is GIO's and names a transfer format or a broken pipe,
            // never a value.
            Err(e) => Err(e.to_string()),
        });
    });

    let context = glib::MainContext::default();
    let deadline = std::time::Instant::now() + timeout;
    while answer.borrow().is_none() {
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "the host clipboard did not answer within {} ms",
                timeout.as_millis()
            ));
        }
        // Non-blocking, then a short sleep, rather than `iteration(true)`: a
        // blocking iteration here would sit in the main loop for as long as the
        // clipboard owner cared to take, which is exactly the stall the
        // deadline exists to prevent.
        if !context.iteration(false) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    let answered = answer.borrow_mut().take().expect("just checked it is Some");
    answered
}

/// Put the host clipboard's text into the focused Roblox text box.
///
/// This is what Android's `EditText` does on Ctrl+V, expressed in the calls
/// Cordial already makes for a keystroke: insert at the caret, then report the
/// field's new contents. No key event is sent, because a paste is not one —
/// Android's editor does not synthesise keystrokes for pasted characters
/// either.
///
/// Returns how many characters went in. `Ok(0)` means no box had focus, which
/// is a result and not a failure: text sent with nothing focused goes to handle
/// 0 and the engine drops it in silence (see [`super::input::pass_text`]).
///
/// **Must be called on the thread that ran `gtk_init`.**
pub fn paste_into_engine(handle: i64) -> Result<usize, String> {
    if !enabled() {
        return Err("the clipboard bridge is off (CORDIAL_SKIP_CLIPBOARD)".into());
    }
    let text = host_text(std::time::Duration::from_millis(400))?;
    if text.len() > MAX_BYTES {
        return Err(format!(
            "the host clipboard holds {} bytes and the limit is {MAX_BYTES}",
            text.len()
        ));
    }
    let Some(which) = linker::game_activity::focused_textbox() else {
        if trace() {
            eprintln!("[clipboard] host -> engine: {} bytes, but no box has focus", text.len());
        }
        return Ok(0);
    };
    // Control characters are dropped by `edit_text_buffer` as a whole insert
    // rather than character by character, so a clipboard carrying a newline
    // would otherwise paste nothing at all with no explanation. Flattening is
    // what a single-line Android field does with pasted newlines, and Roblox's
    // login and chat boxes are single-line.
    let flattened: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let Some((contents, caret)) = super::input::edit_text_buffer(super::input::Edit::Insert(&flattened))
    else {
        return Err("the field refused the insert".into());
    };
    let _ = linker::game_activity::text_input(handle, &contents, caret, caret);
    super::input::pass_text(which, &contents, caret);
    super::input::deliver_surface_redraw(handle);
    let n = flattened.chars().count();
    if trace() {
        eprintln!("[clipboard] host -> engine: {n} characters into the focused box");
    }
    Ok(n)
}

/// Insert `text` at the focused box's caret, for the development control
/// surface's `paste` verb -- a harness's equivalent of Ctrl+V, from a file
/// instead of the host clipboard, for a payload too big to type one keystroke
/// per character (`text` goes through `script_type`) and without touching the
/// clipboard the developer sitting at this machine is using.
///
/// **On Wayland this does not touch [`paste_into_engine`] or `TEXT_BUFFER` at
/// all.** Since `fd0f0c6` a real `gtk::Text` owns the field whenever one has
/// focus -- see `wayland::WaylandWindow::editor_owns_text` -- and a real
/// Ctrl+V is delivered to *that widget's own* clipboard handling; the guard
/// right above the `is_paste_shortcut` check in `wayland.rs` returns before
/// `paste_into_engine` is ever reached while a box is focused, which is
/// **always**, given a paste is only meaningful with one. So the widget is
/// where a real paste's insert-at-caret happens, and this verb goes there too
/// -- writing `TEXT_BUFFER` directly instead (as an earlier, uncommitted
/// version of this function did) leaves the widget showing stale text until
/// the next pump tick's `sync_text_overlay` catches up, and a real keystroke
/// landing in that window reverts the paste: GDK delivers it straight to the
/// widget, which still holds the old text, and the resulting `changed` signal
/// overwrites `TEXT_BUFFER` with the widget's stale content plus one
/// keystroke. Going through the widget makes that window not exist, because
/// GTK's `changed`/`notify::cursor-position` signals are emitted synchronously
/// inside `insert_text`/`set_position`, so `connect_editor_changed` has
/// already run -- mirroring the buffer, pushing to the engine, redrawing --
/// by the time this function returns.
///
/// Returns the field's new character count. `Ok(0)` means no box had focus,
/// which is a result and not a failure.
pub fn paste_text(handle: i64, text: &str) -> Result<usize, String> {
    replace_focused_text(handle, text, false)
}

/// Replace the focused box's whole contents, for the development control
/// surface's `settext` verb.
///
/// Deliberately not select-all followed by [`paste_text`]: that would make a
/// harness using `settext` incidentally exercise select-all as well, and
/// whether select-all itself works through this surface is a separate
/// question (see `docs/NEXT.md` and the devctl `tap 30 4096` check). This is
/// a test seam with no user-reachable equivalent, documented the way
/// `input::FAKE_ENGINE_LOCK` is: a real user cannot instantly replace a
/// field's contents without going through a selection first, and `settext`
/// skips straight to the state that selection would have produced.
///
/// Same widget-vs-buffer split as [`paste_text`], and the same reasoning.
pub fn set_text(handle: i64, text: &str) -> Result<usize, String> {
    replace_focused_text(handle, text, true)
}

/// Shared by [`paste_text`] and [`set_text`]: check the size once, then hand
/// off to whichever backend is the authority for the focused field's text.
fn replace_focused_text(handle: i64, text: &str, replace_all: bool) -> Result<usize, String> {
    if text.len() > MAX_BYTES {
        return Err(format!("{} bytes; the limit is {MAX_BYTES}", text.len()));
    }
    if linker::game_activity::focused_textbox().is_none() {
        return Ok(0);
    }
    if let Some(w) = super::wayland::current() {
        // The widget flattens exactly as the buffer path below would --
        // `editor_paste_at_caret`/`editor_set_text` call GTK's `insert_text`/
        // `set_text` with this same flattened string, not the raw file
        // contents, so a payload is treated identically whichever backend
        // ends up holding it.
        let flattened = super::input::flatten_for_field(text);
        return Ok(w.devctl_replace_editor_text(&flattened, replace_all));
    }
    // X11, which has no editor widget (ADR-024) and so has `TEXT_BUFFER` as
    // the only place the text lives -- the same shape `paste_into_engine`
    // already uses for X11's own Ctrl+V. Also Wayland before a window exists,
    // which should not be reachable given the focus check above, but is a
    // safe fallback rather than a panic if it ever is.
    match super::input::devctl_replace_text(text, replace_all, MAX_BYTES)? {
        None => Ok(0),
        Some((which, contents, caret)) => {
            let _ = linker::game_activity::text_input(handle, &contents, caret, caret);
            super::input::pass_text(which, &contents, caret);
            super::input::deliver_surface_redraw(handle);
            Ok(contents.chars().count())
        }
    }
}

// ------------------------------------------------------------------- arming

static ARMED: AtomicBool = AtomicBool::new(false);
static CONNECTION: AtomicI64 = AtomicI64::new(0);
static IS_CONNECTED_NATIVE: OnceLock<usize> = OnceLock::new();

/// Subscribe to `ExternalContentSharing.setClipboardText`, once.
///
/// Called from [`super::looper::pump`] rather than from `load.rs`, which is
/// where the cookie and identity wiring lives, for one reason: the bus has to
/// exist before anything can subscribe to it, and by the time the looper is
/// pumping the app bridge has started. The library handle is re-opened here
/// rather than threaded through, because the bionic linker hands back the
/// object it already has for a soname it has already loaded.
pub fn arm() {
    if ARMED.swap(true, Ordering::SeqCst) {
        return;
    }
    let lib = match linker::dlopen("libroblox.so", 0x2 /* RTLD_NOW */) {
        Ok(lib) => lib,
        Err(e) => {
            println!("[clipboard] cannot reach the engine's symbols: {e}");
            return;
        }
    };
    let Some(subscribe) =
        lib.symbol("Java_com_roblox_universalapp_messagebus_MessageBus_doSubscribeRaw")
    else {
        println!(
            "[clipboard] doSubscribeRaw is not exported; copying out of Roblox will do nothing"
        );
        return;
    };

    // The sink is passed into the subscribe call itself now, rather than
    // installed separately beforehand: the C++ side sets it on the callback
    // object before that object is ever handed to `doSubscribeRaw`, which
    // keeps the same ordering guarantee clipboard always needed — the bus may
    // deliver a message synchronously from inside the subscribing call, and a
    // callback whose sink was still unset at that point would drop it with
    // nothing in the log to say why.
    let sink = if enabled() {
        Some(on_payload as extern "C" fn(*const c_char))
    } else {
        println!("[clipboard] bridge off (CORDIAL_SKIP_CLIPBOARD); subscribing anyway, as a control");
        None
    };

    let id = CString::new(SET_CLIPBOARD_TEXT).expect("the message id has no NUL in it");
    let mut err = vec![0u8; 512];
    // SAFETY: `subscribe` resolved under its own name, so it is the native this
    // signature describes; every buffer outlives the call.
    let rc = unsafe {
        cordial_messagebus_subscribe(
            subscribe,
            id.as_ptr(),
            sink,
            err.as_mut_ptr() as *mut c_char,
            err.len(),
        )
    };
    if rc != 0 {
        println!("[clipboard] doSubscribeRaw failed: {}", take_err(err));
        return;
    }
    // SAFETY: reads a value the subscribe call above stored, keyed by the same
    // message id.
    let ptr = unsafe { cordial_messagebus_connection_ptr(id.as_ptr()) };
    CONNECTION.store(ptr, Ordering::Relaxed);
    if let Some(f) = lib.symbol("Java_com_roblox_universalapp_messagebus_Connection_isConnected") {
        let _ = IS_CONNECTED_NATIVE.set(f as usize);
    }
    match connected() {
        Some(true) => println!("[clipboard] subscribed to {SET_CLIPBOARD_TEXT}; the bus says it is live"),
        Some(false) => println!(
            "[clipboard] subscribed to {SET_CLIPBOARD_TEXT}, but the bus says the connection is not live"
        ),
        None => println!(
            "[clipboard] subscribed to {SET_CLIPBOARD_TEXT}; nothing here can confirm it (no Connection came back)"
        ),
    }
}

/// What `Connection.isConnected` says about the subscription, or `None` when
/// there is nothing to ask about.
pub fn connected() -> Option<bool> {
    let ptr = CONNECTION.load(Ordering::Relaxed);
    let f = *IS_CONNECTED_NATIVE.get()? as *mut c_void;
    if ptr == 0 {
        return None;
    }
    let mut out: c_int = -1;
    let mut err = vec![0u8; 256];
    // SAFETY: the native resolved under its own name; the buffers outlive the call.
    let rc = unsafe {
        cordial_messagebus_is_connected(
            f,
            ptr,
            &mut out as *mut c_int,
            err.as_mut_ptr() as *mut c_char,
            err.len(),
        )
    };
    (rc == 0).then_some(out != 0)
}

/// Publish a `setClipboardText` on the engine's own bus, as the engine would.
///
/// This exists to make the outward path testable at all. A copy inside an
/// experience needs an account and an experience; this needs neither, and it
/// goes through the real `MessageBus.publishRaw` and the real subscription, so
/// everything from the bus outwards is the same code a real copy would take.
///
/// What it does **not** test is the payload's member name, because the caller
/// chooses it. That is the one part of this surface still marked INFERRED, and
/// no run driven from here can settle it.
pub fn publish_probe(text: &str) -> Result<(), String> {
    let lib = linker::dlopen("libroblox.so", 0x2 /* RTLD_NOW */)
        .map_err(|e| format!("cannot reach the engine's symbols: {e}"))?;
    let publish = lib
        .symbol("Java_com_roblox_universalapp_messagebus_MessageBus_publishRaw")
        .ok_or_else(|| "publishRaw is not exported".to_string())?;
    let payload = serde_json::json!({ "content": text }).to_string();
    // SAFETY: `publish` was just resolved from the loaded `libroblox.so` above,
    // and the library is never unloaded.
    unsafe {
        linker::game_activity::call_static_strings(
            publish,
            "com/roblox/universalapp/messagebus/MessageBus",
            &[SET_CLIPBOARD_TEXT, &payload],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_member_wins() {
        let e = text_from_payload(r#"{"content":"hello","text":"wrong"}"#).unwrap();
        assert_eq!(e.key, "content");
        assert_eq!(e.text, "hello");
    }

    #[test]
    fn a_later_candidate_is_taken_when_the_first_is_absent() {
        let e = text_from_payload(r#"{"kind":"text","text":"hello"}"#).unwrap();
        assert_eq!(e.key, "text");
        assert_eq!(e.text, "hello");
    }

    #[test]
    fn a_lone_string_is_taken_and_named() {
        let e = text_from_payload(r#"{"somethingNobodyPredicted":"hello","count":3}"#).unwrap();
        assert_eq!(e.key, "somethingNobodyPredicted");
        assert_eq!(e.text, "hello");
    }

    /// The case AGENTS.md is about. Two strings and no candidate is a payload
    /// this cannot read, and reading it wrongly would put somebody else's
    /// string on the clipboard.
    #[test]
    fn an_unreadable_payload_fails_rather_than_guessing() {
        let e = text_from_payload(r#"{"a":"one","b":"two"}"#).unwrap_err();
        assert!(e.contains("a (a string)"), "{e}");
        assert!(e.contains("b (a string)"), "{e}");
    }

    #[test]
    fn an_empty_object_fails_rather_than_clearing_the_clipboard() {
        assert!(text_from_payload("{}").is_err());
    }

    /// An empty *string* is a real value and is not the same thing: somebody
    /// copying an empty selection gets an empty clipboard, which is correct.
    #[test]
    fn an_empty_string_is_a_value() {
        let e = text_from_payload(r#"{"content":""}"#).unwrap();
        assert_eq!(e.text, "");
    }

    #[test]
    fn not_json_at_all_says_so_without_quoting_it() {
        let e = text_from_payload("hunter2").unwrap_err();
        assert!(!e.contains("hunter2"), "the error quoted the payload: {e}");
        assert!(e.contains("7 bytes"), "{e}");
    }

    /// Nothing that reports on a payload may print what is in it.
    /// The GDK half, against a real compositor.
    ///
    /// Ignored by default and guarded by an environment variable on top of
    /// that, because it takes the display's clipboard — running it in a
    /// desktop session would throw away whatever the person at that desk had
    /// copied. Point `WAYLAND_DISPLAY` at a compositor nobody is using:
    ///
    /// ```text
    /// WAYLAND_DISPLAY=<nested> CORDIAL_CLIPBOARD_LIVE_TEST=1 \
    ///   cargo test -p cordial-runtime --release -- --ignored --test-threads=1 a_round_trip
    /// ```
    ///
    /// What it separates: `is_local` is GDK answering out of the provider this
    /// process installed, and a compositor that never took the selection would
    /// still give that answer. So a pass here is `set_text` and
    /// `read_text_async` agreeing, not proof the rest of the desktop can see
    /// it.
    #[test]
    #[ignore = "opens a window and takes the display's clipboard"]
    fn a_round_trip_through_gdk() {
        if std::env::var_os("CORDIAL_CLIPBOARD_LIVE_TEST").is_none() {
            panic!("refusing to take a clipboard without CORDIAL_CLIPBOARD_LIVE_TEST=1");
        }
        cordial_shell::host_window::init_wayland().expect("a Wayland compositor to talk to");
        let window = cordial_shell::host_window::HostWindow::with_canvas("clipboard probe", 320, 200);
        window.present();
        window.wait_until_mapped(std::time::Duration::from_secs(5)).expect("a mapped window");

        let clipboard = host_clipboard().expect("a display");
        set_host_text("round-trip-probe").expect("set_text");
        let read = host_text(std::time::Duration::from_secs(2));
        println!(
            "is_local={} read={:?}",
            clipboard.is_local(),
            read.as_ref().map(|s| s.len())
        );
        assert_eq!(read.expect("the clipboard to answer"), "round-trip-probe");

        // The rest of the chain, from where the engine's callback hands over.
        // `on_payload` is what `native/clipboard.cpp` calls and `pump_pending`
        // is what the looper calls, so driving those two in order covers every
        // step between the bus and the host clipboard without a running engine
        // — which matters because the compositor a running engine can be
        // pointed at is the developer's own.
        let payload = CString::new(r#"{"content":"published-probe"}"#).unwrap();
        on_payload(payload.as_ptr());
        pump_pending();
        assert_eq!(
            host_text(std::time::Duration::from_secs(2)).expect("the clipboard to answer"),
            "published-probe"
        );

        // `is_local` above is the whole limitation of an in-process check, so
        // `CORDIAL_CLIPBOARD_LIVE_HOLD=<secs>` keeps the window and the
        // selection alive afterwards for another client to try to read. That is
        // the only way to tell "GDK is answering itself" from "the compositor
        // handed it on", and it needs a second process, which a test cannot be.
        if let Some(secs) = std::env::var("CORDIAL_CLIPBOARD_LIVE_HOLD").ok().and_then(|s| s.parse().ok()) {
            let context = gtk4::glib::MainContext::default();
            let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            while std::time::Instant::now() < until {
                if !context.iteration(false) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }

    #[test]
    fn member_names_never_carry_values() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"a":"hunter2","b":"s3cret"}"#).unwrap();
        let names = member_names(v.as_object().unwrap());
        assert!(!names.contains("hunter2"), "{names}");
        assert!(!names.contains("s3cret"), "{names}");
        assert!(names.contains('a') && names.contains('b'), "{names}");
    }
}
