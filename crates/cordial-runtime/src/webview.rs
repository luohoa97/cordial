//! Roblox's in-experience web window, and the protocol the engine drives it with.
//!
//! This is what opens when somebody taps account settings, buys Robux, or
//! follows any other link the client keeps inside itself rather than handing to
//! a browser. With nobody answering, those buttons do nothing at all — no error,
//! no window, no log line. That is the `broken_feature` shape from AGENTS.md and
//! it is the largest one Cordial has.
//!
//! ## The protocol is the engine's, and it describes itself
//!
//! `libroblox.so` exports twenty-three natives on
//! `com/roblox/protocols/webview/WebViewProtocol`. Between them they carry the
//! whole vocabulary: the protocol's name, the identifiers of the messages the
//! engine will send, and the JSON keys inside those messages.
//!
//! **Every one of them is a getter, and that is the point.** The message the
//! engine sends to open a window carries a URL under some key, and that key is
//! whatever `getUrlKey()` returns *in the build being run*. Hardcoding `"url"`
//! because that is what it is probably called produces something that works
//! until Roblox renames it, and then fails silently — the window opens with no
//! address and nobody can tell why. Reading the vocabulary out of the engine
//! cannot drift, because it is the same constant the engine matches against.
//!
//! The machinery to do that already existed. `JNILinkingProtocol` — deep links —
//! is the same shape and `native/deeplink.cpp` already has a generic "static,
//! zero-argument native returning String" shim, parameterised by class name. So
//! this module is a caller of existing code rather than new plumbing, which is
//! also why it is small.
//!
//! ## What is established and what is not
//!
//! Established, by reading the engine's exports and the dex: the class, the
//! twenty-three natives, their signatures, and — corrected here, because an
//! earlier version of this comment said the transport was untraced and that is
//! no longer true — the transport itself. `docs/analysis/webview-surface.md`
//! §"The transport, found in mocktail's bridge rather than by tracing" names it:
//! `com.roblox.universalapp.messagebus.MessageBus`, the same bus
//! `native/deeplink.cpp` already speaks for deep links. Receiving `openWindow`
//! is three calls — `getMessageId("WebView", "openWindow")` to compose the bus
//! id, `doSubscribeRaw(id, callback, false)` to subscribe, and the callback's
//! `run(String)` handed the raw JSON when the engine publishes. The callback and
//! `Connection` classes that shape needs are already registered, by
//! `native/clipboard.cpp`'s `register_clipboard_classes` — this module must not
//! register them a second time, because a second registration of the same class
//! name overwrites the first and silently breaks the clipboard subscription
//! that got there first.
//!
//! **The two blockers that used to stop here are both cleared.** The first was
//! that no call shape `cordial-linker-sys` exported fit `getMessageId`'s
//! descriptor, `(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;` — two
//! `String`s in, one back out. `cordial_linker_sys::game_activity::
//! call_static_two_strings_ret_string` now exists for exactly that shape. The
//! second was that `native/clipboard.cpp`'s subscribe machinery held one
//! callback and one `Connection` in module-level globals, so a second
//! subscriber would not add a subscription, it would silently overwrite
//! clipboard's. That file's `cordial_messagebus_subscribe` is now keyed by
//! message id, in a map, so [`arm`] below and clipboard's own `arm` in
//! `crate::android::clipboard` each get a `Subscription` of their own. Neither
//! of those changes is this module's to make — this module is the first
//! caller of both, not the place either was fixed.
//!
//! [`find_bus_natives`] and [`report_bus_natives`] stay as an early
//! diagnostic: they run from `load.rs` before `nativeRetryInit`, while there is
//! not yet an app bridge for anything to subscribe against. [`arm`] does the
//! real work — `getMessageId`, then `doSubscribeRaw`, then a first read of
//! `Connection.isConnected` — and is called later, once the AGDK handle
//! exists, the same distinction clipboard's own `arm` draws against
//! `looper::pump`.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

/// The class the whole protocol hangs off.
pub const CLASS: &str = "com/roblox/protocols/webview/WebViewProtocol";

/// `com/roblox/universalapp/messagebus/MessageBus`, named once here for the
/// close-handshake calls below. `deeplink.rs` keeps its own copy of the same
/// literal (`BUS`) rather than importing this one, because that module
/// predates this one and the two have no dependency on each other worth
/// creating for one shared string.
const BUS: &str = "com/roblox/universalapp/messagebus/MessageBus";

/// The getters worth reading, by the native symbol that answers each.
///
/// Not every export is here: the two `getFFlag...` natives return `boolean`
/// rather than `String` and need a different call shape, and
/// `signalJavascriptCallback` takes arguments and is not a getter at all. Those
/// are left for when there is something to say with them, rather than called to
/// make a list look complete.
const STRING_GETTERS: &[(&str, &str)] = &[
    ("protocol", "getProtocolName"),
    // Message identifiers: what the engine will ask for.
    ("open-window", "getOpenWindowId"),
    ("close-window", "getCloseWindowId"),
    ("mutate-window", "getMutateWindowId"),
    ("handle-window-close", "getHandleWindowCloseId"),
    ("is-available", "getIsAvailableId"),
    // Payload keys: where to find things inside a message.
    ("key:url", "getUrlKey"),
    ("key:title", "getTitleKey"),
    ("key:window-type", "getWindowTypeKey"),
    ("key:search-params", "getSearchParamsKey"),
    ("key:search-type", "getSearchTypeKey"),
    ("key:is-visible", "getIsVisibleKey"),
    ("key:hide-header", "getHideHeaderKey"),
    ("key:back-button-visible", "getBackButtonVisibleKey"),
    ("key:show-domain-as-title", "getShowDomainAsTitleKey"),
    ("key:available", "getAvailableKey"),
];

