//! The message set the shell uses to change a running client's settings.
//!
//! **This moved to `cordial-protocol`** (ADR-055), which is where the launcher
//! and any other runtime get it from. What is left here is the path
//! `cordial_shell::live_wire::...`, kept so `cordial-runtime`'s server side
//! keeps compiling unchanged until it depends on the crate itself, and a test
//! that proves the bytes did not change on the way.
//!
//! A fixed vocabulary, not a command channel: each [`Update`] is one setting a
//! running client can change in place, and there is no verb that runs anything
//! (ADR-044, ADR-003).

pub use cordial_protocol::settings::{
    parse_title_bar, title_bar_word, valid_sink_name, Accel, Throttle, Update, KEYS, MAX_SINK_NAME,
};
pub use cordial_protocol::v0::{
    decode, encode_get, encode_set, socket_path, Reply, Request, MAX_LINE, SOCKET_DIR, SOCKET_NAME,
};

#[cfg(test)]
mod tests {
    //! **The oracle.** `legacy` below is the encoder exactly as it stood in this
    //! file before it moved to `cordial-protocol`, copied verbatim and kept only
    //! here. Every test feeds the same values through it and through the
    //! re-exports and demands identical bytes, which is the claim the move makes:
    //! a running client written against the old shell reads the new one. The
    //! golden strings in `cordial-protocol`'s `v0` tests pin the same bytes from
    //! the other side.

    use super::*;
    use cordial_protocol::{FrameRateLimit, TitleBar};
    use serde::Serialize;
    use serde_json::{Map, Value};
    use std::collections::BTreeMap;

    mod legacy {
        use super::*;

        pub fn value(u: &Update) -> Value {
            match u {
                Update::PointerAcceleration(a) => Value::from(a.as_str()),
                Update::Throttle(t) => Value::from(t.as_str()),
                Update::CloseOnLeave(b) | Update::CarryLaunchTicket(b) | Update::Gamemode(b) | Update::Gamepad(b) => {
                    Value::from(*b)
                }
                Update::AudioOutput(name) | Update::AudioInput(name) => Value::from(name.as_str()),
                Update::TitleBar(t) => Value::from(title_bar_word(*t)),
                Update::FrameRateLimit(l) => Value::from(l.as_env()),
            }
        }

        pub fn encode_set(updates: &[Update]) -> String {
            let mut set = Map::new();
            for u in updates {
                set.insert(u.key().to_string(), value(u));
            }
            let mut line = Value::Object(Map::from_iter([("set".to_string(), Value::Object(set))])).to_string();
            line.push('\n');
            line
        }

        pub fn encode_get() -> String {
            "{\"get\":true}\n".to_string()
        }

        #[derive(Serialize, Default)]
        pub struct Reply {
            pub ok: bool,
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            pub applied: Vec<String>,
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            pub ignored: Vec<String>,
            #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
            pub values: BTreeMap<String, Value>,
            #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
            pub notes: BTreeMap<String, String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub error: Option<String>,
        }

        impl Reply {
            pub fn encode(&self) -> String {
                let mut line = serde_json::to_string(self).expect("a Reply always serialises");
                line.push('\n');
                line
            }
        }
    }

    /// Every variant, with every enum value it can carry, so the loop below
    /// covers the whole wire vocabulary rather than one example of each.
    fn every_value() -> Vec<Update> {
        let mut all = Vec::new();
        all.extend([Accel::Unlocked, Accel::Always].map(Update::PointerAcceleration));
        all.extend([Throttle::Visible, Throttle::Unfocused, Throttle::Off].map(Update::Throttle));
        for b in [true, false] {
            all.extend([Update::CloseOnLeave(b), Update::CarryLaunchTicket(b), Update::Gamemode(b), Update::Gamepad(b)]);
        }
        for name in ["", "alsa_output.pci-0000_00_1f.3.analog-stereo", "bluez_output.AC_12.1", "caf\u{e9} \"quoted\" \\ slash"] {
            all.extend([Update::AudioOutput(name.into()), Update::AudioInput(name.into())]);
        }
        all.extend([TitleBar::Default, TitleBar::Compact, TitleBar::Hidden].map(Update::TitleBar));
        all.extend(FrameRateLimit::ALL.map(Update::FrameRateLimit));
        all
    }

