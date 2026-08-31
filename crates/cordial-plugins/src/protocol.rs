//! The wire protocol between Cordial and a plugin.
//!
//! Newline-delimited JSON over the plugin's stdin and stdout. Chosen because it
//! is debuggable by eye and by `cat`, works with any language, and needs no
//! shared memory — which matters, because ADR-003 rules out plugins having
//! memory access to Cordial and a shared-memory transport would be the first
//! step back toward it.
//!
//! Every request names a capability. The broker checks it before the call is
//! dispatched, so a plugin cannot reach an effect by naming a method it was not
//! granted.

use crate::capability::Capability;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    /// Correlates the response. Plugins may have several calls in flight.
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Response {
    Ok { id: u64, result: serde_json::Value },
    /// The call was refused. `denied` is distinct from `error` on purpose: a
    /// plugin author needs to tell "I was not allowed" from "it went wrong",
    /// and collapsing them produces bug reports about the wrong thing.
    Denied { id: u64, capability: String },
    Error { id: u64, message: String },
}

impl Response {
    pub fn id(&self) -> u64 {
        match self {
            Response::Ok { id, .. } | Response::Denied { id, .. } | Response::Error { id, .. } => {
                *id
            }
        }
    }
}

/// A message the host sends without being asked, because something happened
/// on Cordial's own timeline rather than because the plugin made a call — a
/// lifecycle event, or another plugin's published event arriving for a
/// subscriber. Distinct from `Response` on the wire: a `Response` always
/// carries `status` and answers a specific request `id`; a `Push` carries
/// neither, so a plugin's dispatcher can tell "this is a reply I am waiting
/// for" from "this arrived on its own" just by checking which shape a line
/// deserialises as.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Push {
    pub event: String,
    pub payload: serde_json::Value,
}