/// A getter's answer, or why it could not be had.
pub struct Vocabulary {
    pub entries: Vec<(&'static str, String)>,
    pub missing: Vec<&'static str>,
}

/// Read the protocol's vocabulary out of the running engine.
///
/// `symbol` resolves a JNI native by name — the same lookup `load.rs` already
/// does for every other engine entry point. A getter that is not exported is
/// recorded rather than treated as fatal: Roblox adds and removes these between
/// builds, and one missing name is a fact worth printing, not a reason to
/// abandon the other fifteen.
pub fn read_vocabulary(mut symbol: impl FnMut(&str) -> Option<*mut c_void>) -> Vocabulary {
    let mut entries = Vec::new();
    let mut missing = Vec::new();
    for (label, getter) in STRING_GETTERS {
        let name = format!("Java_com_roblox_protocols_webview_WebViewProtocol_{getter}");
        match symbol(&name) {
            // SAFETY: `f` is a native resolved via a symbol lookup against the loaded libroblox.so, which is never unloaded.
            Some(f) => match unsafe { cordial_linker_sys::game_activity::call_static_ret_string(f, CLASS) } {
                Ok(v) => entries.push((*label, v)),
                // A getter that is exported and then fails is a different fact
                // from one that is absent, and conflating them would hide it.
                Err(e) => entries.push((*label, format!("<error: {e}>"))),
            },
            None => missing.push(*getter),
        }
    }
    Vocabulary { entries, missing }
}

/// Print what was read.
///
/// Verbose on purpose. This is the only record of what the protocol's names are
/// in a given build, and the next piece of work — receiving a message — cannot
/// start without it.
pub fn report(v: &Vocabulary) {
    if v.entries.is_empty() && v.missing.is_empty() {
        println!("  webview: WebViewProtocol is not exported by this build");
        return;
    }
    println!("  webview: protocol vocabulary, read from the engine");
    for (label, value) in &v.entries {
        println!("    {label:<26} {value}");
    }
    if !v.missing.is_empty() {
        println!("    not exported by this build: {}", v.missing.join(", "));
    }
}

/// The two `com/roblox/universalapp/messagebus/MessageBus` natives that
/// receiving `openWindow` needs, found or not, by the same symbol lookup
/// [`read_vocabulary`] already uses.
pub struct BusNatives {
    pub get_message_id: bool,
    pub do_subscribe_raw: bool,
}

/// Resolve `MessageBus.getMessageId` and `MessageBus.doSubscribeRaw`.
///
/// Finding a native and being able to call it are different facts —
/// `getMessageId` is found by every build that has ever been run against this
/// module, and it is still not called. See the module doc for why.
pub fn find_bus_natives(mut symbol: impl FnMut(&str) -> Option<*mut c_void>) -> BusNatives {
    BusNatives {
        get_message_id: symbol("Java_com_roblox_universalapp_messagebus_MessageBus_getMessageId")
            .is_some(),
        do_subscribe_raw: symbol(
            "Java_com_roblox_universalapp_messagebus_MessageBus_doSubscribeRaw",
        )
        .is_some(),
    }
}

/// Print what [`find_bus_natives`] found. This runs before `nativeRetryInit`,
/// while the app bridge that `doSubscribeRaw` needs to call into does not
/// exist yet — so finding both natives here is a fact worth logging, not
/// grounds to subscribe from this spot. [`arm`] is the call that actually
/// subscribes, made later once there is a bridge to subscribe against.
pub fn report_bus_natives(n: &BusNatives) {
    println!(
        "  webview: MessageBus.getMessageId exported: {}",
        n.get_message_id
    );
    println!(
        "  webview: MessageBus.doSubscribeRaw exported: {}",
        n.do_subscribe_raw
    );
    if n.get_message_id && n.do_subscribe_raw {
        println!("  webview: both natives are present; openWindow is subscribed later, from arm()");
    }
}

// -------------------------------------------------------------- subscribing
//
// `native/clipboard.cpp` carries the C++ half of this: `RawCallback` and
// `Connection` are classes it registers once for the whole process (never
// again here — see that file's header for the second-registration bug that
// already happened once), and `cordial_messagebus_subscribe` keeps one
// `Subscription` per message id in a map, so this subscription and
// clipboard's own live independently. Declared again here, rather than made
// `pub` in `crate::android::clipboard` and imported, because that file is off
// limits to edit for this change and an `extern "C"` block naming symbols the
// linker already provides is not a second definition of anything — both
// modules are just callers of the same three C++ functions.
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

// --------------------------------------------------------------- user agent
//
// `native/init_params.cpp`'s `build_user_agent` is what fills
// `InitParams.userAgent` -- the string the engine's own HTTP client sends.
// `cordial_build_user_agent` hands the identical bytes out to Rust, so the
// desktop web view (`cordial_shell::webview::open`) and
// `NativeGLInterface.setWebviewUserAgent` (`load.rs`'s call, beside the
// other `NativeGLInterface` calls) can present the same client the engine
// already told the server it was, rather than a second, Rust-side
// recomputation that could silently drift from it.

unsafe extern "C" {
    fn cordial_build_user_agent(buf: *mut c_char, n: usize) -> usize;
}

/// The exact `User-Agent` `InitParams.userAgent` was built with.
///
/// `None` only if the string somehow does not fit a buffer four times the
/// size of `build_user_agent`'s own 512-byte `snprintf` target, or is not
/// valid UTF-8 -- neither of which that function's fixed format string and
/// numeric/ASCII fields can produce. A real failure here is worth seeing
/// rather than silently falling back to no User-Agent at all, which is why
/// this returns `Option` instead of an empty string on error.
pub fn user_agent() -> Option<String> {
    let mut buf = vec![0u8; 2048];
    // SAFETY: `cordial_build_user_agent` is statically linked into this
    // binary via `cordial_jni_shim` (native/init_params.cpp); the buffer
    // outlives the call.
    let len = unsafe { cordial_build_user_agent(buf.as_mut_ptr() as *mut c_char, buf.len()) };
    if len == 0 || len >= buf.len() {
        return None;
    }
    std::str::from_utf8(&buf[..len]).ok().map(str::to_string)
}

// ------------------------------------------------------------- opening it
//
// Everything above this point receives the message and, until this change,
// only measured it. What follows turns the JSON into something
// `cordial_shell::webview::open` can act on, and names the one call site that
// is still missing — see [`set_presenter`].

/// The JSON keys this build's engine uses for an `openWindow` message.
///
/// Read from [`read_vocabulary`] rather than hardcoded, for the reason that
/// function's own doc gives: a literal `"url"` is a guess that holds until
/// Roblox renames the field, and then fails by opening a window with an
/// empty address and no explanation. [`OpenWindowKeys::from_vocabulary`] is
/// the only constructor, so a caller cannot assemble one from guessed
/// strings by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindowKeys {
    pub url: String,
    pub title: String,
    pub hide_header: String,
    pub back_button_visible: String,
    pub show_domain_as_title: String,
}

impl OpenWindowKeys {
    /// Pull the five keys `openWindow` needs out of a vocabulary already read
    /// from the running engine. `Err` names every getter that was missing
    /// rather than the first one, because a build missing several of these is
    /// a different fact from a build missing one, and the difference is worth
    /// seeing in a single log line rather than one `arm()` retry at a time.
    pub fn from_vocabulary(v: &Vocabulary) -> Result<Self, Vec<&'static str>> {
        let get = |label: &'static str| {
            v.entries.iter().find(|(l, _)| *l == label).map(|(_, value)| value.clone())
        };
        let fields: [(&'static str, Option<String>); 5] = [
            ("key:url", get("key:url")),
            ("key:title", get("key:title")),
            ("key:hide-header", get("key:hide-header")),
            ("key:back-button-visible", get("key:back-button-visible")),
            ("key:show-domain-as-title", get("key:show-domain-as-title")),
        ];
        let missing: Vec<&'static str> =
            fields.iter().filter(|(_, v)| v.is_none()).map(|(label, _)| *label).collect();
        if !missing.is_empty() {
            return Err(missing);
        }
        Ok(OpenWindowKeys {
            url: fields[0].1.clone().expect("checked above"),
            title: fields[1].1.clone().expect("checked above"),
            hide_header: fields[2].1.clone().expect("checked above"),
            back_button_visible: fields[3].1.clone().expect("checked above"),
            show_domain_as_title: fields[4].1.clone().expect("checked above"),
        })
    }
}

/// An `openWindow` request, parsed and ready to become a
/// `cordial_shell::webview::WindowRequest` — kept as this crate's own type
/// rather than that one directly, so [`parse_open_window`] and its tests
/// compile without `webkitgtk6.0-devel` even when the `webview` feature is
/// off (see `Cargo.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindowRequest {
    pub url: String,
    pub title: Option<String>,
    pub hide_header: bool,
    pub back_button_visible: bool,
    pub show_domain_as_title: bool,
}

/// Parse one `openWindow` payload against this build's own key names.
///
/// `url` is the one field a window cannot open without, so a payload missing
/// it — or carrying it empty — is refused outright rather than opening a
/// blank window that says nothing about why. The three booleans default to
/// `false` when absent, which is [`cordial_shell::webview::WindowRequest`]'s
/// own conservative default (see that type's test): a flag the engine did not
/// send is a flag Cordial should not invent an opinion about.
pub fn parse_open_window(json: &str, keys: &OpenWindowKeys) -> Result<OpenWindowRequest, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("openWindow payload is not JSON: {e}"))?;
    let object = value.as_object().ok_or_else(|| "openWindow payload is not a JSON object".to_string())?;
    let url = object
        .get(&keys.url)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("openWindow payload has no non-empty {:?} field", keys.url))?;
    let flag = |key: &str| object.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    Ok(OpenWindowRequest {
        url: url.to_string(),
        title: object.get(&keys.title).and_then(|v| v.as_str()).map(str::to_string),
        hide_header: flag(&keys.hide_header),
        back_button_visible: flag(&keys.back_button_visible),
        show_domain_as_title: flag(&keys.show_domain_as_title),
    })
}

/// The largest `.ROBLOSECURITY` value this window will accept.
///
/// Cordial's own bound, not mocktail's copied — `webview_roblox_cookie.h`,
/// which would declare their equivalent constant, is not among the vendored
/// files (`third_party/mocktail-webview/README.md` lists exactly five `.cc`
/// files and one `.h`, and it is not this one), so only their reasoning
/// transfers: a bearer token about to become part of a `Set-Cookie` header
/// needs a ceiling before an oversized one is a bug rather than a feature.
pub const MAX_ROBLOSECURITY_COOKIE_BYTES: usize = 4096;

/// Whether `byte` is an RFC 6265 `cookie-octet`.
fn is_cookie_octet(byte: u8) -> bool {
    byte == 0x21
        || (0x23..=0x2b).contains(&byte)
        || (0x2d..=0x3a).contains(&byte)
        || (0x3c..=0x5b).contains(&byte)
        || (0x5d..=0x7e).contains(&byte)
}

