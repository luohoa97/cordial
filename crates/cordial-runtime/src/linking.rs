//! `Linking.openURL` — the engine asking the host to open a link.
//!
//! Clicking Terms, Privacy, or an experience's external link did nothing in
//! Cordial while Sober opened a browser. Not a bug: nothing was ever bound to
//! answer the request.
//!
//! **It is not an `Intent`.** Roblox's own Java builds the `ACTION_VIEW`
//! intent on Android, and Roblox's Java is exactly the layer Cordial replaces,
//! so no `android/content/Intent` appears anywhere in this path -- consistent
//! with `docs/analysis/deep-links.md`, which found every `Intent` line in the
//! Android capture belonging to Google Play services rather than to Roblox's
//! own pid. What the engine actually does is bind a message-bus *request*:
//! protocol `Linking`, method `openURL`, payload `{"url": "..."}`, and it
//! waits for `{"success": <bool>}` back.
//!
//! The vocabulary is not guessed. `libroblox.so` exports zero-argument getters
//! for every piece of it -- `getProtocolName`, `getOpenURLId`, `getUrlKey`,
//! `getSuccessKey` -- and their values were read out of a running engine and
//! recorded in `docs/analysis/deep-links.md`: `"Linking"`, `"openURL"`,
//! `"url"`. The live ClientSettings for this build corroborate the shape by
//! name, `FFlagKeepLinkingOpenUrlRequestHandlerBound` among them.
//!
//! This is the request/response half of the bus, which is why it needs
//! `setRequestHandlerRaw` rather than the `doSubscribeRaw` the inbound
//! direction uses. `deeplink.cpp` handles links coming *in*; this is the way
//! out.

use std::ffi::{c_char, c_int, c_void, CString};

unsafe extern "C" {
    fn cordial_messagebus_set_request_handler(
        f: *mut c_void,
        protocol: *const c_char,
        method: *const c_char,
        sink: extern "C" fn(*const c_char, *mut c_char, usize) -> c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}

/// Read out of a running engine, not inferred from the class names. See the
/// module doc.
const PROTOCOL: &str = "Linking";
const METHOD: &str = "openURL";

/// What the engine hands over, and the only field of it Cordial reads.
const URL_KEY: &str = "url";

/// Answer one `openURL` request.
///
/// Runs on whichever engine thread issued the request. `urlopen::open` talks
/// to the portal over a fresh session-bus connection, so it does not need the
/// GTK thread and must not take it -- blocking the engine's thread on a GTK
/// round trip is how the pump gets stalled.
extern "C" fn on_open_url(request: *const c_char, out: *mut c_char, out_len: usize) -> c_int {
    // SAFETY: the bus hands over a NUL-terminated string it owns for the
    // duration of the call, which is exactly `borrow_request`'s contract.
    let Some(raw) = (unsafe { crate::ffi_util::borrow_request(request) }) else {
        return 0;
    };
    let opened = open_from_request(&raw.to_string_lossy());

    let body = if opened { "{\"success\":true}" } else { "{\"success\":false}" };
    let Ok(c) = CString::new(body) else { return 0 };
    if crate::ffi_util::write_response(c.as_bytes_with_nul(), out, out_len) {
        1
    } else {
        0
    }
}

/// Parse the request and open the URL, returning whether it actually opened.
///
/// **Never logs the URL.** A link can carry credentials in its query string,
/// and this codebase has printed one in full before -- `docs/analysis/deep-links.md`
/// section 6.3 records it. Failures name the reason, not the address.
///
/// **Never reports success it did not achieve.** The engine's Lua shows the
/// user something based on that boolean, so a refused scheme returning `true`
/// would put "link opened" in front of somebody whose browser never moved.
/// That is AGENTS.md's rule about stubs that lie, with a user-visible
/// consequence attached.
fn open_from_request(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        eprintln!("[linking] openURL request was not JSON");
        return false;
    };
    let Some(url) = value.get(URL_KEY).and_then(|u| u.as_str()) else {
        eprintln!("[linking] openURL request carried no {URL_KEY:?}");
        return false;
    };
    // The URL comes from the engine, which got it from Lua, which got it from
    // an experience's script or a web page. Attacker-controlled, and treated
    // that way: `urlopen::open` refuses anything that is not http or https
    // before it touches the bus, which covers `file://`, `javascript:` and
    // `mailto:`. The engine has its own scheme validation
    // (`DFFlagEnableOpenUrlSchemeValidation`) and it is not relied on -- it
    // runs on the far side of this call and its lists are not visible here.
    match cordial_plugins::urlopen::open(url) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("[linking] openURL refused: {e}");
            false
        }
    }
}

/// Bind the handler. `symbol` resolves a name in the loaded engine.
///
/// Called once, after the message bus exists. A failure is reported and not
/// fatal: a client that cannot open links is worth having, and this is exactly
/// the shape of thing that should not stop a launch.
pub fn arm(symbol: impl Fn(&str) -> Option<*mut c_void>) {
    let Some(set) = symbol("Java_com_roblox_universalapp_messagebus_MessageBus_setRequestHandlerRaw")
    else {
        println!("  linking: setRequestHandlerRaw is not exported; external links will not open");
        return;
    };
    let protocol = CString::new(PROTOCOL).expect("literal");
    let method = CString::new(METHOD).expect("literal");
    let mut err = vec![0u8; 512];
    // SAFETY: both strings outlive the call; `err` is only written into.
    let rc = unsafe {
        cordial_messagebus_set_request_handler(
            set,
            protocol.as_ptr(),
            method.as_ptr(),
            on_open_url,
            err.as_mut_ptr() as *mut c_char,
            err.len(),
        )
    };
    if rc == 0 {
        println!("  linking: {PROTOCOL}.{METHOD} handler bound");
    } else {
        let msg = String::from_utf8_lossy(&err);
        println!("  linking: could not bind {PROTOCOL}.{METHOD}: {}", msg.trim_end_matches('\0'));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_with_no_url_is_refused_rather_than_guessed_at() {
        assert!(!open_from_request("{}"));
        assert!(!open_from_request(r#"{"notaurl":"https://example.com"}"#));
    }

    #[test]
    fn a_malformed_request_is_refused() {
        assert!(!open_from_request("not json at all"));
        assert!(!open_from_request(""));
    }

    #[test]
    fn a_refused_scheme_reports_failure_and_does_not_lie() {
        // The engine's Lua shows the user something based on this boolean, so
        // these must be false rather than optimistic. They also never reach
        // the bus: `urlopen` validates before connecting.
        for hostile in [
            r#"{"url":"file:///etc/passwd"}"#,
            r#"{"url":"javascript:alert(1)"}"#,
            r#"{"url":"mailto:someone@example.com"}"#,
            r#"{"url":"example.com"}"#,
        ] {
            assert!(!open_from_request(hostile), "{hostile} must be refused");
        }
    }
}
