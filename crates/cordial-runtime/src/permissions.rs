//! `PermissionsProtocol` -- the engine asking the host whether it may use the
//! microphone.
//!
//! Voice could not ask for the mic. On Android the engine does not call
//! `checkSelfPermission` for this (that is answered, and granted, in
//! `android_classes.cpp`, and it changed nothing); it sends a message-bus
//! request on protocol `PermissionsProtocol` and waits for Roblox's Java to
//! answer it. Cordial replaces that Java, bound nothing, and the request went
//! unanswered. `RBX::PermissionsProtocolCore::requestPermissions` and
//! `hasPermissions` in the engine's symbol table, and the three method names
//! next to the protocol name in its string pool, are the evidence for the shape.
//!
//! The vocabulary is read from string tables, not from any implementation:
//! the engine carries `PermissionsProtocol`, `PermissionsRequest`,
//! `HasPermissions`, `SupportsPermissions`, `MICROPHONE_ACCESS`, `permissions`
//! and `status`; the dex carries the same names plus `AUTHORIZED`, `DENIED`
//! and `missingPermissions`. Response schemas are cross-checked against
//! mocktail's Apache-2.0 `src/runtime/roblox_permissions_bridge.cc`: discovery
//! returns a permission list, authorisation returns status and missing names,
//! and the two Android-dialog queries return hidden upsells. A status response
//! to discovery cannot tell the engine that microphone access is supported.
//! Protocol publication followed by async resolution reached voice setup in a
//! controlled run, and the completed capture path was then confirmed manually
//! to transmit audible microphone audio in a live game.
//!
//! Granting the permission does not open the microphone. The rule in
//! `native/audio_classes.cpp` stands: no capture stream exists until Roblox
//! actually starts recording.

use std::ffi::{c_char, c_int, c_void, CString, OsStr};

unsafe extern "C" {
    fn cordial_messagebus_set_request_handler_async(
        set_fn: *mut c_void,
        publish_fn: *mut c_void,
        respond_fn: *mut c_void,
        dual_response: c_int,
        protocol: *const c_char,
        method: *const c_char,
        sink: extern "C" fn(*const c_char, *mut c_char, usize) -> c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}

const PROTOCOL: &str = "PermissionsProtocol";
const DUAL_RESPONSE_ENV: &str = "CORDIAL_PERMISSIONS_DUAL_RESPONSE";
const PUBLISH_RESPONSE_NATIVE: &str =
    "Java_com_roblox_universalapp_messagebus_MessageBus_publishProtocolMethodResponseRaw";

/// Microphone capture is implemented. LOCAL_NETWORK has no Android runtime
/// permission to request; actual network access remains subject to host policy.
const MICROPHONE: &str = "MICROPHONE_ACCESS";
const SUPPORTED: [&str; 2] = [MICROPHONE, "LOCAL_NETWORK"];

extern "C" fn on_request(request: *const c_char, out: *mut c_char, out_len: usize) -> c_int {
    reply("PermissionsRequest", request, out, out_len)
}

extern "C" fn on_has(request: *const c_char, out: *mut c_char, out_len: usize) -> c_int {
    reply("HasPermissions", request, out, out_len)
}

extern "C" fn on_supports(request: *const c_char, out: *mut c_char, out_len: usize) -> c_int {
    reply("SupportsPermissions", request, out, out_len)
}

extern "C" fn on_upsell(request: *const c_char, out: *mut c_char, out_len: usize) -> c_int {
    reply("ShouldShowPermissionUpsell", request, out, out_len)
}

extern "C" fn on_rationale(request: *const c_char, out: *mut c_char, out_len: usize) -> c_int {
    reply(
        "ShouldShowRequestPermissionRationale",
        request,
        out,
        out_len,
    )
}

fn reply(method: &str, request: *const c_char, out: *mut c_char, out_len: usize) -> c_int {
    // SAFETY: the bus hands over a NUL-terminated string it owns for the
    // call, which is exactly `borrow_request`'s contract.
    let Some(raw) = (unsafe { crate::ffi_util::borrow_request(request) }) else {
        return 0;
    };
    let body = answer(method, &raw.to_string_lossy());
    let Ok(c) = CString::new(body) else { return 0 };
    if crate::ffi_util::write_response(c.as_bytes_with_nul(), out, out_len) {
        1
    } else {
        0
    }
}

/// Build the response for one request without logging its contents.
fn answer(method: &str, raw: &str) -> String {
    let object = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw).ok();
    let asked = object
        .as_ref()
        .and_then(|value| value.get("permissions"))
        .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok());
    let response = match method {
        "SupportsPermissions" => {
            let permissions = if object.is_some() {
                SUPPORTED.as_slice()
            } else {
                &[]
            };
            serde_json::json!({ "permissions": permissions })
        }
        "ShouldShowPermissionUpsell" | "ShouldShowRequestPermissionRationale" => {
            // There is no Android permission dialog to show on this desktop.
            let hidden = asked.unwrap_or_else(|| vec![MICROPHONE.to_owned()]);
            serde_json::json!({ "upsellStatus": "HIDE", "hiddenUpsellPermissions": hidden })
        }
        "HasPermissions" | "PermissionsRequest" => {
            let valid = asked.is_some();
            let names = asked.unwrap_or_else(|| vec![MICROPHONE.to_owned()]);
            let missing: Vec<&str> = names
                .iter()
                .map(String::as_str)
                .filter(|name| !valid || !SUPPORTED.contains(name))
                .collect();
            let status = if valid && missing.is_empty() {
                "AUTHORIZED"
            } else {
                "DENIED"
            };
            serde_json::json!({ "status": status, "missingPermissions": missing })
        }
        _ => serde_json::json!({ "status": "DENIED", "missingPermissions": [MICROPHONE] }),
    };
    println!("  permissions: {method} answered");
    response.to_string()
}