/// Pull `.ROBLOSECURITY`'s value out of one jar.
///
/// Derived from mocktail's `webview_roblox_cookie.cc`
/// (`PrepareWebViewRobloxCookie`), Copyright 2026 komaruworld, Apache-2.0 —
/// see `NOTICE` and `third_party/mocktail-webview/`. Rewritten against
/// Cordial's own store rather than translated: mocktail strips a fixed
/// `.ROBLOSECURITY=` prefix off one canonical header read from Android's
/// credential manager, while `crate::cookies::load` hands back a `Jar` whose
/// `expose()`d form is `name=value; name2=value2` for a whole host (see that
/// module's `to_settable`), so this scans for the pair instead of a prefix.
/// What is kept unmodified is the reason for the character-class check: the
/// value is headed for a `Set-Cookie` header WebKit will send over the wire,
/// and a byte outside `cookie-octet` there is a header-injection surface, not
/// a display bug.
pub fn extract_roblosecurity(jar: &str) -> Result<String, &'static str> {
    for pair in jar.split(';') {
        let Some((name, value)) = pair.trim().split_once('=') else { continue };
        if name != ".ROBLOSECURITY" {
            continue;
        }
        if value.is_empty() {
            return Err(".ROBLOSECURITY is present but empty");
        }
        if value.len() > MAX_ROBLOSECURITY_COOKIE_BYTES {
            return Err(".ROBLOSECURITY is larger than this window will accept");
        }
        if !value.bytes().all(is_cookie_octet) {
            return Err(".ROBLOSECURITY contains a byte outside cookie-octet");
        }
        return Ok(value.to_string());
    }
    Err("no .ROBLOSECURITY pair in this jar")
}

/// The signed-in session's `.ROBLOSECURITY`, for the profile stored at `dir`,
/// or `None` for "signed out" and "the store could not be read" alike — both
/// mean the window opens without a session, the same conservative default
/// [`crate::cookies::load`] already uses when there is nothing to restore.
///
/// Deliberately not called from [`on_open_window`]. That callback runs on
/// whichever thread the engine published `openWindow` from, which this crate
/// has never established is safe to block on a Secret Service round trip
/// (`secrets.rs`'s own `CALL_TIMEOUT` is five seconds) — reading a stored
/// cookie belongs at the point something is about to call
/// `cordial_shell::webview::open`, on whatever thread that turns out to be,
/// not folded into message receipt on a thread this module does not own.
///
/// Never logs the value — only [`extract_roblosecurity`]'s error strings,
/// which name a reason and never a byte of the cookie, are fit to print, the
/// same discipline `secrets.rs` and `cookies.rs` already hold themselves to.
pub fn roblox_session_cookie(dir: &std::path::Path) -> Option<String> {
    let jars = crate::cookies::load(dir);
    let (_, jar) = jars.iter().find(|(host, _)| host == "roblox.com" || host == ".roblox.com")?;
    match extract_roblosecurity(jar.expose()) {
        Ok(value) => Some(value),
        Err(reason) => {
            println!("  webview: stored session is unusable for the web window: {reason}");
            None
        }
    }
}

/// What actually presents a parsed request as a window, installed once by
/// whoever owns a live `cordial_shell::host_window::HostWindow` to attach it
/// to.
///
/// **Corrected**: this doc used to say "nothing in this crate installs one"
/// and described the single missing call as future work. That is no longer
/// true and left uncorrected would be exactly the kind of stale comment
/// AGENTS.md asks to be fixed rather than tolerated. `load.rs`'s
/// `install_webview_presenter` is that call now, made right after [`arm`],
/// from the same GTK-owning thread `arm` is called from: it installs a
/// closure that re-enters `glib::MainContext::default().invoke` (not a
/// channel, which was the shape this doc used to propose, but the same
/// "marshal onto the GTK thread" effect) and, once there, reads
/// `android::wayland::current()` for the live `HostWindow`, fetches a session
/// cookie via [`roblox_session_cookie`], and calls
/// `cordial_shell::webview::open`. What was never established until a real
/// run proved it, and is the reason [`dev_trigger_open_window`] exists: an
/// `openWindow` message from the engine needs a click nobody could produce
/// without one, so this path had never actually run end to end.
type Presenter = dyn Fn(OpenWindowRequest) + Send + Sync;
static PRESENTER: OnceLock<std::sync::Arc<Presenter>> = OnceLock::new();

/// Install the presenter [`on_open_window`] hands parsed requests to. See
/// that type's doc for what a presenter has to do and why nothing here
/// installs one. Only the first call takes effect — later ones are reported,
/// not silently dropped, because two presenters racing to open the same
/// request is a bug worth seeing rather than one that quietly resolves itself
/// in whichever order closures happened to run.
pub fn set_presenter(f: impl Fn(OpenWindowRequest) + Send + Sync + 'static) {
    if PRESENTER.set(std::sync::Arc::new(f)).is_err() {
        println!("  webview: set_presenter called twice; the first presenter installed is the one in effect");
    }
}

/// The presenter's close counterpart: what actually makes the currently open
/// window go away when something other than a click on it decided it should.
/// Installed once, from `load.rs`, the same shape and for the same reason as
/// [`set_presenter`] -- this module knows the engine's message bus, and
/// nothing here can safely touch a `gtk4`/`libadwaita` object, which is why
/// closing goes through a callback into `cordial_shell::webview::
/// close_current` rather than this module reaching for one directly.
type CloseHandler = dyn Fn() + Send + Sync;
static CLOSE_HANDLER: OnceLock<std::sync::Arc<CloseHandler>> = OnceLock::new();

/// Install the handler [`on_close_window`] calls once the engine has actually
/// published on `close-window`'s bus id. Only the first call takes effect,
/// the same discipline [`set_presenter`] holds itself to, for the same
/// reason: two handlers racing to close (or fail to close) the same window is
/// a bug worth seeing, not one that resolves itself.
pub fn set_close_handler(f: impl Fn() + Send + Sync + 'static) {
    if CLOSE_HANDLER.set(std::sync::Arc::new(f)).is_err() {
        println!("  webview: set_close_handler called twice; keeping the first handler installed");
    }
}

/// The sink `cordial_messagebus_subscribe` calls when the engine publishes on
/// `close-window`'s bus id -- `getCloseWindowId`, read but never subscribed
/// to before this change. `STRING_GETTERS`'s own comment grouped it with
/// `getOpenWindowId` and `getMutateWindowId` as one of three parallel
/// host-facing commands and left it untried.
///
/// **Measured, not still open: the engine does not publish on this for a
/// join.** Live on 2026-09-24, this subscription resolved cleanly (`arm()`'s
/// log shows `subscribed to WebView.closeWindow`), and a real signed-in join
/// from the Servers list -- a public Brookhaven server, clicked, the
/// `RequestGameJob` bridge payload arriving and `Game.launch` publishing
/// correctly, the game visibly running behind the still-open dialog in a
/// `grim` capture -- never once reached this callback. That is why the web
/// window staying open is fixed in [`route_as_hybrid_launch`] instead, by
/// closing once a join is actually published, rather than here. This
/// subscription is kept rather than removed: `close-window` may still be
/// what the engine uses for a window it opened and wants closed for some
/// other reason (a completed sign-in, a finished purchase) that this session
/// had no signed-out account or spare Robux to exercise, and answering it
/// costs nothing if it never fires.
///
/// The payload is not parsed -- [`report_window_closed`]'s own doc gives the
/// reason a close message has nothing worth reading out of it (no window
/// identifier exists in this protocol's vocabulary, and Cordial never has
/// more than one window open), and the same reasoning applies to the message
/// arriving in this direction.
extern "C" fn on_close_window(_json: *const c_char) {
    println!("[webview] closeWindow message arrived from the engine; closing the open web window");
    match CLOSE_HANDLER.get() {
        Some(close) => close(),
        None => println!(
            "[webview] no close handler installed (arm() ran before install_webview_presenter, \
             or the `webview` feature is off); the window the engine asked to close will stay open"
        ),
    }
}

