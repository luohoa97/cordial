//! Typed payloads for the version-1 verbs and events (spec section 5).
//!
//! Every payload is a plain struct with `serde` derives and **no**
//! `deny_unknown_fields`: a receiver ignores fields it does not know, which is
//! how a later minor adds some. What a receiver does refuse is a field of the
//! wrong type, a bound broken, or a closed enumeration given a value outside it.
//!
//! The verb set is closed per spec version. There is nothing here that carries
//! engine memory, code, a command, a descriptor or a generic "set raw" or
//! "call", and exactly one message carries a path: [`AssetsOverlaySet`].

use crate::error::Violation;
use crate::frame::{Event, Reply, Request};
use crate::settings::{parse_set_body, set_body, Applies, SetBody, Update};
use crate::version::{Capabilities, Protocol};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Verb and event names.
pub mod names {
    // Requests, launcher to runtime. Version 1 defines no runtime-to-launcher
    // requests; the shape is reserved.
    pub const HELLO: &str = "hello";
    pub const LIFECYCLE_STOP: &str = "lifecycle.stop";
    pub const STATE_GET: &str = "state.get";
    pub const SETTINGS_SET: &str = "settings.set";
    pub const SETTINGS_GET: &str = "settings.get";
    pub const FLAGS_APPLY: &str = "flags.apply";
    pub const FLAGS_LIVE: &str = "flags.live";
    pub const ASSETS_OVERLAY_SET: &str = "assets.overlay.set";
    pub const ASSETS_OVERLAY_CLEAR: &str = "assets.overlay.clear";
    pub const DIAGNOSTICS_GET: &str = "diagnostics.get";

    // Events. `BYE` may be sent by either side.
    pub const BYE: &str = "bye";
    pub const LIFECYCLE_READY: &str = "lifecycle.ready";
    pub const HEALTH: &str = "health";
    pub const EVENTS_DROPPED: &str = "events.dropped";
    pub const GAME_JOINED: &str = "game.joined";
    pub const GAME_LEFT: &str = "game.left";
    pub const SESSION_STATE: &str = "session.state";
    pub const ENGINE_VERSION: &str = "engine.version";
    pub const GAME_PRESENCE: &str = "game.presence";
}

/// Capability names (spec section 5).
pub mod caps {
    pub const LIFECYCLE: &str = "lifecycle";
    pub const EVENTS_CORE: &str = "events.core";
    pub const EVENTS_PRESENCE: &str = "events.presence";
    pub const STATE: &str = "state";
    pub const SETTINGS: &str = "settings";
    pub const FLAGS: &str = "flags";
    pub const ASSETS_OVERLAY: &str = "assets.overlay";
    pub const DIAGNOSTICS: &str = "diagnostics";

    /// The one capability a runtime must offer.
    pub const REQUIRED: &str = LIFECYCLE;
}

/// What a message needs to be live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Part of the transport and handshake; no capability gates it.
    Always,
    /// Only when this capability is in the negotiated set.
    Capability(&'static str),
}

/// The gate for a known verb or event name, or `None` for a name this spec does
/// not define. An unknown request is answered `unsupported` and an unknown
/// event is ignored; a name the spec defines but whose capability was not
/// negotiated is the same `unsupported`, and a launcher never sends one.
pub fn gate(name: &str) -> Option<Gate> {
    use names::*;
    Some(match name {
        HELLO | EVENTS_DROPPED => Gate::Always,
        LIFECYCLE_STOP | BYE | LIFECYCLE_READY | HEALTH => Gate::Capability(caps::LIFECYCLE),
        GAME_JOINED | GAME_LEFT | SESSION_STATE | ENGINE_VERSION => Gate::Capability(caps::EVENTS_CORE),
        GAME_PRESENCE => Gate::Capability(caps::EVENTS_PRESENCE),
        STATE_GET => Gate::Capability(caps::STATE),
        SETTINGS_SET | SETTINGS_GET => Gate::Capability(caps::SETTINGS),
        FLAGS_APPLY | FLAGS_LIVE => Gate::Capability(caps::FLAGS),
        ASSETS_OVERLAY_SET | ASSETS_OVERLAY_CLEAR => Gate::Capability(caps::ASSETS_OVERLAY),
        DIAGNOSTICS_GET => Gate::Capability(caps::DIAGNOSTICS),
        _ => return None,
    })
}