fn dual_response_enabled(value: Option<&OsStr>) -> bool {
    value != Some(OsStr::new("0"))
}

/// Bind discovery, authorisation and dialog queries. `symbol` resolves a name
/// in the loaded engine.
///
/// Not fatal on failure, like `linking::arm`: a client without voice is still
/// worth launching.
pub fn arm(symbol: impl Fn(&str) -> Option<*mut c_void>) {
    let set =
        symbol("Java_com_roblox_universalapp_messagebus_MessageBus_setRequestHandlerAsyncRaw");
    let respond =
        symbol("Java_com_roblox_universalapp_messagebus_MessageBus_callResponseHandlerRaw");
    let (Some(set), Some(respond)) = (set, respond) else {
        println!("  permissions: async request natives are not exported; the microphone cannot be granted");
        return;
    };
    let configured = std::env::var_os(DUAL_RESPONSE_ENV);
    let dual_response = dual_response_enabled(configured.as_deref());
    let publish = if dual_response {
        symbol(PUBLISH_RESPONSE_NATIVE)
    } else {
        None
    };
    if dual_response && publish.is_none() {
        println!(
            "  permissions: protocol+async delivery is enabled, but \
             publishProtocolMethodResponseRaw is not exported; no permission handlers were bound"
        );
        return;
    }
    println!(
        "  permissions: response delivery mode {}",
        if dual_response {
            "protocol+async"
        } else {
            "async"
        }
    );
    let protocol = CString::new(PROTOCOL).expect("literal");
    let methods: [(
        &str,
        extern "C" fn(*const c_char, *mut c_char, usize) -> c_int,
    ); 5] = [
        ("PermissionsRequest", on_request),
        ("HasPermissions", on_has),
        ("SupportsPermissions", on_supports),
        ("ShouldShowPermissionUpsell", on_upsell),
        ("ShouldShowRequestPermissionRationale", on_rationale),
    ];
    for (method, sink) in methods {
        let m = CString::new(method).expect("literal");
        let mut err = vec![0u8; 512];
        // SAFETY: resolved native pointers use the declared ABI; strings outlive
        // the call, and `err` is writable for the supplied length.
        let rc = unsafe {
            cordial_messagebus_set_request_handler_async(
                set,
                publish.unwrap_or(std::ptr::null_mut()),
                respond,
                c_int::from(dual_response),
                protocol.as_ptr(),
                m.as_ptr(),
                sink,
                err.as_mut_ptr() as *mut c_char,
                err.len(),
            )
        };
        if rc == 0 {
            println!("  permissions: {PROTOCOL}.{method} handler bound");
        } else {
            let msg = String::from_utf8_lossy(&err);
            println!(
                "  permissions: could not bind {PROTOCOL}.{method}: {}",
                msg.trim_end_matches('\0')
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(json: &str) -> (String, Vec<String>) {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let missing = v["missingPermissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect();
        (v["status"].as_str().unwrap().to_owned(), missing)
    }

    #[test]
    fn the_microphone_is_granted() {
        let (s, missing) = status(&answer(
            "PermissionsRequest",
            r#"{"permissions":["MICROPHONE_ACCESS"]}"#,
        ));
        assert_eq!(s, "AUTHORIZED");
        assert!(missing.is_empty());
    }

    #[test]
    fn a_permission_with_nothing_behind_it_is_not_granted() {
        let (s, missing) = status(&answer(
            "PermissionsRequest",
            r#"{"permissions":["MICROPHONE_ACCESS","CAMERA_ACCESS"]}"#,
        ));
        assert_eq!(s, "DENIED");
        assert_eq!(missing, vec!["CAMERA_ACCESS"]);
    }

    #[test]
    fn a_malformed_request_still_gets_a_well_formed_answer() {
        let (s, _) = status(&answer("HasPermissions", "not json"));
        assert_eq!(s, "DENIED");
    }

    #[test]
    fn supported_permissions_are_a_list_when_discovery_is_requested() {
        // Given a support query with no permissions array.
        let request = "{}";
        // When the engine asks what the host supports.
        let response: serde_json::Value =
            serde_json::from_str(&answer("SupportsPermissions", request)).unwrap();
        // Then it receives the discovery schema, not an authorisation result.
        assert_eq!(
            response,
            serde_json::json!({"permissions": ["MICROPHONE_ACCESS", "LOCAL_NETWORK"]})
        );
    }

    #[test]
    fn network_and_microphone_are_granted_when_requested_together() {
        // Given the voice permission bundle.
        let request = r#"{"permissions":["MICROPHONE_ACCESS","LOCAL_NETWORK"]}"#;
        // When the host checks the bundle.
        let result = status(&answer("HasPermissions", request));
        // Then neither permission remains missing.
        assert_eq!(result, ("AUTHORIZED".to_owned(), vec![]));
    }

    #[test]
    fn android_dialogs_are_hidden_when_upsell_or_rationale_is_requested() {
        for method in [
            "ShouldShowPermissionUpsell",
            "ShouldShowRequestPermissionRationale",
        ] {
            // Given a permission for which this desktop has no Android dialog.
            let request = r#"{"permissions":["CAMERA_ACCESS"]}"#;
            // When the engine asks whether to show one.
            let response: serde_json::Value =
                serde_json::from_str(&answer(method, request)).unwrap();
            // Then hiding the dialog does not claim to grant the permission.
            assert_eq!(
                response,
                serde_json::json!({"upsellStatus":"HIDE", "hiddenUpsellPermissions":["CAMERA_ACCESS"]})
            );
        }
    }

    #[test]
    fn dual_response_defaults_on_and_zero_is_the_rollback() {
        // Given the default, rollback, and explicit enabled values.
        let values = [None, Some(OsStr::new("0")), Some(OsStr::new("1"))];
        // When each value is parsed at the permission boundary.
        let enabled = values.map(dual_response_enabled);
        // Then dual delivery is on unless the rollback explicitly disables it.
        assert_eq!(enabled, [true, false, true]);
    }
}