/// Synthesise the one message nobody could produce by clicking: drive the
/// installed presenter directly with `url`, bypassing the engine, the message
/// bus and `parse_open_window` entirely.
///
/// **Why this exists at all.** `openWindow` needs a real click in signed-in
/// UI — the server browser, account settings, a purchase flow — and this
/// project's own rule against synthesising input at the compositor (AGENTS.md
/// "Two practical cautions") rules out faking that click from outside
/// Cordial. It does not rule out calling Cordial's own code path directly,
/// the same distinction that section draws for `input::pass_key_event`: this
/// is that, aimed at the presenter instead of the input queue. Called from
/// exactly one place, `load.rs`, guarded by the `CORDIAL_WEBVIEW_TEST`
/// environment variable — see that call site's own comment for why it must
/// stay off by default and out of the ordinary path.
///
/// **What is bypassed and what is not.** `parse_open_window` and its key
/// vocabulary are skipped, because there is no real JSON payload to parse —
/// the caller already has a URL as a Rust `String`. What is *not* skipped is
/// everything downstream of a real parse: the presenter this hands off to is
/// the identical closure `install_webview_presenter` installed, so it re-enters
/// the GTK thread, re-fetches the profile's session cookie, and calls
/// `cordial_shell::webview::open`, which runs the full
/// [`crate::android::wayland`]-independent [`webview_policy::evaluate`] check
/// this crate depends on `cordial_shell` for. A refused URL is refused here
/// exactly as it would be for a real message; this function cannot open
/// anything the policy would not have allowed a real click to open.
///
/// `title`, `back_button_visible` and `show_domain_as_title` are set rather
/// than left at the parser's `false` default so the window this produces is
/// visibly a synthesised one in a screenshot — a plain header with no domain
/// shown would be indistinguishable from a real request that simply asked for
/// that chrome, and the whole point of a diagnostic is that it not be mistaken
/// for the thing it is standing in for.
pub fn dev_trigger_open_window(url: String) {
    match PRESENTER.get() {
        Some(present) => present(synthetic_open_window_request(url)),
        None => println!(
            "[webview] CORDIAL_WEBVIEW_TEST set, but no presenter is installed yet; nothing to \
             drive"
        ),
    }
}

/// The request [`dev_trigger_open_window`] hands to the presenter. Split out
/// so the shape can be pinned by a test without touching the process-global
/// `PRESENTER` — see `set_presenter_keeps_the_first_installation`'s own
/// comment for why that static is exercised by exactly one test.
fn synthetic_open_window_request(url: String) -> OpenWindowRequest {
    OpenWindowRequest {
        url,
        title: Some("CORDIAL_WEBVIEW_TEST".to_string()),
        hide_header: false,
        back_button_visible: true,
        show_domain_as_title: true,
    }
}

static KEYS: OnceLock<OpenWindowKeys> = OnceLock::new();

static CONNECTION: AtomicI64 = AtomicI64::new(0);
static IS_CONNECTED_NATIVE: OnceLock<usize> = OnceLock::new();

/// The bus id [`report_window_closed`] publishes on, and the resolved
/// `MessageBus.publishRaw` native to publish it with — both filled by
/// [`arm`], both empty in a build that could not arm the close report. See
/// [`report_window_closed`]'s own doc for why a window closing without
/// either of these set must be loud rather than silent.
static CLOSE_BUS_ID: OnceLock<String> = OnceLock::new();
static PUBLISH_RAW_NATIVE: OnceLock<usize> = OnceLock::new();

/// The resolved `WebViewProtocol.signalJavascriptCallback` native, filled by
/// [`arm`]. See [`forward_bridge_message`] for what calling it does and does
/// not establish.
static SIGNAL_JAVASCRIPT_CALLBACK_NATIVE: OnceLock<usize> = OnceLock::new();

/// `CORDIAL_TRACE_BRIDGE=1` -- print a bridge command's contents rather than
/// just its byte length.
///
/// Written for issue #40: the one fact missing from that whole thread was
/// what the Servers page's Join button actually sends, and a length is not
/// enough to answer it. Off by default for the same reason
/// `CORDIAL_TRACE_TEXT_SHOW_PASSWORDS` is -- a bridge command is exactly as
/// likely to carry a private-server `accessCode` as a text box is to carry a
/// password, and `forward_bridge_message`'s own doc already makes that
/// argument for this direction.
pub fn trace_bridge() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_TRACE_BRIDGE").is_some())
}

/// The sink `cordial_messagebus_subscribe` calls when the engine publishes
/// `openWindow`.
///
/// Runs on whichever thread the engine published from, same as clipboard's
/// `on_payload`. Parsing is safe there — it touches no GTK object, only
/// `serde_json` — but presenting the result is not, which is why the parsed
/// request is handed to whatever [`set_presenter`] installed rather than
/// acted on directly here.
///
/// **The URL itself is never printed**, only what
/// `cordial_shell::webview_policy::evaluate` says about it — a scheme and a
/// host — because a Roblox web-view URL can carry a one-time authentication
/// ticket in its query string (`docs/analysis/webview-surface.md` §4's rule,
/// applied here). That module needs no `webkitgtk6.0-devel` and is always
/// compiled, so this call does not need the `webview` feature either.
extern "C" fn on_open_window(json: *const c_char) {
    // SAFETY: the C side passes a NUL-terminated string that outlives the
    // call, which is exactly `borrow_request`'s contract.
    let Some(request) = (unsafe { crate::ffi_util::borrow_request(json) }) else {
        println!("[webview] openWindow published with a null payload");
        return;
    };
    let bytes = request.to_bytes();
    let Ok(json) = std::str::from_utf8(bytes) else {
        println!("[webview] openWindow message arrived: {} bytes, not valid UTF-8", bytes.len());
        return;
    };
    let Some(keys) = KEYS.get() else {
        println!(
            "[webview] openWindow message arrived: {} bytes, but this build's key vocabulary was \
             never read (arm() must run first, and must find getUrlKey and friends)",
            bytes.len()
        );
        return;
    };
    match parse_open_window(json, keys) {
        Ok(request) => {
            let verdict = cordial_shell::webview_policy::evaluate(&request.url);
            println!(
                "[webview] openWindow message arrived: {} bytes, host {} (scheme {}), privileged \
                 {}, hideHeader {}, backButtonVisible {}, showDomainAsTitle {}",
                bytes.len(),
                verdict.host,
                verdict.scheme,
                verdict.privileged_bridge_allowed,
                request.hide_header,
                request.back_button_visible,
                request.show_domain_as_title,
            );
            match PRESENTER.get() {
                Some(present) => present(request),
                None => println!(
                    "[webview] no presenter installed; nothing will render this window. See \
                     `set_presenter`'s doc in this module for the one remaining call site."
                ),
            }
        }
        Err(reason) => {
            println!("[webview] openWindow message arrived: {} bytes, but could not be parsed: {reason}", bytes.len());
        }
    }
}