/// Whether `name` may be used given the negotiated capabilities.
pub fn offered(name: &str, live: &Capabilities) -> bool {
    match gate(name) {
        Some(Gate::Always) => true,
        Some(Gate::Capability(c)) => live.contains_key(c),
        None => false,
    }
}

/// Longest string a message may carry, by verb (spec section 2: 512 bytes
/// "unless a capability says otherwise"). The one exception is the path in
/// `assets.overlay.set`, which is a filesystem path and may be as long as one.
pub fn string_limit(name: &str) -> usize {
    match name {
        names::ASSETS_OVERLAY_SET => MAX_PATH,
        _ => crate::frame::MAX_STRING,
    }
}

/// `PATH_MAX` on Linux.
pub const MAX_PATH: usize = 4096;
/// `diagnostics.get` returns at most this many lines.
pub const MAX_DIAGNOSTIC_LINES: usize = 200;
/// ... of at most this many bytes each.
pub const MAX_DIAGNOSTIC_LINE: usize = 256;

/// Checks a payload's own rules beyond what its types already guarantee.
pub trait Validate {
    fn validate(&self) -> Result<(), Violation>;
}

/// Read a payload. An absent payload reads as `{}`, so a verb with no fields can
/// be sent either way. The result is validated.
pub fn payload<T: DeserializeOwned + Validate>(p: &Value) -> Result<T, Violation> {
    let v: T = payload_unvalidated(p)?;
    v.validate()?;
    Ok(v)
}

fn payload_unvalidated<T: DeserializeOwned>(p: &Value) -> Result<T, Violation> {
    serde_json::from_value(crate::frame::object_or_empty(p)).map_err(|e| Violation::new("p", e.to_string()))
}

impl Request {
    /// This request's payload as `T`, validated.
    pub fn params<T: DeserializeOwned + Validate>(&self) -> Result<T, Violation> {
        payload(&self.p)
    }
}

impl Event {
    pub fn payload<T: DeserializeOwned + Validate>(&self) -> Result<T, Violation> {
        payload(&self.p)
    }
}

impl Reply {
    /// The payload of an ok reply as `T`. An error reply is a violation here;
    /// match on `result` first when either is possible.
    pub fn payload<T: DeserializeOwned + Validate>(&self) -> Result<T, Violation> {
        match &self.result {
            Ok(p) => payload(p),
            Err(e) => Err(Violation::new("e", format!("an error reply: {}", e.code))),
        }
    }
}

/// A request with a typed payload.
pub fn request<T: Serialize>(id: u64, name: &str, payload: &T) -> Request {
    Request::new(id, name, to_value(payload))
}

pub fn event<T: Serialize>(name: &str, n: u64, payload: &T) -> Event {
    Event::new(name, n, to_value(payload))
}

pub fn reply_ok<T: Serialize>(id: u64, payload: &T) -> Reply {
    Reply::ok(id, to_value(payload))
}

/// A payload as the JSON value a frame carries. Public so a runtime that queues
/// events by name and value (see [`crate::queue::EventQueue`]) need not depend on
/// `serde` itself to build the value.
pub fn to_value<T: Serialize>(t: &T) -> Value {
    serde_json::to_value(t).expect("a payload always serialises")
}

/// An empty payload, for verbs and replies that carry nothing.
pub fn empty() -> Value {
    Value::Object(Map::new())
}

// ---- handshake --------------------------------------------------------------

/// The launcher's `hello` request (spec section 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: Protocol,
    /// The launcher's version, such as `"0.25.0"`.
    pub cordial: String,
    pub session: String,
    /// What the launcher can serve.
    pub caps: Capabilities,
    /// Sent by a launcher that restarted and found the runtime still running.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reattach: bool,
}