/// Which capability a method requires, or `None` if the method is unknown.
///
/// A closed mapping rather than a convention like "flags.* needs flags": a
/// typo in a method name must fail as unknown, not fall through to a capability
/// check that happens to pass.
pub fn required_capability(method: &str) -> Option<Capability> {
    Some(match method {
        "flags.list" => Capability::FlagsRead,
        "flags.get" => Capability::FlagsRead,
        "flags.set" => Capability::FlagsWrite,
        "flags.setDynamic" => Capability::FlagsWriteDynamic,
        "log.write" => Capability::Log,
        "lifecycle.subscribe" => Capability::LifecycleRead,
        // A snapshot, so a plugin that starts mid-session is not blind until
        // the next event. See `state::SessionState`.
        "state.get" => Capability::StateRead,
        "presence.set" => Capability::PresenceSet,
        "presence.clear" => Capability::PresenceSet,
        "notify.send" => Capability::NotifySend,
        "url.open" => Capability::UrlOpen,
        "assets.override" => Capability::AssetsOverride,
        "settings.get" => Capability::SettingsRead,
        // The user's answers to the questions this plugin's own manifest
        // asked. Under `SettingsRead` rather than a capability of its own:
        // both are "read what Cordial keeps for you", the data is the
        // plugin's own by construction, and a separate permission a user
        // could deny would leave a plugin declaring questions it cannot hear
        // the answers to. There is deliberately no `preferences.set` -- see
        // ADR-020; Cordial is the only writer.
        "preferences.get" => Capability::SettingsRead,
        "settings.set" => Capability::SettingsWrite,
        "events.declare" => Capability::EventsDeclare,
        "events.publish" => Capability::EventsPublish,
        "events.subscribe" => Capability::EventsSubscribe,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips() {
        let r = Request { id: 7, method: "flags.get".into(), params: serde_json::json!({"k": "v"}) };
        let line = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), r);
    }

    #[test]
    fn params_may_be_omitted() {
        let r: Request = serde_json::from_str(r#"{"id":1,"method":"flags.list"}"#).unwrap();
        assert_eq!(r.params, serde_json::Value::Null);
    }

    #[test]
    fn denied_is_not_an_error() {
        let d = Response::Denied { id: 3, capability: "flags.write".into() };
        let line = serde_json::to_string(&d).unwrap();
        assert!(line.contains(r#""status":"denied""#));
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), d);
    }

    #[test]
    fn presence_is_one_capability_covering_set_and_clear() {
        // Clearing presence is not a lesser power than setting it — both say
        // something about what the user is doing — so they share a capability
        // rather than inviting a plugin to ask for two.
        assert_eq!(required_capability("presence.set"), Some(Capability::PresenceSet));
        assert_eq!(required_capability("presence.clear"), Some(Capability::PresenceSet));
    }

    #[test]
    fn each_brokered_effect_is_its_own_capability() {
        // Presence, notifications and opening a URL all happen to be brokered
        // over the same kind of host resource, which is exactly why they must
        // not share a capability — a plugin granted "tell me when a server
        // shuffles" would otherwise also be able to open pages in the browser.
        assert_eq!(required_capability("notify.send"), Some(Capability::NotifySend));
        assert_eq!(required_capability("url.open"), Some(Capability::UrlOpen));
        assert_ne!(required_capability("notify.send"), required_capability("url.open"));
        assert_ne!(required_capability("notify.send"), required_capability("presence.set"));
    }

    #[test]
    fn registering_an_asset_overlay_needs_its_own_capability() {
        // Distinct from every other capability for the same reason
        // notify.send and url.open are distinct from each other: a plugin
        // granted something unrelated should not incidentally be able to
        // shadow Roblox's own files.
        assert_eq!(required_capability("assets.override"), Some(Capability::AssetsOverride));
    }

    #[test]
    fn reading_a_plugins_own_settings_is_a_different_capability_from_rewriting_them() {
        // The same split as flags.read against flags.write, and it has to be
        // real at the method mapping and not only in the enum: a plugin
        // granted settings.read that could reach settings.set would be able to
        // discard configuration a user spent time on, from a grant that reads
        // like permission to remember a window size.
        assert_eq!(required_capability("settings.get"), Some(Capability::SettingsRead));
        assert_eq!(required_capability("settings.set"), Some(Capability::SettingsWrite));
        assert_ne!(required_capability("settings.get"), required_capability("settings.set"));
    }

    #[test]
    fn an_unknown_method_maps_to_no_capability() {
        assert!(required_capability("flags.delete_everything").is_none());
        assert!(required_capability("flags").is_none());
    }

    #[test]
    fn setting_a_live_flag_needs_its_own_capability() {
        assert_eq!(required_capability("flags.set"), Some(Capability::FlagsWrite));
        assert_eq!(
            required_capability("flags.setDynamic"),
            Some(Capability::FlagsWriteDynamic)
        );
    }

    #[test]
    fn declaring_publishing_and_subscribing_are_three_separate_capabilities() {
        // ADR-006 is explicit that these three must not collapse into one: a
        // plugin that can only subscribe must not thereby be able to publish,
        // and a plugin that can publish must have declared first.
        assert_eq!(required_capability("events.declare"), Some(Capability::EventsDeclare));
        assert_eq!(required_capability("events.publish"), Some(Capability::EventsPublish));
        assert_eq!(required_capability("events.subscribe"), Some(Capability::EventsSubscribe));
        assert_ne!(required_capability("events.declare"), required_capability("events.publish"));
        assert_ne!(required_capability("events.publish"), required_capability("events.subscribe"));
    }

    #[test]
    fn a_push_is_shaped_differently_from_a_response_on_the_wire() {
        // A plugin's dispatcher tells a push from a reply by shape alone —
        // no `status`, no `id` — so this must never accidentally grow either
        // field and become ambiguous with a `Response`.
        let push = Push { event: "flag-manager/profile-changed".into(), payload: serde_json::json!({"slot": 2}) };
        let line = serde_json::to_string(&push).unwrap();
        assert!(!line.contains(r#""status""#), "{line}");
        assert!(!line.contains(r#""id""#), "{line}");
        assert_eq!(serde_json::from_str::<Push>(&line).unwrap(), push);
    }
}