/// Subscribe to the engine's `openWindow` message.
///
/// Three calls, each depending on the last having worked: `getMessageId`
/// composes the bus id the way the engine itself would look it up rather than
/// guessing at a constant that changes between builds (see the module doc on
/// [`read_vocabulary`]); `doSubscribeRaw` installs [`on_open_window`] through
/// the callback class `native/clipboard.cpp`'s `register_clipboard_classes`
/// already registered; `Connection.isConnected` is the only honest way to say
/// the subscription is live rather than merely attempted without throwing.
///
/// `symbol` is the same JNI-native lookup `load.rs` already threads through
/// [`read_vocabulary`] and [`find_bus_natives`]. Call this once the AGDK
/// handle exists and the app bridge has started — the same point
/// `android::clipboard::arm` is called from, for the same reason: the bus has
/// to exist before anything can subscribe to it, and this module cannot reach
/// `looper::pump` to add itself there, so `load.rs` calls this directly,
/// right before it hands control to that pump.
///
/// **Also arms the other direction.** Beyond subscribing to `openWindow`,
/// this now also resolves what [`report_window_closed`] needs to tell the
/// engine a window closed — see that function's doc for why
/// `android::wayland`'s subsurface-lowering fix, on its own, leaves the
/// engine's canvas blank after every web window: it makes the dialog visible
/// and makes the canvas visible again, but nothing was ever telling the
/// engine the window it blanked itself for is gone.
pub fn arm(mut symbol: impl FnMut(&str) -> Option<*mut c_void>) {
    // Called first and unconditionally, because it is a lifecycle call on
    // this protocol rather than a step in either direction below. The
    // zero-argument shape is not a guess: the dex's own `method_ids`/
    // `proto_ids` tables declare `initializeAndroidWebViewProtocol()V` —
    // read directly out of the shipping APK the way `docs/analysis/
    // webview-surface.md` reads every other prototype in this file, not
    // inferred from the naming convention the way an earlier version of this
    // comment had to. **Whether calling it is required is still not
    // established** — `openWindow` subscribes and fires without this ever
    // having been called (confirmed live, screenshot in hand), which is
    // evidence it is not a precondition for that half of the protocol. It is
    // attempted anyway because the maintainer's report named it as the first
    // thing to rule out for the half that is broken, and now that the shape
    // is confirmed rather than assumed, there is no argument-list risk left
    // in trying it.
    match symbol(
        "Java_com_roblox_protocols_webview_WebViewProtocol_initializeAndroidWebViewProtocol",
    ) {
        // SAFETY: `f` is a native resolved via a symbol lookup against the loaded libroblox.so, which is never unloaded.
        Some(f) => match unsafe { cordial_linker_sys::game_activity::protocol_init(f, CLASS) } {
            Ok(()) => println!("  webview: initializeAndroidWebViewProtocol ok"),
            Err(e) => println!("  webview: initializeAndroidWebViewProtocol failed: {e}"),
        },
        None => println!(
            "  webview: initializeAndroidWebViewProtocol is not exported by this build"
        ),
    }

    // `signalJavascriptCallback` -- resolved here, alongside the protocol's
    // other direct (non-message-bus) native, and stored for
    // [`forward_bridge_message`] to use once a real bridge message arrives.
    // See that function's own doc for the signature this was checked against
    // and what calling it does and does not establish.
    match symbol("Java_com_roblox_protocols_webview_WebViewProtocol_signalJavascriptCallback") {
        Some(f) => {
            let _ = SIGNAL_JAVASCRIPT_CALLBACK_NATIVE.set(f as usize);
            println!("  webview: signalJavascriptCallback resolved; bridge messages can be forwarded");
        }
        None => println!(
            "  webview: signalJavascriptCallback is not exported by this build; bridge messages \
             will not be forwarded to the engine"
        ),
    }

    let vocabulary = read_vocabulary(&mut symbol);
    match OpenWindowKeys::from_vocabulary(&vocabulary) {
        Ok(keys) => {
            if KEYS.set(keys).is_err() {
                println!("  webview: arm() called twice; keeping the first build's key vocabulary");
            }
        }
        Err(missing) => println!(
            "  webview: this build is missing {} of openWindow's key getters ({}); a message will \
             arrive and cannot be parsed until they are",
            missing.len(),
            missing.join(", ")
        ),
    }
    let get = |label: &str| {
        vocabulary
            .entries
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, v)| v.clone())
    };
    let (Some(protocol), Some(open_window_id)) = (get("protocol"), get("open-window")) else {
        println!(
            "  webview: cannot subscribe to openWindow without getProtocolName and \
             getOpenWindowId (see the vocabulary reported above)"
        );
        return;
    };

    let Some(get_message_id) =
        symbol("Java_com_roblox_universalapp_messagebus_MessageBus_getMessageId")
    else {
        println!("  webview: MessageBus.getMessageId is not exported; not subscribing");
        return;
    };
    // SAFETY: `get_message_id` is a native resolved via a symbol lookup against the loaded libroblox.so, which is never unloaded.
    let bus_id = match unsafe { cordial_linker_sys::game_activity::call_static_two_strings_ret_string(
        get_message_id,
        BUS,
        &protocol,
        &open_window_id,
    ) } {
        Ok(id) => id,
        Err(e) => {
            println!("  webview: getMessageId({protocol:?}, {open_window_id:?}) failed: {e}");
            return;
        }
    };

    let Some(do_subscribe_raw) =
        symbol("Java_com_roblox_universalapp_messagebus_MessageBus_doSubscribeRaw")
    else {
        println!(
            "  webview: MessageBus.doSubscribeRaw is not exported; openWindow will do nothing"
        );
        return;
    };
    let Ok(id) = CString::new(bus_id.clone()) else {
        println!("  webview: the bus id getMessageId returned has a NUL in it: {bus_id:?}");
        return;
    };
    let mut err = vec![0u8; 512];
    // SAFETY: `do_subscribe_raw` resolved under its own name, so it is the
    // native `cordial_messagebus_subscribe` expects; every buffer outlives
    // the call.
    let rc = unsafe {
        cordial_messagebus_subscribe(
            do_subscribe_raw,
            id.as_ptr(),
            Some(on_open_window),
            err.as_mut_ptr() as *mut c_char,
            err.len(),
        )
    };
    if rc != 0 {
        println!("  webview: doSubscribeRaw failed: {}", take_err(err));
        return;
    }
    // SAFETY: reads a value the subscribe call above stored, keyed by the
    // same message id.
    let ptr = unsafe { cordial_messagebus_connection_ptr(id.as_ptr()) };
    CONNECTION.store(ptr, Ordering::Relaxed);
    if let Some(f) = symbol("Java_com_roblox_universalapp_messagebus_Connection_isConnected") {
        let _ = IS_CONNECTED_NATIVE.set(f as usize);
    }
    match connected() {
        Some(true) => println!(
            "  webview: subscribed to {bus_id} ({protocol}.{open_window_id}); the bus says it \
             is live"
        ),
        Some(false) => println!(
            "  webview: subscribed to {bus_id}, but the bus says the connection is not live"
        ),
        None => println!(
            "  webview: subscribed to {bus_id}; nothing here can confirm it (no Connection came \
             back)"
        ),
    }

    // The other host-facing command in the same trio as `openWindow`: the
    // engine asking Cordial to close a window it opened. Best-effort and
    // deliberately not a reason to bail the rest of `arm` out -- a build
    // missing `getCloseWindowId`, or one where this particular subscribe
    // fails, still leaves `openWindow` working, which matters more.
    match get("close-window") {
        Some(close_window_id) => {
            // SAFETY: `get_message_id` is a native resolved via a symbol lookup against the loaded libroblox.so, which is never unloaded.
            match unsafe {
                cordial_linker_sys::game_activity::call_static_two_strings_ret_string(
                    get_message_id,
                    BUS,
                    &protocol,
                    &close_window_id,
                )
            } {
                Ok(close_window_bus_id) => match CString::new(close_window_bus_id.clone()) {
                    Ok(id) => {
                        let mut err = vec![0u8; 512];
                        // SAFETY: `do_subscribe_raw` resolved under its own name above, and used the same way as the `openWindow` subscription; every buffer outlives the call.
                        let rc = unsafe {
                            cordial_messagebus_subscribe(
                                do_subscribe_raw,
                                id.as_ptr(),
                                Some(on_close_window),
                                err.as_mut_ptr() as *mut c_char,
                                err.len(),
                            )
                        };
                        if rc == 0 {
                            println!(
                                "  webview: subscribed to {close_window_bus_id} \
                                 ({protocol}.{close_window_id}); a closeWindow message will now \
                                 close the open window"
                            );
                        } else {
                            println!(
                                "  webview: subscribing to {close_window_bus_id} failed: {}",
                                take_err(err)
                            );
                        }
                    }
                    Err(_) => println!(
                        "  webview: the close-window bus id getMessageId returned has a NUL in \
                         it: {close_window_bus_id:?}"
                    ),
                },
                Err(e) => println!(
                    "  webview: getMessageId({protocol:?}, {close_window_id:?}) failed: {e}"
                ),
            }
        }
        None => println!(
            "  webview: no getCloseWindowId in this build's vocabulary; a closeWindow message \
             cannot be subscribed to"
        ),
    }

    // The close report. `getMessageId` and `protocol` are the same values
    // just used for `openWindow`'s bus id -- this is the same class asked
    // for a different method id, not a second lookup of anything already
    // established.
    //
    // **`getHandleWindowCloseId` is the id used here, and that choice is
    // `INFERRED`.** The dex does not label which of `getCloseWindowId` and
    // `getHandleWindowCloseId` is which direction, and nothing in this
    // session observed either one fire. The reasoning: `get{Open,Close,
    // Mutate}WindowId` are grouped together, both in `libroblox.so`'s own
    // export order (`docs/analysis/webview-surface.md` §3c) and in
    // `STRING_GETTERS` above -- three parallel *commands*, which reads
    // naturally as engine-to-host, the direction `getOpenWindowId` already
    // has confirmed. `getHandleWindowCloseId` sits apart from that group,
    // beside `getIsAvailableId`, and "handle" names the thing that consumes
    // a report rather than the thing that issues a command -- the same shape
    // `onDataModelNotificationCallback` and `onAppBridgeNotification` have,
    // both named for handling something that already happened. If a live run
    // shows the canvas still blank after this publishes, that reading is
    // wrong and `getCloseWindowId` — which this file does not otherwise use;
    // by the same grouping argument it is likely the engine asking the host
    // to close a window, a distinct feature from this one — is the next
    // thing to try.
    let Some(handle_close_id) = get("handle-window-close") else {
        println!(
            "  webview: no getHandleWindowCloseId in this build's vocabulary; a closed window \
             will not be reported to the engine"
        );
        return;
    };
    // SAFETY: `get_message_id` is a native resolved via a symbol lookup against the loaded libroblox.so, which is never unloaded.
    let close_bus_id = match unsafe { cordial_linker_sys::game_activity::call_static_two_strings_ret_string(
        get_message_id,
        BUS,
        &protocol,
        &handle_close_id,
    ) } {
        Ok(id) => id,
        Err(e) => {
            println!("  webview: getMessageId({protocol:?}, {handle_close_id:?}) failed: {e}");
            return;
        }
    };
    let Some(publish_raw) = symbol("Java_com_roblox_universalapp_messagebus_MessageBus_publishRaw")
    else {
        println!(
            "  webview: MessageBus.publishRaw is not exported; a closed window will not be \
             reported to the engine"
        );
        return;
    };
    if CLOSE_BUS_ID.set(close_bus_id.clone()).is_err() {
        println!("  webview: arm() called twice; keeping the first build's close-report bus id");
    }
    let _ = PUBLISH_RAW_NATIVE.set(publish_raw as usize);
    println!(
        "  webview: armed to report a closed window on {close_bus_id} ({protocol}.{handle_close_id})"
    );
}