    #[test]
    fn each_update_encodes_to_the_same_bytes_as_the_old_encoder() {
        for u in every_value() {
            let one = std::slice::from_ref(&u);
            assert_eq!(encode_set(one), legacy::encode_set(one), "{u:?}");
        }
    }

    #[test]
    fn a_message_of_every_key_encodes_to_the_same_bytes_as_the_old_encoder() {
        // One value per key, a few ways, since a set carries one value per key.
        let every = every_value();
        for round in 0..4 {
            let mut by_key: BTreeMap<&str, Update> = BTreeMap::new();
            for (i, u) in every.iter().enumerate() {
                if i % 4 == round % 4 || !by_key.contains_key(u.key()) {
                    by_key.insert(u.key(), u.clone());
                }
            }
            let message: Vec<Update> = by_key.into_values().collect();
            assert_eq!(message.len(), KEYS.len());
            assert_eq!(encode_set(&message), legacy::encode_set(&message));
        }
        assert_eq!(encode_set(&[]), legacy::encode_set(&[]));
        assert_eq!(encode_get(), legacy::encode_get());
    }

    #[test]
    fn replies_encode_to_the_same_bytes_as_the_old_encoder() {
        let mut new = Reply { ok: true, applied: vec!["throttle".into(), "title_bar".into()], ..Reply::default() };
        let mut old = legacy::Reply { ok: true, applied: new.applied.clone(), ..legacy::Reply::default() };
        assert_eq!(new.encode(), old.encode());
        new.ignored = vec!["warp_drive".into()];
        old.ignored = new.ignored.clone();
        assert_eq!(new.encode(), old.encode());
        for (k, v) in [("throttle", Value::from("off")), ("gamemode", Value::from(true)), ("audio_output", Value::from(""))] {
            new.values.insert(k.into(), v.clone());
            old.values.insert(k.into(), v);
        }
        assert_eq!(new.encode(), old.encode());
        // `notes` is the field the version-0 reply gained for audio; it must
        // still ride on the wire.
        new.notes.insert("audio_output".into(), "nothing was playing".into());
        old.notes.insert("audio_output".into(), "nothing was playing".into());
        assert_eq!(new.encode(), old.encode());
        assert!(new.encode().contains("\"notes\":{\"audio_output\":\"nothing was playing\"}"));
        new.error = Some("nope".into());
        old.error = Some("nope".into());
        assert_eq!(new.encode(), old.encode());
        assert_eq!(Reply::failure("nope").encode(), legacy::Reply { error: Some("nope".into()), ..legacy::Reply::default() }.encode());
        assert_eq!(Reply::default().encode(), legacy::Reply::default().encode());
    }

    #[test]
    fn the_line_limit_and_version_are_the_ones_the_old_module_had() {
        assert_eq!(MAX_LINE, 1024);
        assert_eq!((SOCKET_DIR, SOCKET_NAME), ("live", "settings.sock"));
    }

    #[test]
    fn the_two_paths_are_the_same_types() {
        // A re-export, not a copy: a value built through either path is accepted
        // by the other without a conversion.
        let u: cordial_protocol::Update = Update::Throttle(Throttle::Off);
        let _: Update = u;
        let t: crate::title_bar::TitleBar = TitleBar::Compact;
        let _: cordial_protocol::TitleBar = t;
        let f: crate::frame_rate_limit::FrameRateLimit = FrameRateLimit::Cap90;
        let _: cordial_protocol::FrameRateLimit = f;
    }
}
