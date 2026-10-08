//! The version-0 live-settings line protocol: `<profile>/live/settings.sock`.
//!
//! The shell sends `{"set":{"pointer_acceleration":"unlocked"}}`; the client
//! answers with a [`Reply`]. One request per connection, one JSON line each way,
//! at most [`MAX_LINE`] bytes a request. This is the surface ADR-044 built and
//! `cordial.runtime/1` will replace; it stays an alias until the launcher no
//! longer needs it (spec section 8), and **nothing new is added to it**.
//!
//! It is pure -- no sockets, no GTK -- because both ends need to agree on it.
//! A plugin never sees this socket (ADR-003, ADR-007): it lives in a `0700`
//! directory in the profile, which the plugin sandbox does not bind.
//!
//! The wire bytes are pinned by golden strings in the tests below, taken from
//! the encoder as it stood in `cordial-shell` before it moved here.

use crate::settings::{parse_set_body, set_body, Update};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Directory inside the profile that holds the socket.
///
/// A directory rather than a bare socket file so the permission is on the
/// directory: a socket takes the process umask at `bind`, and closing the gap
/// between `bind` and a `chmod` is easier done by making the path unreachable
/// to anyone else from the start.
pub const SOCKET_DIR: &str = "live";
pub const SOCKET_NAME: &str = "settings.sock";

/// Longest request line either side will read. Generous for four keys and
/// small enough that a peer sending noise cannot make the client buffer it.
/// Version 1 raises this to 64 KiB; version 0 keeps what it shipped with.
pub const MAX_LINE: usize = 1024;

/// Where a profile's client listens.
pub fn socket_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join(SOCKET_DIR).join(SOCKET_NAME)
}

/// What a client is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Change these. `ignored` names keys this client does not know, which are
    /// reported back rather than failing the whole message, so a newer shell
    /// talking to an older client still gets its known keys applied.
    Set { updates: Vec<Update>, ignored: Vec<String> },
    /// Report the values in force now.
    Get,
}

/// One line, newline-terminated.
pub fn encode_set(updates: &[Update]) -> String {
    #[derive(Serialize)]
    struct Line<'a> {
        set: &'a BTreeMap<String, Value>,
    }
    let mut line = serde_json::to_string(&Line { set: &set_body(updates) }).expect("a set always serialises");
    line.push('\n');
    line
}

pub fn encode_get() -> String {
    "{\"get\":true}\n".to_string()
}

pub fn decode(line: &str) -> Result<Request, String> {
    if line.len() > MAX_LINE {
        return Err(format!("request longer than {MAX_LINE} bytes"));
    }
    let value: Value = serde_json::from_str(line.trim()).map_err(|e| format!("not JSON: {e}"))?;
    let Value::Object(top) = value else {
        return Err("a request is a JSON object".to_string());
    };
    if top.len() != 1 {
        return Err("a request has exactly one verb".to_string());
    }
    let (verb, body) = top.into_iter().next().expect("length checked above");
    match (verb.as_str(), body) {
        ("get", Value::Bool(true)) => Ok(Request::Get),
        ("set", Value::Object(map)) => {
            let body = parse_set_body(&map)?;
            Ok(Request::Set { updates: body.updates, ignored: body.ignored })
        }
        (other, _) => Err(format!("unknown request {other:?}")),
    }
}