/// Tell the engine a web window closed.
///
/// The missing half of the close handshake reported live: `android::wayland`'s
/// `webview_dialog_opened`/`webview_dialog_closed` (called from `load.rs`'s
/// `install_webview_presenter`, in the dialog's own `connect_closed`) lowers
/// the engine's Wayland subsurface while a dialog is open and raises it again
/// once the dialog is gone — which makes the *stacking* right, but the engine
/// blanks its own drawing when a window opens expecting to be covered, and
/// raising a subsurface back above blank content is still blank content.
/// Nothing was telling the engine the window it blanked itself for had gone
/// away. This is that: a publish on [`arm`]'s cached close-report bus id, the
/// same `MessageBus.publishRaw` mechanism `deeplink.rs`'s `publish_url`
/// already uses for `Game.launch` — see that function's own doc for why the
/// engine reading this bus at all is established rather than assumed.
///
/// The payload is `{}`. Nothing in this protocol's key vocabulary names a
/// window identifier (`STRING_GETTERS` has no `getWindowIdKey`), and Cordial
/// opens at most one web window at a time, so there is nothing else honest to
/// put in it — inventing a key nobody asked for would be the same mistake
/// `nativeReportReceived`'s unused second argument is a warning about.
///
/// A loud no-op if [`arm`] never got this far — built without
/// `getHandleWindowCloseId`, `publishRaw` unexported, or `arm` itself never
/// run. Silence here would read as "the report was sent" to whoever is
/// reading the log next to a canvas that is still blank.
pub fn report_window_closed() {
    let (Some(bus_id), Some(publish_raw)) = (CLOSE_BUS_ID.get(), PUBLISH_RAW_NATIVE.get()) else {
        println!(
            "[webview] a window closed, but this build never armed a handle-window-close report \
             (see arm()'s log above); the engine will not be told"
        );
        return;
    };
    let f = *publish_raw as *mut c_void;
    // SAFETY: `publish_raw` was resolved under its own symbol name in `arm`
    // and never used for anything else; `call_static_strings` owns its own
    // buffers for the call.
    match unsafe { cordial_linker_sys::game_activity::call_static_strings(f, BUS, &[bus_id.as_str(), "{}"]) } {
        Ok(()) => println!("[webview] reported window closed on {bus_id}"),
        Err(e) => println!("[webview] reporting window closed on {bus_id} failed: {e}"),
    }
}

/// The runtime half of the default fix for issue #40/#34, on unless
/// `CORDIAL_WEBVIEW_DISABLE_HYBRID_LAUNCH` is set --
/// `cordial_shell::webview`'s own doc on `hybrid_launch_enabled` describes it
/// in full. The shell's injected script wraps `Roblox.Hybrid.Game.launchGame`
/// and posts its JSON payload through `executeRoblox` exactly as any other
/// bridge message; this is what recognises that specific payload on arrival
/// here and answers it differently. Named identically to the shell's own
/// escape hatch so one setting turns the whole feature off on both ends,
/// rather than leaving the shell intercepting a call this half then ignores.
pub fn hybrid_launch_enabled() -> bool {
    std::env::var_os("CORDIAL_WEBVIEW_DISABLE_HYBRID_LAUNCH").is_none()
}

/// Recognises a live `Roblox.Hybrid.Game.launchGame` interception inside an
/// ordinary `executeRoblox` message, by its `requestType` -- the one field
/// round 6 of issue #40 confirmed is always present and always
/// `"RequestGameJob"` for this call, on two different games. Returns whether
/// the message was claimed: `true` means this function decided what to do
/// with it and [`forward_bridge_message`] must not also forward it to
/// `signalJavascriptCallback`, which expects the engine's own "Hybrid
/// Module" JSON-RPC envelope (`moduleID`/`functionName`) -- a shape this
/// project has never confirmed and this payload does not match, so
/// forwarding it there too would not do anything except make the log read as
/// if two things happened when one did.
///
/// Every field below is read with `and_then(|v| v.as_str())`, so a field
/// present but not a JSON string is treated the same as one absent, rather
/// than passed on as some other type `crate::deeplink::publish_hybrid_game_launch`
/// was never written to expect.
///
/// **Also closes the web window, once the publish actually succeeds.** Live
/// on 2026-09-24: joining from the Servers list, with a subscription to
/// `close-window` (`WebView.closeWindow`, confirmed both exported and
/// subscribable — see `arm`'s log) armed and watching, never once saw the
/// engine publish on it, across a real click on a real public server that
/// joined correctly (screenshot and log both showed the dialog still open,
/// Join buttons still live, with the game already running behind it). So
/// Sober closing the window and Cordial not is not a message Cordial was
/// failing to answer; nothing arrives to answer. This is the fallback the
/// maintainer asked for: the earliest honest signal available, closing only
/// once [`crate::deeplink::publish_hybrid_game_launch`] has actually put the
/// join on the bus, not merely once this function decided to claim the
/// message -- a failed publish leaves nothing happening, and closing the
/// window over that would strand the player looking at a blank canvas with
/// no way back to the server list they were just on.
fn route_as_hybrid_launch(message: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(message) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    // `RequestGameJob` is a specific public server; a private server's Join
    // sends `RequestPrivateGame` with its codes instead, and was left unclaimed
    // here until 2026-09-25, so it went to `signalJavascriptCallback` and did
    // nothing. The request names match mocktail's `roblox_launch_uri.cc`.
    if !matches!(
        object.get("requestType").and_then(|v| v.as_str()),
        Some("RequestGameJob" | "RequestPrivateGame" | "RequestGame")
    ) {
        return false;
    }
    let str_field = |k: &str| object.get(k).and_then(|v| v.as_str());
    let Some(place_id) = str_field("placeId") else {
        println!(
            "[webview] hybrid launch payload has no string placeId; not publishing anything"
        );
        return true;
    };
    match crate::deeplink::publish_hybrid_game_launch(&crate::deeplink::HybridLaunch {
        place_id,
        instance_id: str_field("instanceId"),
        join_attempt_id: str_field("joinAttemptId"),
        join_attempt_origin: str_field("joinAttemptOrigin"),
        browser_tracker_id: str_field("browserTrackerId"),
        access_code: str_field("accessCode"),
        link_code: str_field("linkCode"),
        reserved_server_access_code: str_field("reservedServerAccessCode"),
    }) {
        Ok(()) => {
            println!("[webview] hybrid launch: handed off to deeplink::publish_hybrid_game_launch");
            // The join is actually on the bus now -- close the same way
            // `close-window` would have, so `WebView.handleWindowClose`
            // still reports it and the canvas-raise in `load.rs` still runs.
            // See this function's own doc for why this, and not a
            // `close-window` subscription, is what actually closes the
            // window: that subscription is armed and watching, and nothing
            // ever arrives on it.
            match CLOSE_HANDLER.get() {
                Some(close) => close(),
                None => println!(
                    "[webview] no close handler installed; the web window will stay open over \
                     the game that just started"
                ),
            }
        }
        Err(e) => println!("[webview] hybrid launch: could not publish: {e}"),
    }
    true
}