/// Who the runtime says it is. The launcher's keyring entries and its report use
/// the **manifest's** `id`, never this one; a handshake whose id differs from
/// the manifest is refused ([`crate::manifest::Manifest::check_identity`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeIdent {
    pub id: String,
    pub version: String,
}

/// The engine client the runtime drives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientIdent {
    pub name: String,
    pub version: String,
    pub build: String,
}

/// The runtime's answer to `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReply {
    pub protocol: Protocol,
    pub runtime: RuntimeIdent,
    pub client: ClientIdent,
    /// What the runtime offers.
    pub caps: Capabilities,
}

impl Validate for Hello {
    fn validate(&self) -> Result<(), Violation> {
        if self.session.is_empty() {
            return Err(Violation::new("p.session", "empty"));
        }
        Ok(())
    }
}

impl Validate for HelloReply {
    fn validate(&self) -> Result<(), Violation> {
        if self.runtime.id.is_empty() {
            return Err(Violation::new("p.runtime.id", "empty"));
        }
        if !self.caps.contains_key(caps::REQUIRED) {
            return Err(Violation::new("p.caps", format!("a runtime offers {}", caps::REQUIRED)));
        }
        Ok(())
    }
}

// ---- lifecycle --------------------------------------------------------------

/// `lifecycle.stop`: ask for a clean exit. The launcher sends SIGTERM after the
/// grace, and SIGKILL two seconds later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleStop {
    pub grace_ms: u64,
}

/// `bye`, from either side, before closing. `reason` is free text bounded by
/// the usual string limit; `"superseded"` is the one the spec names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bye {
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    /// The engine stopped making progress while the control thread kept
    /// answering.
    Stalled,
}

/// `health`: optional, so the launcher need not read the runtime's log to
/// notice a wedged engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub state: HealthState,
    pub what: String,
}

// ---- events.core, events.presence, state ------------------------------------

/// `game.joined`. `at` is Unix time in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameJoined {
    pub place_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universe_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub at: u64,
}

/// `game.left`. `at` is Unix time in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameLeft {
    pub at: u64,
}

/// `session.state`. **Never the token**: this says whether somebody is signed
/// in and, at most, which account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub signed_in: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineVersion {
    pub version: String,
}

/// `events.dropped`: the runtime's bounded queue was full and this many events
/// were lost since the last report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsDropped {
    pub count: u64,
}

/// `game.presence`: the BloxstrapRPC presence as the game has built it up so
/// far, folded from a stream of partial updates. Only fields the game set are
/// present; an empty string is the game clearing one. The picture fields carry
/// Cordial's own key for an image, never an asset id or a URL.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GamePresence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub large_image_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub large_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_image_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_text: Option<String>,
}

/// The reply to `state.get`: the latest value of each event above, for a
/// launcher that reattached. A field is absent until its event has happened.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StateSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_joined: Option<GameJoined>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_left: Option<GameLeft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_state: Option<SessionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<EngineVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_presence: Option<GamePresence>,
}

// ---- settings ---------------------------------------------------------------

/// `settings.set`: a key-to-value object over Cordial's closed key set, the same
/// words and values the launch environment uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSet {
    pub updates: Vec<Update>,
    /// Keys this runtime does not know. Reported back in the reply's `ignored`;
    /// the known keys are still applied.
    pub ignored: Vec<String>,
}

impl SettingsSet {
    pub fn new(updates: Vec<Update>) -> Self {
        SettingsSet { updates, ignored: Vec::new() }
    }

    /// The payload to send.
    pub fn to_payload(&self) -> Value {
        Value::Object(set_body(&self.updates).into_iter().collect())
    }

    /// Read a received payload. A known key with an unusable value refuses the
    /// whole message: half-applying one would leave the sender believing one thing
    /// and the receiver another.
    pub fn from_payload(p: &Value) -> Result<Self, Violation> {
        let Value::Object(map) = crate::frame::object_or_empty(p) else {
            return Err(Violation::new("p", "settings.set takes an object"));
        };
        let SetBody { updates, ignored } = parse_set_body(&map).map_err(|e| Violation::new("p", e))?;
        Ok(SettingsSet { updates, ignored })
    }
}