/// A client's answer. `values` is filled for `get` and for a `set` (the state
/// after the change), so the shell can log what is actually in force rather
/// than what it asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, Value>,
    /// Things the client applied but wants said: `audio_output` with nothing
    /// playing has nothing to move, and on a backend with no notion of a sink it
    /// cannot move anything. Keyed by setting. Present so "applied" is never
    /// read as "you will hear it", which is the claim a settings row must not
    /// make on the client's behalf.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    pub fn failure(why: impl Into<String>) -> Self {
        Reply { ok: false, error: Some(why.into()), ..Reply::default() }
    }
    pub fn encode(&self) -> String {
        let mut line = serde_json::to_string(self).expect("a Reply always serialises");
        line.push('\n');
        line
    }
    pub fn decode(line: &str) -> Result<Self, String> {
        serde_json::from_str(line.trim()).map_err(|e| format!("unreadable reply: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{valid_sink_name, Accel, Throttle, KEYS, MAX_SINK_NAME};

    fn all() -> Vec<Update> {
        vec![
            Update::PointerAcceleration(Accel::Unlocked),
            Update::Throttle(Throttle::Off),
            Update::CloseOnLeave(true),
            Update::CarryLaunchTicket(false),
            Update::AudioOutput("alsa_output.pci-0000_00_1f.3.analog-stereo".to_string()),
            Update::AudioInput("alsa_input.usb-Headset-00.mono-fallback".to_string()),
            Update::Gamemode(false),
            Update::Gamepad(false),
            Update::TitleBar(crate::settings::TitleBar::Hidden),
            Update::FrameRateLimit(crate::settings::FrameRateLimit::Cap144),
        ]
    }

    #[test]
    fn every_update_round_trips_through_the_wire() {
        let line = encode_set(&all());
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1, "one line");
        let Request::Set { mut updates, ignored } = decode(&line).unwrap() else {
            panic!("a set encodes to a set")
        };
        assert!(ignored.is_empty());
        let mut want = all();
        // JSON objects are unordered; compare as sets of keys.
        updates.sort_by_key(|u| u.key());
        want.sort_by_key(|u| u.key());
        assert_eq!(updates, want);
    }

    #[test]
    fn the_key_list_matches_what_update_can_carry() {
        let mut keys: Vec<_> = all().iter().map(Update::key).collect();
        keys.sort_unstable();
        let mut listed = KEYS.to_vec();
        listed.sort_unstable();
        assert_eq!(keys, listed, "KEYS and Update::key drifted apart");
    }

    #[test]
    fn a_sink_name_may_be_empty_and_may_not_be_enormous() {
        // Empty is the system default, which is a choice and not an error.
        let Request::Set { updates, .. } = decode(r#"{"set":{"audio_output":""}}"#).unwrap() else {
            panic!("a set decodes to a set")
        };
        assert_eq!(updates, vec![Update::AudioOutput(String::new())]);
        let long = format!(r#"{{"set":{{"audio_output":"{}"}}}}"#, "a".repeat(MAX_SINK_NAME + 1));
        assert!(decode(&long).is_err());
        assert!(valid_sink_name("bluez_output.AA_BB_CC.1"));
        assert!(!valid_sink_name("two\nlines"));
    }

    #[test]
    fn get_round_trips() {
        assert_eq!(decode(&encode_get()).unwrap(), Request::Get);
    }

    #[test]
    fn an_unknown_key_is_ignored_and_named_and_known_keys_still_apply() {
        let r = decode(r#"{"set":{"throttle":"off","warp_drive":"on"}}"#).unwrap();
        assert_eq!(
            r,
            Request::Set {
                updates: vec![Update::Throttle(Throttle::Off)],
                ignored: vec!["warp_drive".to_string()],
            }
        );
    }

    #[test]
    fn a_known_key_with_a_bad_value_refuses_the_whole_message() {
        // Half-applying a message would leave the shell believing one thing and
        // the client another, so a bad value is an error, not a skip.
        for bad in [
            r#"{"set":{"throttle":"sometimes"}}"#,
            r#"{"set":{"pointer_acceleration":true}}"#,
            r#"{"set":{"close_on_leave":"yes"}}"#,
            r#"{"set":{"throttle":"off","carry_launch_ticket":1}}"#,
            r#"{"set":{"audio_output":true}}"#,
            r#"{"set":{"gamemode":"on"}}"#,
            r#"{"set":{"gamepad":0}}"#,
            r#"{"set":{"title_bar":"tiny"}}"#,
            r#"{"set":{"title_bar":true}}"#,
            r#"{"set":{"frame_rate_limit":"unlimited"}}"#,
            r#"{"set":{"frame_rate_limit":"9999"}}"#,
            r#"{"set":{"frame_rate_limit":144}}"#,
            r#"{"set":{"audio_output":"a\u0000b"}}"#,
            r#"{"set":{"audio_input":false}}"#,
            r#"{"set":{"audio_input":"a\u0000b"}}"#,
        ] {
            assert!(decode(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn nothing_but_the_two_verbs_is_understood() {
        for bad in [
            "",
            "hello",
            "[]",
            r#"{}"#,
            r#"{"get":false}"#,
            r#"{"run":"ls"}"#,
            r#"{"exec":{"cmd":"ls"}}"#,
            r#"{"set":{},"get":true}"#,
            r#"{"set":"throttle"}"#,
        ] {
            assert!(decode(bad).is_err(), "{bad:?} must not decode");
        }
    }

    #[test]
    fn an_oversized_line_is_refused_before_it_is_parsed() {
        let big = format!(r#"{{"set":{{"x":"{}"}}}}"#, "a".repeat(MAX_LINE));
        assert!(decode(&big).unwrap_err().contains("longer than"));
    }

    #[test]
    fn a_reply_round_trips_and_a_failure_says_why() {
        let mut ok = Reply { ok: true, applied: vec!["throttle".into()], ..Reply::default() };
        ok.values.insert("throttle".into(), Value::from("off"));
        assert_eq!(Reply::decode(&ok.encode()).unwrap(), ok);
        ok.notes.insert("audio_output".into(), "nothing was playing".into());
        assert_eq!(Reply::decode(&ok.encode()).unwrap(), ok);
        let bad = Reply::failure("nope");
        let back = Reply::decode(&bad.encode()).unwrap();
        assert!(!back.ok);
        assert_eq!(back.error.as_deref(), Some("nope"));
    }

    #[test]
    fn the_socket_lives_in_a_directory_of_its_own_inside_the_profile() {
        assert_eq!(
            socket_path(Path::new("/p/default")),
            PathBuf::from("/p/default/live/settings.sock")
        );
    }

    // Taken from `cordial-shell`'s encoder before it moved here, by printing
    // the output of the old `encode_set`/`encode_get`/`Reply::encode` for the
    // values below on the commit that preceded the move. They are the bytes a
    // running client has been reading, so a change to any of them is a wire
    // change and not a refactor.
    const GOLDEN_SET_ALL: &str = "{\"set\":{\"audio_input\":\"alsa_input.usb-Headset-00.mono-fallback\",\"audio_output\":\"alsa_output.pci-0000_00_1f.3.analog-stereo\",\"carry_launch_ticket\":false,\"close_on_leave\":true,\"frame_rate_limit\":\"144\",\"gamemode\":false,\"gamepad\":false,\"pointer_acceleration\":\"unlocked\",\"throttle\":\"off\",\"title_bar\":\"hidden\"}}\n";
    const GOLDEN_ONE: [&str; 10] = [
        "{\"set\":{\"pointer_acceleration\":\"unlocked\"}}\n",
        "{\"set\":{\"throttle\":\"off\"}}\n",
        "{\"set\":{\"close_on_leave\":true}}\n",
        "{\"set\":{\"carry_launch_ticket\":false}}\n",
        "{\"set\":{\"audio_output\":\"alsa_output.pci-0000_00_1f.3.analog-stereo\"}}\n",
        "{\"set\":{\"audio_input\":\"alsa_input.usb-Headset-00.mono-fallback\"}}\n",
        "{\"set\":{\"gamemode\":false}}\n",
        "{\"set\":{\"gamepad\":false}}\n",
        "{\"set\":{\"title_bar\":\"hidden\"}}\n",
        "{\"set\":{\"frame_rate_limit\":\"144\"}}\n",
    ];
    const GOLDEN_REPLY_OK: &str = "{\"ok\":true,\"applied\":[\"throttle\",\"title_bar\"],\"ignored\":[\"warp_drive\"],\"values\":{\"gamemode\":true,\"throttle\":\"off\"}}\n";
    const GOLDEN_REPLY_NOTES: &str = "{\"ok\":true,\"applied\":[\"throttle\",\"title_bar\"],\"ignored\":[\"warp_drive\"],\"values\":{\"gamemode\":true,\"throttle\":\"off\"},\"notes\":{\"audio_output\":\"nothing was playing\"}}\n";
    const GOLDEN_REPLY_FAIL: &str = "{\"ok\":false,\"error\":\"nope\"}\n";
    const GOLDEN_REPLY_DEFAULT: &str = "{\"ok\":false}\n";

    #[test]
    fn the_wire_bytes_are_the_ones_the_old_encoder_produced() {
        assert_eq!(encode_set(&all()), GOLDEN_SET_ALL);
        for (u, want) in all().iter().zip(GOLDEN_ONE) {
            assert_eq!(encode_set(std::slice::from_ref(u)), want, "{}", u.key());
        }
        assert_eq!(encode_get(), "{\"get\":true}\n");
        let mut ok = Reply {
            ok: true,
            applied: vec!["throttle".into(), "title_bar".into()],
            ignored: vec!["warp_drive".into()],
            ..Reply::default()
        };
        ok.values.insert("throttle".into(), Value::from("off"));
        ok.values.insert("gamemode".into(), Value::from(true));
        assert_eq!(ok.encode(), GOLDEN_REPLY_OK);
        ok.notes.insert("audio_output".into(), "nothing was playing".into());
        assert_eq!(ok.encode(), GOLDEN_REPLY_NOTES);
        assert_eq!(Reply::failure("nope").encode(), GOLDEN_REPLY_FAIL);
        assert_eq!(Reply::default().encode(), GOLDEN_REPLY_DEFAULT);
    }

    #[test]
    fn the_golden_lines_still_decode() {
        let Request::Set { updates, ignored } = decode(GOLDEN_SET_ALL).unwrap() else { panic!("a set") };
        assert!(ignored.is_empty());
        assert_eq!(updates.len(), 10);
        assert_eq!(Reply::decode(GOLDEN_REPLY_NOTES).unwrap().notes["audio_output"], "nothing was playing");
        assert!(!Reply::decode(GOLDEN_REPLY_FAIL).unwrap().ok);
    }
}