/// Hand an approved bridge message to `WebViewProtocol.signalJavascriptCallback`.
///
/// The receiving half of `cordial_shell::webview::set_bridge_sink`, wired to
/// it by `load.rs`'s `install_webview_presenter` — see that setter's doc for
/// the crate dependency edge that rules out the shell calling this directly,
/// and `cordial_shell::webview::open`'s bridge handler for the policy every
/// message has already passed before it arrives here.
///
/// This is the half whose absence made "pressing Join opens the game's detail
/// page instead of joining it" look like a WebKit bug. It was not: the page's
/// JS posts its command to `executeRoblox`/`RobloxWKHybrid`, nothing on this
/// end answered, and the page fell back to ordinary navigation — which is
/// what a web page does when the application hosting it is not listening.
///
/// **The argument shape is established; what the engine does with it is not.**
/// `signalJavascriptCallback(Ljava/lang/String;)V` is read out of the dex's
/// own method table (`tools/dex_method.py --class WebViewProtocol`), so the
/// single `String` is a fact rather than a naming-convention guess. What is
/// The shell mirrors Mocktail's two measured contracts before calling this:
/// `executeRoblox` supplies the posted object's JSON, while `RobloxWKHybrid`
/// supplies the envelope's string `command` property. **A successful call
/// here is not evidence that Join
/// worked** — it means the native returned without throwing. The observable
/// test is a game actually launching, and until someone has seen that, this
/// having "succeeded" says only that a string reached the engine.
///
/// Whether the native is declared static is likewise unsettled, and matters:
/// [`call_static_strings`] passes the class where an instance method would
/// want its object. Every other native Cordial reaches this way ignores that
/// argument, which is why this is worth attempting rather than blocking on —
/// and a native that does dereference it fails loudly on the error path below
/// rather than silently doing nothing.
///
/// **The message itself is never logged**, only its length, unless
/// [`trace_bridge`] is on. A bridge command carries whatever the page and the
/// engine are mid-conversation about, and `docs/analysis/webview-surface.md`
/// §4's rule about one-time authentication tickets in a web-view url applies
/// to this direction exactly as it does to that one -- which is why the
/// switch is opt-in and named for what it does, the same shape as
/// `CORDIAL_TRACE_TEXT_SHOW_PASSWORDS`.
pub fn forward_bridge_message(message: &str) {
    if trace_bridge() {
        println!("[webview] bridge message (CORDIAL_TRACE_BRIDGE=1): {message}");
    }
    if hybrid_launch_enabled() && route_as_hybrid_launch(message) {
        return;
    }
    let Some(native) = SIGNAL_JAVASCRIPT_CALLBACK_NATIVE.get() else {
        // Loud, for the same reason `report_window_closed` is: a page that
        // believes its command was delivered, next to a log that said
        // nothing, is precisely the lie AGENTS.md's rule about stubs exists
        // to rule out.
        println!(
            "[webview] a bridge message passed policy ({} bytes), but this build never resolved \
             signalJavascriptCallback (see arm()'s log above); the engine will not receive it",
            message.len()
        );
        return;
    };
    let f = *native as *mut c_void;
    // SAFETY: `native` was resolved under its own symbol name in `arm` and is
    // never used for anything else; `call_static_strings` owns the buffers it
    // passes for the duration of the call.
    match unsafe { cordial_linker_sys::game_activity::call_static_strings(f, CLASS, &[message]) } {
        Ok(()) => println!(
            "[webview] forwarded a bridge message to signalJavascriptCallback ({} bytes); \
             whether the engine acted on it is not established by this line",
            message.len()
        ),
        Err(e) => println!("[webview] forwarding a bridge message failed: {e}"),
    }
}

/// What `Connection.isConnected` says about the subscription, or `None` when
/// there is nothing to ask about. Mirrors `android::clipboard::connected`.
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