/// The reply to `settings.set`. `applied` names what took; `notes` carries
/// anything applied with a caveat, so "applied" is never read as "you will hear
/// it".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SettingsReply {
    pub applied: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
}

/// The reply to `settings.get`: what is in force now, and how each key reaches
/// a running game on this runtime.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SettingsGetReply {
    pub values: BTreeMap<String, Value>,
    #[serde(default)]
    pub declared: BTreeMap<String, Applies>,
}

// ---- flags ------------------------------------------------------------------

/// `flags.apply`, sent after the launcher writes the resolved document to
/// `{session_dir}/flags.json`. The path is not in the message: it is a
/// convention, so a runtime cannot be handed a different one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagsApply {
    /// Lowercase hex SHA-256 of the document, so the runtime can tell it read
    /// the one that was announced.
    pub sha256: String,
    /// How many flags the document holds.
    pub count: u64,
}

/// A flag value: the three shapes `ClientAppSettings.json` carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FlagValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

/// `flags.live`: `DF*` names only, applied to the running engine and held in
/// force against the engine's own refresh (ADR-051).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagsLive {
    pub flags: BTreeMap<String, FlagValue>,
}

/// The reply to `flags.live`: applied or ignored, per name.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FlagsLiveReply {
    pub applied: Vec<String>,
    #[serde(default)]
    pub ignored: Vec<String>,
}

// ---- assets.overlay ---------------------------------------------------------

/// `assets.overlay.set`: **the one message that carries a path.** `root` is an
/// absolute directory the launcher has canonicalised, checked to sit inside a
/// plugin's install directory, and confirmed to exist. The runtime treats it as
/// read-only and confines its reads to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsOverlaySet {
    pub plugin: String,
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsOverlayClear {
    pub plugin: String,
}

// ---- diagnostics ------------------------------------------------------------

/// The reply to `diagnostics.get`: at most 200 ordered, redacted lines of 256
/// bytes. The report always names the runtime id and `support_url` itself.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiagnosticsReply {
    pub lines: Vec<String>,
}

// ---- validation -------------------------------------------------------------

macro_rules! nothing_to_check {
    ($($t:ty),* $(,)?) => { $(impl Validate for $t { fn validate(&self) -> Result<(), Violation> { Ok(()) } })* };
}
nothing_to_check!(
    LifecycleStop,
    Bye,
    Health,
    GameJoined,
    GameLeft,
    SessionState,
    EngineVersion,
    EventsDropped,
    GamePresence,
    StateSnapshot,
    SettingsReply,
    SettingsGetReply,
    FlagsLiveReply,
    AssetsOverlayClear,
);

impl Validate for FlagsApply {
    fn validate(&self) -> Result<(), Violation> {
        let hex = self.sha256.len() == 64 && self.sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if hex {
            Ok(())
        } else {
            Err(Violation::new("p.sha256", "64 lowercase hex digits"))
        }
    }
}

impl Validate for FlagsLive {
    fn validate(&self) -> Result<(), Violation> {
        for name in self.flags.keys() {
            // `DF*` only: a live flag is a dynamic one. The static `F*` flags are
            // read once at startup, and applying one to a running engine would
            // be a claim the engine cannot keep.
            if !name.starts_with("DF") || name.len() < 3 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err(Violation::new(format!("p.flags.{name}"), "flags.live takes DF* names only"));
            }
        }
        Ok(())
    }
}

impl Validate for AssetsOverlaySet {
    fn validate(&self) -> Result<(), Violation> {
        if self.plugin.is_empty() {
            return Err(Violation::new("p.plugin", "empty"));
        }
        if !self.root.starts_with('/') || self.root.contains('\0') {
            return Err(Violation::new("p.root", "an absolute path, with no NUL"));
        }
        // The launcher canonicalised this, so `..` should never arrive; refusing
        // it here costs nothing and means a runtime does not depend on that.
        if self.root.split('/').any(|c| c == "..") {
            return Err(Violation::new("p.root", "no `..` component"));
        }
        if self.root.len() > MAX_PATH {
            return Err(Violation::new("p.root", format!("over {MAX_PATH} bytes")));
        }
        Ok(())
    }
}

impl Validate for DiagnosticsReply {
    fn validate(&self) -> Result<(), Violation> {
        if self.lines.len() > MAX_DIAGNOSTIC_LINES {
            return Err(Violation::new("p.lines", format!("{} lines is over {MAX_DIAGNOSTIC_LINES}", self.lines.len())));
        }
        if let Some((i, l)) = self.lines.iter().enumerate().find(|(_, l)| l.len() > MAX_DIAGNOSTIC_LINE) {
            return Err(Violation::new(format!("p.lines[{i}]"), format!("{} bytes is over {MAX_DIAGNOSTIC_LINE}", l.len())));
        }
        Ok(())
    }
}

/// Which side of the wire a message travels, for [`check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Request,
    Event,
}

/// Validate a request or event payload against the spec, if the name is one it
/// defines. An unknown name passes: unknown requests are answered `unsupported`
/// and unknown events are ignored, and neither is a violation.
pub fn check(kind: Kind, name: &str, p: &Value) -> Result<(), Violation> {
    use names::*;
    fn ok<T: DeserializeOwned + Validate>(p: &Value) -> Result<(), Violation> {
        payload::<T>(p).map(|_| ())
    }
    match (kind, name) {
        (Kind::Request, HELLO) => ok::<Hello>(p),
        (Kind::Request, LIFECYCLE_STOP) => ok::<LifecycleStop>(p),
        (Kind::Request, SETTINGS_SET) => SettingsSet::from_payload(p).map(|_| ()),
        (Kind::Request, FLAGS_APPLY) => ok::<FlagsApply>(p),
        (Kind::Request, FLAGS_LIVE) => ok::<FlagsLive>(p),
        (Kind::Request, ASSETS_OVERLAY_SET) => ok::<AssetsOverlaySet>(p),
        (Kind::Request, ASSETS_OVERLAY_CLEAR) => ok::<AssetsOverlayClear>(p),
        (Kind::Event, BYE) => ok::<Bye>(p),
        (Kind::Event, HEALTH) => ok::<Health>(p),
        (Kind::Event, EVENTS_DROPPED) => ok::<EventsDropped>(p),
        (Kind::Event, GAME_JOINED) => ok::<GameJoined>(p),
        (Kind::Event, GAME_LEFT) => ok::<GameLeft>(p),
        (Kind::Event, SESSION_STATE) => ok::<SessionState>(p),
        (Kind::Event, ENGINE_VERSION) => ok::<EngineVersion>(p),
        (Kind::Event, GAME_PRESENCE) => ok::<GamePresence>(p),
        _ => Ok(()),
    }?;
    // The string bound, with the one capability-specific exception.
    crate::frame::check_value("p", p, string_limit(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{decode_line, encode_line, Frame};
    use crate::settings::{Accel, Throttle};
    use serde_json::json;

    fn caps_of(list: &[(&str, u32)]) -> Capabilities {
        list.iter().map(|(n, v)| (n.to_string(), *v)).collect()
    }

    #[test]
    fn hello_matches_the_spec_shape_and_reattach_is_sent_only_when_true() {
        let hello = Hello {
            protocol: Protocol::new(1, 0),
            cordial: "0.25.0".into(),
            session: "a1b2c3d4".into(),
            caps: caps_of(&[("lifecycle", 1), ("settings", 1)]),
            reattach: false,
        };
        let line = encode_line(&Frame::Request(request(1, names::HELLO, &hello)));
        // Compared as JSON, not as bytes: key order inside a payload is not part
        // of the protocol, and a build that turns on `serde_json/preserve_order`
        // changes it.
        let sent: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            sent,
            json!({"id": 1, "m": "hello", "p": {"protocol": {"major": 1, "minor": 0}, "cordial": "0.25.0", "session": "a1b2c3d4", "caps": {"lifecycle": 1, "settings": 1}}})
        );
        let Frame::Request(back) = decode_line(&line).unwrap() else { panic!() };
        assert_eq!(back.params::<Hello>().unwrap(), hello);
        let again = Hello { reattach: true, ..hello };
        assert!(serde_json::to_string(&again).unwrap().contains("\"reattach\":true"));
    }

    #[test]
    fn a_hello_reply_must_offer_lifecycle_and_name_itself() {
        let mut r = HelloReply {
            protocol: Protocol::new(1, 0),
            runtime: RuntimeIdent { id: "org.example.runtime".into(), version: "0.13".into() },
            client: ClientIdent { name: "Roblox".into(), version: "2.700".into(), build: "700".into() },
            caps: caps_of(&[("lifecycle", 1)]),
        };
        assert!(r.validate().is_ok());
        r.caps.clear();
        assert!(r.validate().is_err(), "lifecycle is the one required capability");
        r.caps.insert("lifecycle".into(), 1);
        r.runtime.id.clear();
        assert!(r.validate().is_err());
    }

    #[test]
    fn core_events_decode_from_the_spec_shapes_and_ignore_what_they_do_not_know() {
        let joined: GameJoined = payload(&json!({"place_id": 1818, "at": 1700000000000u64, "later": true})).unwrap();
        assert_eq!(joined, GameJoined { place_id: 1818, universe_id: None, job_id: None, at: 1700000000000 });
        assert!(payload::<GameJoined>(&json!({"place_id": "1818", "at": 1})).is_err(), "ids are integers");
        assert!(payload::<GameJoined>(&json!({"place_id": -1, "at": 1})).is_err());
        assert!(payload::<GameJoined>(&json!({"at": 1})).is_err(), "place_id is required");
        let s: SessionState = payload(&json!({"signed_in": true, "user_id": 7})).unwrap();
        assert_eq!(s.user_id, Some(7));
        assert!(payload::<SessionState>(&json!({"signed_in": "yes"})).is_err());
    }

    #[test]
    fn health_state_is_a_closed_set() {
        assert!(payload::<Health>(&json!({"state": "stalled", "what": "no frames"})).is_ok());
        assert!(payload::<Health>(&json!({"state": "on fire", "what": "x"})).is_err());
    }

    #[test]
    fn settings_set_uses_the_launch_environment_words_and_reports_unknown_keys() {
        let set = SettingsSet::from_payload(&json!({"throttle": "off", "warp_drive": "on"})).unwrap();
        assert_eq!(set.updates, vec![Update::Throttle(Throttle::Off)]);
        assert_eq!(set.ignored, vec!["warp_drive".to_string()]);
        let p = SettingsSet::new(vec![Update::PointerAcceleration(Accel::Always), Update::Gamepad(true)]).to_payload();
        assert_eq!(p, json!({"gamepad": true, "pointer_acceleration": "always"}));
        assert!(SettingsSet::from_payload(&json!({"throttle": "sometimes"})).is_err(), "a bad value refuses the lot");
        assert!(SettingsSet::from_payload(&json!([1])).is_err());
    }

    #[test]
    fn settings_replies_name_applied_keys_and_carry_notes() {
        let r = SettingsReply {
            applied: vec!["audio_output".into()],
            ignored: vec![],
            notes: BTreeMap::from([("audio_output".to_string(), "nothing was playing".to_string())]),
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"applied":["audio_output"],"notes":{"audio_output":"nothing was playing"}}"#
        );
    }

    #[test]
    fn flags_apply_wants_a_real_digest() {
        let good = "0".repeat(63) + "f";
        assert!(payload::<FlagsApply>(&json!({"sha256": good, "count": 3})).is_ok());
        for bad in ["", "abc", &"G".repeat(64), &"A".repeat(64)] {
            assert!(payload::<FlagsApply>(&json!({"sha256": bad, "count": 3})).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn flags_live_takes_dynamic_names_only() {
        let ok: FlagsLive = payload(&json!({"flags": {"DFIntTaskSchedulerTargetFps": 144, "DFFlagX": true, "DFStringY": "v"}})).unwrap();
        assert_eq!(ok.flags["DFIntTaskSchedulerTargetFps"], FlagValue::Int(144));
        assert_eq!(ok.flags["DFFlagX"], FlagValue::Bool(true));
        for bad in ["FIntX", "DF", "dfFlagX", "DFFlag X", "../DFFlag"] {
            assert!(payload::<FlagsLive>(&json!({"flags": {bad: 1}})).is_err(), "{bad:?}");
        }
        assert!(payload::<FlagsLive>(&json!({"flags": {"DFX": [1]}})).is_err(), "a flag value is a scalar");
    }

    #[test]
    fn the_overlay_root_is_an_absolute_path_and_the_only_long_string() {
        assert!(payload::<AssetsOverlaySet>(&json!({"plugin": "p", "root": "/a/b"})).is_ok());
        for root in ["rel/path", "", "/a/../b", "/a\u{0}b"] {
            assert!(payload::<AssetsOverlaySet>(&json!({"plugin": "p", "root": root})).is_err(), "{root:?}");
        }
        let long = format!("/{}", "a".repeat(1000));
        assert!(check(Kind::Request, names::ASSETS_OVERLAY_SET, &json!({"plugin": "p", "root": long})).is_ok());
        assert!(check(Kind::Request, names::ASSETS_OVERLAY_CLEAR, &json!({"plugin": long})).is_err(), "512 elsewhere");
        assert_eq!(string_limit(names::ASSETS_OVERLAY_SET), MAX_PATH);
    }

    #[test]
    fn diagnostics_are_bounded_in_lines_and_in_line_length() {
        let lines = |n: usize, len: usize| DiagnosticsReply { lines: vec!["x".repeat(len); n] };
        assert!(lines(200, 256).validate().is_ok());
        assert!(lines(201, 1).validate().is_err());
        assert!(lines(1, 257).validate().is_err());
    }

    #[test]
    fn every_defined_name_has_a_gate_and_unknown_names_have_none() {
        use names::*;
        for n in [
            HELLO, LIFECYCLE_STOP, STATE_GET, SETTINGS_SET, SETTINGS_GET, FLAGS_APPLY, FLAGS_LIVE, ASSETS_OVERLAY_SET,
            ASSETS_OVERLAY_CLEAR, DIAGNOSTICS_GET, BYE, LIFECYCLE_READY, HEALTH, EVENTS_DROPPED, GAME_JOINED, GAME_LEFT,
            SESSION_STATE, ENGINE_VERSION, GAME_PRESENCE,
        ] {
            assert!(gate(n).is_some(), "{n}");
        }
        assert!(gate("exec").is_none());
        assert!(gate("settings.raw").is_none());
    }

    #[test]
    fn a_capability_that_was_not_negotiated_is_not_offered() {
        let live = caps_of(&[("lifecycle", 1), ("settings", 1)]);
        assert!(offered(names::HELLO, &live));
        assert!(offered(names::SETTINGS_SET, &live));
        assert!(offered(names::LIFECYCLE_STOP, &live));
        assert!(!offered(names::FLAGS_APPLY, &live));
        assert!(!offered("exec", &live));
    }

    #[test]
    fn check_passes_unknown_names_and_refuses_bad_known_payloads() {
        assert!(check(Kind::Request, "x-vendor.thing", &json!({"anything": 1})).is_ok());
        assert!(check(Kind::Event, "x-vendor.thing", &Value::Null).is_ok());
        assert!(check(Kind::Event, names::GAME_LEFT, &json!({"at": "now"})).is_err());
        assert!(check(Kind::Event, names::GAME_LEFT, &Value::Null).is_err(), "at is required");
        assert!(check(Kind::Request, names::STATE_GET, &Value::Null).is_ok());
    }

    #[test]
    fn a_presence_carries_only_what_the_game_set() {
        let p = GamePresence { details: Some("In the lobby".into()), end: Some(0), ..GamePresence::default() };
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"details": "In the lobby", "end": 0}));
    }
}