/// Convert a parsed request into what `cordial_shell::webview::open` takes.
///
/// A one-for-one field mapping, kept as an explicit function rather than
/// inlined at the one call site that needs it (see [`set_presenter`]'s doc)
/// so that call site is a conversion plus an `open()` call and nothing more.
/// `roblox_session_cookie` is threaded through rather than looked up here,
/// because [`roblox_session_cookie`] and this conversion have different
/// callers in mind — a presenter fetches the cookie itself, on whatever
/// thread it decided was safe to do a Secret Service round trip on, and
/// passes the result in.
#[cfg(feature = "webview")]
pub fn to_shell_request(
    request: &OpenWindowRequest,
    roblox_session_cookie: Option<String>,
) -> cordial_shell::webview::WindowRequest {
    cordial_shell::webview::WindowRequest {
        url: request.url.clone(),
        title: request.title.clone(),
        hide_header: request.hide_header,
        show_domain_as_title: request.show_domain_as_title,
        back_button_visible: request.back_button_visible,
        roblox_session_cookie,
        // Same string `setWebviewUserAgent` gives the engine (see
        // `load.rs`'s call, wired beside the other `NativeGLInterface`
        // natives) -- one source of truth in `user_agent()` above, so the
        // view and the engine cannot disagree about what this client claims
        // to be. `None` here means `cordial_shell::webview::open` leaves
        // WebKitGTK's own default in place rather than fabricating a string;
        // see that module for why a mismatch is worse than a fallback.
        user_agent: user_agent(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The symbol names are built by concatenation, and a typo in the prefix
    /// would make every lookup miss at once and read as "the build does not
    /// export this" rather than as a bug here.
    #[test]
    fn getter_symbols_are_spelled_the_way_the_engine_exports_them() {
        let mut asked = Vec::new();
        let _ = read_vocabulary(|name| {
            asked.push(name.to_string());
            None
        });
        assert_eq!(asked.len(), STRING_GETTERS.len());
        assert!(asked.contains(
            &"Java_com_roblox_protocols_webview_WebViewProtocol_getUrlKey".to_string()
        ));
        assert!(asked.contains(
            &"Java_com_roblox_protocols_webview_WebViewProtocol_getOpenWindowId".to_string()
        ));
        assert!(asked.iter().all(|n| n
            .starts_with("Java_com_roblox_protocols_webview_WebViewProtocol_")));
    }

    /// An absent getter must be reported as absent rather than as an empty
    /// answer: "this build has no `getHideHeaderKey`" and "`getHideHeaderKey`
    /// returned an empty string" would need different work, and a caller that
    /// cannot tell them apart will do the wrong one.
    #[test]
    fn a_missing_getter_is_recorded_rather_than_silently_skipped() {
        let v = read_vocabulary(|_| None);
        assert!(v.entries.is_empty());
        assert_eq!(v.missing.len(), STRING_GETTERS.len());
    }

    /// Same spelling discipline as the vocabulary getters: a typo in either
    /// symbol name would read as "this build does not export MessageBus" and
    /// send the next agent looking at the wrong build rather than at this file.
    #[test]
    fn bus_native_symbols_are_spelled_the_way_the_engine_exports_them() {
        let mut asked = Vec::new();
        let _ = find_bus_natives(|name| {
            asked.push(name.to_string());
            None
        });
        assert_eq!(asked.len(), 2);
        assert!(asked.contains(
            &"Java_com_roblox_universalapp_messagebus_MessageBus_getMessageId".to_string()
        ));
        assert!(asked.contains(
            &"Java_com_roblox_universalapp_messagebus_MessageBus_doSubscribeRaw".to_string()
        ));
    }

    /// Neither native being found must read as neither found, not as a partial
    /// or default-true answer that would let `report_bus_natives` claim
    /// something it never checked.
    #[test]
    fn missing_bus_natives_are_reported_as_missing() {
        let n = find_bus_natives(|_| None);
        assert!(!n.get_message_id);
        assert!(!n.do_subscribe_raw);
    }

    /// `arm` must bail out before touching any of the `extern "C"` functions
    /// when the vocabulary getters are not exported — a build with none of
    /// this protocol must not crash trying to subscribe to it. Nothing here
    /// should ever reach `cordial_messagebus_subscribe`, so the only failure
    /// mode this rules out is a panic or a call into unresolved FFI.
    ///
    /// This is as far as `arm` can be exercised without a running engine:
    /// every path past this point calls into `cordial-linker-sys` with a
    /// function pointer the callee dereferences, and a test has no real JNI
    /// environment to hand it one that would not crash. The rest of `arm` —
    /// `getMessageId` composing a bus id, `doSubscribeRaw` installing the
    /// callback, `Connection.isConnected` reporting live — is established by
    /// running the client and reading what it printed, not by a unit test.
    #[test]
    fn arm_does_nothing_when_the_vocabulary_is_not_exported() {
        arm(|_| None);
        assert!(connected().is_none(), "a bare arm() with nothing exported must not connect");
    }

    /// A stand-in vocabulary for the parsing tests below. **Not a claim about
    /// what any real build's `getUrlKey` and friends return** — nobody has
    /// run a signed-in client and watched `openWindow` fire (see this
    /// module's doc and `docs/analysis/webview-surface.md` §6, "not
    /// established"), so the true key strings are unknown here. What is
    /// tested is [`parse_open_window`]'s behaviour given *some* self-
    /// consistent vocabulary, which is everything a unit test can honestly
    /// claim without a running engine.
    fn stand_in_keys() -> OpenWindowKeys {
        OpenWindowKeys {
            url: "url".into(),
            title: "title".into(),
            hide_header: "hideHeader".into(),
            back_button_visible: "backButtonVisible".into(),
            show_domain_as_title: "showDomainAsTitle".into(),
        }
    }

    #[test]
    fn from_vocabulary_reads_every_key_by_its_label() {
        let v = Vocabulary {
            entries: vec![
                ("key:url", "u".into()),
                ("key:title", "t".into()),
                ("key:hide-header", "hh".into()),
                ("key:back-button-visible", "bbv".into()),
                ("key:show-domain-as-title", "sdat".into()),
            ],
            missing: Vec::new(),
        };
        let keys = OpenWindowKeys::from_vocabulary(&v).expect("all five present");
        assert_eq!(keys.url, "u");
        assert_eq!(keys.title, "t");
        assert_eq!(keys.hide_header, "hh");
        assert_eq!(keys.back_button_visible, "bbv");
        assert_eq!(keys.show_domain_as_title, "sdat");
    }

    /// Every missing getter must be named, not just the first — see the
    /// function's own doc for why a partial vocabulary is a different fact
    /// from a single hole in it.
    #[test]
    fn from_vocabulary_names_every_missing_getter() {
        let v = Vocabulary { entries: vec![("key:url", "u".into())], missing: Vec::new() };
        let missing = OpenWindowKeys::from_vocabulary(&v).unwrap_err();
        assert_eq!(missing.len(), 4);
        assert!(!missing.contains(&"key:url"));
        assert!(missing.contains(&"key:title"));
    }

    /// A constructed payload shaped like a real `openWindow` message under
    /// `stand_in_keys()` — **not a captured one**. Reproducing a real one
    /// needs a click in a signed-in client, which this session could not do
    /// (AGENTS.md's caution on synthesised input, and the task's own
    /// instruction not to sign in). The one real observation on record is a
    /// log line reading "90 bytes" with no field-level content in it, so
    /// this cannot even be checked against that shape beyond order of
    /// magnitude. It is included because it is the honest version of what
    /// was asked for, not a stand-in for the real thing.
    #[test]
    fn a_constructed_open_window_payload_parses() {
        let json = r#"{"url":"https://www.roblox.com/games/mock","hideHeader":false}"#;
        // Recorded rather than asserted to a specific figure: the point is
        // that this is the same order of magnitude as the one real capture
        // ("90 bytes"), not that it reproduces it.
        assert!(json.len() > 40 && json.len() < 200, "payload should be a small JSON object, was {} bytes", json.len());
        let request = parse_open_window(json, &stand_in_keys()).expect("a well-formed payload parses");
        assert_eq!(request.url, "https://www.roblox.com/games/mock");
        assert!(!request.hide_header);
        assert!(!request.back_button_visible, "absent fields default to false");
        assert!(request.title.is_none());
    }

    #[test]
    fn a_payload_with_no_url_is_refused() {
        let err = parse_open_window(r#"{"title":"Marketplace"}"#, &stand_in_keys()).unwrap_err();
        assert!(err.contains("url"), "{err}");
    }

    #[test]
    fn an_empty_url_is_refused_the_same_as_a_missing_one() {
        let err = parse_open_window(r#"{"url":""}"#, &stand_in_keys()).unwrap_err();
        assert!(err.contains("url"), "{err}");
    }

    #[test]
    fn malformed_json_is_refused_rather_than_panicking() {
        assert!(parse_open_window("not json", &stand_in_keys()).is_err());
        assert!(parse_open_window("[]", &stand_in_keys()).is_err(), "an array is not an object");
    }

    #[test]
    fn a_valid_roblosecurity_pair_is_extracted() {
        let jar = ".ROBLOSECURITY=abc123-value; other=1";
        assert_eq!(extract_roblosecurity(jar).unwrap(), "abc123-value");
    }

    #[test]
    fn a_jar_with_no_roblosecurity_pair_is_refused() {
        assert!(extract_roblosecurity("other=1; another=2").is_err());
    }

    #[test]
    fn an_empty_roblosecurity_value_is_refused() {
        assert_eq!(
            extract_roblosecurity(".ROBLOSECURITY=; other=1").unwrap_err(),
            ".ROBLOSECURITY is present but empty"
        );
    }

    /// The header-injection shape: a control byte or a semicolon-adjacent
    /// character smuggled into the value would land inside a `Set-Cookie`
    /// header WebKit sends verbatim.
    #[test]
    fn a_roblosecurity_value_with_an_unsafe_byte_is_refused() {
        assert!(extract_roblosecurity(".ROBLOSECURITY=abc\ndef").is_err());
        assert!(extract_roblosecurity(".ROBLOSECURITY=abc\"def").is_err());
    }

    #[test]
    fn an_oversized_roblosecurity_value_is_refused() {
        let huge = "a".repeat(MAX_ROBLOSECURITY_COOKIE_BYTES + 1);
        assert!(extract_roblosecurity(&format!(".ROBLOSECURITY={huge}")).is_err());
    }

    /// `set_presenter` records only the first installation, deterministically
    /// — a second one is reported rather than silently replacing or being
    /// silently ignored. Run in isolation from the other `PRESENTER` state
    /// this file's tests might otherwise race on: this is the only test that
    /// touches it, so no lock is needed beyond that.
    #[test]
    fn set_presenter_keeps_the_first_installation() {
        use std::sync::atomic::{AtomicUsize, Ordering as O};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        set_presenter(|_req| {
            CALLS.fetch_add(1, O::SeqCst);
        });
        // A second installation must not panic and must not replace the
        // first -- there is no second `CALLS` counter to prove it changed,
        // which is the point: nothing observable here should change.
        set_presenter(|_req| {
            CALLS.fetch_add(100, O::SeqCst);
        });
        if let Some(present) = PRESENTER.get() {
            present(OpenWindowRequest {
                url: "https://www.roblox.com/".into(),
                title: None,
                hide_header: false,
                back_button_visible: false,
                show_domain_as_title: false,
            });
        }
        assert_eq!(CALLS.load(O::SeqCst), 1, "the second presenter must never run");
    }

    /// Same discipline as [`set_presenter_keeps_the_first_installation`], for
    /// the close side: `set_close_handler` must keep the first installation
    /// rather than let a second one silently win or silently do nothing. Runs
    /// against `on_close_window` directly rather than through
    /// `cordial_messagebus_subscribe`, which needs a real JNI environment
    /// this test has none of -- the same limit `arm_does_nothing_when_the_
    /// vocabulary_is_not_exported`'s own doc gives for why `arm` cannot be
    /// exercised further here.
    #[test]
    fn set_close_handler_keeps_the_first_installation() {
        use std::sync::atomic::{AtomicUsize, Ordering as O};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        set_close_handler(|| {
            CALLS.fetch_add(1, O::SeqCst);
        });
        set_close_handler(|| {
            CALLS.fetch_add(100, O::SeqCst);
        });
        on_close_window(std::ptr::null());
        assert_eq!(CALLS.load(O::SeqCst), 1, "the second close handler must never run");
    }

    /// `CORDIAL_WEBVIEW_TEST`'s request must carry chrome that marks it as a
    /// synthesised one -- a plain header with the domain hidden would be
    /// indistinguishable from a real request that happened to ask for the
    /// same thing, defeating the reason this diagnostic sets them at all (see
    /// `dev_trigger_open_window`'s own doc). Deliberately does not touch
    /// `PRESENTER`, so it cannot race with `set_presenter_keeps_the_first_installation`.
    #[test]
    fn the_synthetic_request_is_visibly_a_diagnostic() {
        let request = synthetic_open_window_request("https://www.roblox.com/home".to_string());
        assert_eq!(request.url, "https://www.roblox.com/home");
        assert_eq!(request.title.as_deref(), Some("CORDIAL_WEBVIEW_TEST"));
        assert!(!request.hide_header, "a hidden header would hide the fact this is synthetic");
        assert!(request.back_button_visible);
        assert!(request.show_domain_as_title, "the title bar must show the real host, not a guess");
    }
}
