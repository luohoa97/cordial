//! BloxstrapRPC: the protocol a Roblox game uses to drive the launcher's own
//! Discord presence from inside the experience.
//!
//! A game calls Lua's `print` with a marker and a JSON envelope. The engine
//! writes that to its own log under `[FLog::Output]`, at
//! `appData/logs/<version>_<stamp>_Player_<id>.log` inside the profile, and a
//! launcher that is watching the log picks it up. Nothing is injected and
//! nothing is hooked: the engine is writing a file into a directory Cordial
//! created, and Cordial is reading it. That is the same shape as every other
//! core event -- `cordial_plugins::core_events` makes the argument at length,
//! that Cordial knows what the engine is doing because it already owns the
//! sockets and the paths -- and it is why this runs into neither ADR-001 nor
//! ADR-003.
//!
//! The protocol is Bloxstrap's, and Bloxstrap is MIT (Bloxstrap Labs). What is
//! adapted here is the documented shape -- the marker, the envelope, the field
//! names and the persist rules -- and none of its implementation.
//!
//! This module is deliberately the protocol and nothing else. It opens no
//! socket, resolves no asset id to a URL and decides nothing about what Cordial
//! publishes. Those belong to the presence broker, which ADR-007 keeps on
//! Cordial's side of the line: a plugin receives the parsed result as an event
//! and asks for an effect, and never touches the log or the wire itself.

use serde_json::Value;

/// The marker a game prints ahead of the envelope.
///
/// Matched anywhere in the line rather than at its start, because by the time
/// the line reaches the log the engine has already put its own timestamp,
/// channel and thread id in front of it.
pub const MARKER: &str = "[BloxstrapRPC]";

/// Discord's limit on `details` and `state`.
///
/// Kept here as well as in `cordial_plugins::presence` so that an over-long
/// field is clamped where it arrives rather than rejected at the broker. A
/// game that writes 200 characters into `details` should lose the tail, not
/// lose its presence entirely with nobody told why -- but the broker's check
/// stays the authority, and this only avoids handing it something it will
/// refuse.
const TEXT_FIELD_LIMIT: usize = 128;

/// What one field was asked to become.
///
/// Four states rather than `Option`, because the protocol distinguishes "say
/// nothing about this field" from "make this field empty" from "put this field
/// back to the launcher's own value", and collapsing any two of those loses a
/// behaviour a game can depend on.
#[derive(Debug, Clone, PartialEq)]
pub enum Update<T> {
    /// Absent or null. The previous value stands.
    Keep,
    /// The empty string, or an image's `clear`. Show nothing.
    Clear,
    /// The literal `<reset>`, or an image's `reset`. Back to whatever Cordial
    /// would have shown on its own.
    Reset,
    Set(T),
}

impl<T> Update<T> {
    fn is_keep(&self) -> bool {
        matches!(self, Update::Keep)
    }
}

/// One of the two image slots Discord shows beside an activity.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    /// A Roblox asset id, kept as text because ids outgrew 32 bits long ago
    /// and the protocol permits either a number or a string.
    ///
    /// Validated as digits only. It ends up as a path component when something
    /// downstream resolves it, and an unvalidated string in that position is
    /// exactly the smuggling `presence.rs` refuses for `client_id`.
    pub asset_id: String,
    pub hover_text: Option<String>,
}

/// The accumulated presence, after every command seen so far.
///
/// Held as `Slot`s rather than `Option`s for the same reason `Update` has four
/// arms: "the game cleared this" and "the game never mentioned it" render
/// differently and must not merge.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Presence {
    pub details: Slot<String>,
    pub state: Slot<String>,
    pub time_start: Slot<i64>,
    pub time_end: Slot<i64>,
    pub large_image: Slot<Image>,
    pub small_image: Slot<Image>,
}

impl Slot<String> {
    /// The string to publish, or `None` when the game never mentioned it.
    ///
    /// `Empty` becomes `""` rather than `None`: the game clearing a field is a
    /// statement, and dropping it would leave whatever was there before.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Slot::Default => None,
            Slot::Empty => Some(""),
            Slot::Value(v) => Some(v),
        }
    }
}

/// A field's settled value.
#[derive(Debug, Clone, PartialEq)]
pub enum Slot<T> {
    /// Cordial's own value stands. The starting state, and where `<reset>`
    /// returns a field to.
    Default,
    /// The game asked for nothing to be shown here.
    Empty,
    Value(T),
}

// Written out rather than derived because `#[derive(Default)]` on a generic
// would bound `T: Default`, and `Slot<Image>` has no business requiring an
// `Image` that means nothing. The starting state does not depend on `T`.
impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot::Default
    }
}

impl<T> Slot<T> {
    fn apply(&mut self, update: Update<T>) {
        match update {
            Update::Keep => {}
            Update::Clear => *self = Slot::Empty,
            Update::Reset => *self = Slot::Default,
            Update::Set(v) => *self = Slot::Value(v),
        }
    }

    /// The value, if the game set one. `Default` and `Empty` both answer
    /// `None` -- they differ in what the caller should do about it, which is
    /// why they are distinct in the type and not here.
    pub fn value(&self) -> Option<&T> {
        match self {
            Slot::Value(v) => Some(v),
            _ => None,
        }
    }
}

/// A parsed command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    SetRichPresence(Box<RichPresence>),
    /// The payload behind the join button. Carried through as the game sent
    /// it; this module does not build launch URLs.
    SetLaunchData(String),
}

/// The field-by-field intent of one `SetRichPresence`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RichPresence {
    pub details: Update<String>,
    pub state: Update<String>,
    /// Both time fields are marked deprecated in Bloxstrap's own
    /// documentation. Parsed anyway, because games in the wild already send
    /// them and refusing would break presences that work today.
    pub time_start: Update<i64>,
    pub time_end: Update<i64>,
    pub large_image: Update<Image>,
    pub small_image: Update<Image>,
    /// Fields the game sent longer than Discord accepts, clamped rather than
    /// refused. Named so a caller can say so once instead of every frame.
    pub clamped: Vec<&'static str>,
}

impl<T> Default for Update<T> {
    fn default() -> Self {
        Update::Keep
    }
}

impl Presence {
    /// An empty presence: every field still Cordial's own.
    ///
    /// `const` so it can start a `static Mutex` without a lazy initialiser --
    /// `game_log` keeps one for the process, since there is one engine in it.
    pub const fn new() -> Self {
        Presence {
            details: Slot::Default,
            state: Slot::Default,
            time_start: Slot::Default,
            time_end: Slot::Default,
            large_image: Slot::Default,
            small_image: Slot::Default,
        }
    }

    /// What a subscriber to `core_events::GAME_PRESENCE` receives.
    ///
    /// **Only fields the game actually set.** `Slot::Default` means the game
    /// never mentioned it and Cordial's own value stands, so it is omitted
    /// rather than sent as null -- a subscriber that filled a field from a
    /// null would be publishing "the game asked for nothing here" as though
    /// the game had asked for it. `Slot::Empty` is the opposite and *is* sent,
    /// as an empty string, because the game explicitly cleared it.
    ///
    /// Images are deliberately not carried. They are Discord asset keys
    /// belonging to whichever application the presence is published under, and
    /// a key from a game's own Bloxstrap application means nothing under
    /// Cordial's -- Discord renders a missing key as no image at all, so
    /// forwarding them would silently blank the icon rather than improve it.
    pub fn to_payload(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        if let Some(v) = self.details.as_text() {
            map.insert("details".into(), serde_json::json!(v));
        }
        if let Some(v) = self.state.as_text() {
            map.insert("state".into(), serde_json::json!(v));
        }
        if let Slot::Value(t) = &self.time_start {
            map.insert("start".into(), serde_json::json!(t));
        }
        if let Slot::Value(t) = &self.time_end {
            map.insert("end".into(), serde_json::json!(t));
        }
        serde_json::Value::Object(map)
    }

    /// Fold a command in. Returns the launch data if that is what it was, so a
    /// caller driving both from one stream does not need to match twice.
    pub fn apply(&mut self, command: Command) -> Option<String> {
        match command {
            Command::SetLaunchData(d) => Some(d),
            Command::SetRichPresence(rp) => {
                self.details.apply(rp.details);
                self.state.apply(rp.state);
                self.time_start.apply(rp.time_start);
                self.time_end.apply(rp.time_end);
                self.large_image.apply(rp.large_image);
                self.small_image.apply(rp.small_image);
                None
            }
        }
    }
}

/// Pull a command out of one log line, if it carries one.
///
/// `Ok(None)` for a line with no marker, which is almost every line; that is
/// not an error and must not be logged as one, or a busy game's log would bury
/// the machine.
pub fn parse_line(line: &str) -> Result<Option<Command>, String> {
    let Some(at) = line.find(MARKER) else {
        return Ok(None);
    };
    let body = line[at + MARKER.len()..].trim();
    if body.is_empty() {
        return Err("marker with no envelope".into());
    }
    parse_envelope(body).map(Some)
}

fn parse_envelope(body: &str) -> Result<Command, String> {
    let value: Value = serde_json::from_str(body).map_err(|e| format!("bad envelope: {e}"))?;
    let object = value.as_object().ok_or("envelope is not an object")?;
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .ok_or("envelope has no string command")?;
    // An absent `data` is tolerated for `SetRichPresence`, where it means the
    // game asked for nothing in particular and every field keeps. It is not
    // tolerated for `SetLaunchData`, which is entirely its payload.
    let data = object.get("data");

    match command {
        "SetRichPresence" => {
            let data = match data {
                None | Some(Value::Null) => return Ok(Command::SetRichPresence(Box::default())),
                Some(v) => v.as_object().ok_or("data is not an object")?,
            };
            let mut clamped = Vec::new();
            let rp = RichPresence {
                details: text_update(data.get("details"), "details", &mut clamped)?,
                state: text_update(data.get("state"), "state", &mut clamped)?,
                time_start: int_update(data.get("timeStart"), "timeStart")?,
                time_end: int_update(data.get("timeEnd"), "timeEnd")?,
                large_image: image_update(data.get("largeImage"), "largeImage")?,
                small_image: image_update(data.get("smallImage"), "smallImage")?,
                clamped,
            };
            Ok(Command::SetRichPresence(Box::new(rp)))
        }
        "SetLaunchData" => {
            let data = data.ok_or("SetLaunchData with no data")?;
            // Bloxstrap documents this as a string. A game that sends an
            // object here has a bug, and saying so beats quietly stringifying
            // something the join button will then fail on.
            let text = data.as_str().ok_or("SetLaunchData data is not a string")?;
            Ok(Command::SetLaunchData(text.to_string()))
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

/// The sentinel that returns a field to the launcher's own value.
const RESET: &str = "<reset>";

fn text_update(
    value: Option<&Value>,
    field: &'static str,
    clamped: &mut Vec<&'static str>,
) -> Result<Update<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(Update::Keep),
        Some(Value::String(s)) if s.is_empty() => Ok(Update::Clear),
        Some(Value::String(s)) if s == RESET => Ok(Update::Reset),
        Some(Value::String(s)) => {
            // Clamped on a character boundary, not a byte one. Slicing a
            // string by byte offset behind a length check panics the moment a
            // multi-byte character straddles it, which is not hypothetical --
            // it already happened once in this codebase, in the settings
            // window's `short_reason`.
            if s.chars().count() > TEXT_FIELD_LIMIT {
                clamped.push(field);
                let end = s
                    .char_indices()
                    .nth(TEXT_FIELD_LIMIT)
                    .map(|(i, _)| i)
                    .unwrap_or(s.len());
                Ok(Update::Set(s[..end].to_string()))
            } else {
                Ok(Update::Set(s.clone()))
            }
        }
        Some(other) => Err(format!("{field} is {}, expected a string", kind(other))),
    }
}

fn int_update(value: Option<&Value>, field: &'static str) -> Result<Update<i64>, String> {
    match value {
        None | Some(Value::Null) => Ok(Update::Keep),
        // There is no empty integer, so the only way to unset a time is the
        // string sentinel -- which is why this arm accepts a string at all.
        Some(Value::String(s)) if s == RESET => Ok(Update::Reset),
        Some(Value::String(s)) if s.is_empty() => Ok(Update::Clear),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(Update::Set)
            .ok_or_else(|| format!("{field} is not a whole number of seconds")),
        Some(other) => Err(format!("{field} is {}, expected a number", kind(other))),
    }
}

fn image_update(value: Option<&Value>, field: &'static str) -> Result<Update<Image>, String> {
    let object = match value {
        None | Some(Value::Null) => return Ok(Update::Keep),
        Some(v) => v.as_object().ok_or_else(|| format!("{field} is not an object"))?,
    };

    // `clear` is checked before `reset` because the two are contradictory and
    // the documentation does not say which wins. Clearing is the stronger and
    // more surprising of the two, so a game that sets both gets the one it
    // would notice. INFERRED: no source establishes the real precedence.
    if object.get("clear").and_then(Value::as_bool) == Some(true) {
        return Ok(Update::Clear);
    }
    if object.get("reset").and_then(Value::as_bool) == Some(true) {
        return Ok(Update::Reset);
    }

    let asset_id = match object.get("assetId") {
        None | Some(Value::Null) => return Ok(Update::Keep),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(format!("{field}.assetId is {}, expected a number or string", kind(other)))
        }
    };
    if asset_id.is_empty() || !asset_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{field}.assetId is not an asset id"));
    }

    let hover_text = match object.get("hoverText") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => Some(s.chars().take(TEXT_FIELD_LIMIT).collect()),
        Some(other) => {
            return Err(format!("{field}.hoverText is {}, expected a string", kind(other)))
        }
    };

    Ok(Update::Set(Image { asset_id, hover_text }))
}

/// What a value is, for an error message. `serde_json` has no such accessor
/// and the alternative is printing the value itself, which puts a game's
/// arbitrary text into Cordial's log.
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

impl RichPresence {
    /// Whether this command asked for anything at all.
    ///
    /// A game that prints `SetRichPresence` with every field absent is telling
    /// the launcher nothing, and republishing to Discord for it would spend
    /// one of a small number of allowed updates on a no-op.
    pub fn is_empty(&self) -> bool {
        self.details.is_keep()
            && self.state.is_keep()
            && self.time_start.is_keep()
            && self.time_end.is_keep()
            && self.large_image.is_keep()
            && self.small_image.is_keep()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker never arrives at the start of a line in practice: the engine
    /// has already prefixed its timestamp and channel by then.
    #[test]
    fn finds_the_marker_after_the_engines_own_prefix() {
        let line = "2026-08-30T00:24:05.123Z,0.123456,f1d57,6 [FLog::Output] \
                    [BloxstrapRPC] {\"command\":\"SetRichPresence\",\"data\":{\"details\":\"Room 2\"}}";
        let Some(Command::SetRichPresence(rp)) = parse_line(line).unwrap() else {
            panic!("expected a rich presence command");
        };
        assert_eq!(rp.details, Update::Set("Room 2".into()));
    }

    #[test]
    fn a_line_without_the_marker_is_not_an_error() {
        assert_eq!(parse_line("[FLog::Output] just a game printing").unwrap(), None);
    }

    /// The three string states the protocol distinguishes, and the fourth that
    /// comes from the field being absent.
    #[test]
    fn absent_empty_and_reset_are_three_different_things() {
        let mut p = Presence::default();

        let cmd = one(r#"{"command":"SetRichPresence","data":{"details":"In the lobby"}}"#);
        p.apply(cmd);
        assert_eq!(p.details, Slot::Value("In the lobby".into()));

        // Absent: the previous value stands.
        p.apply(one(r#"{"command":"SetRichPresence","data":{"state":"Team A"}}"#));
        assert_eq!(p.details, Slot::Value("In the lobby".into()));
        assert_eq!(p.state, Slot::Value("Team A".into()));

        // Empty string: show nothing.
        p.apply(one(r#"{"command":"SetRichPresence","data":{"details":""}}"#));
        assert_eq!(p.details, Slot::Empty);

        // The sentinel: back to Cordial's own value.
        p.apply(one(r#"{"command":"SetRichPresence","data":{"details":"<reset>"}}"#));
        assert_eq!(p.details, Slot::Default);
    }

    #[test]
    fn images_clear_and_reset_by_flag_rather_than_by_sentinel() {
        let mut p = Presence::default();
        p.apply(one(
            r#"{"command":"SetRichPresence","data":{"largeImage":{"assetId":10630555127,"hoverText":"hi"}}}"#,
        ));
        assert_eq!(
            p.large_image,
            Slot::Value(Image { asset_id: "10630555127".into(), hover_text: Some("hi".into()) })
        );

        p.apply(one(r#"{"command":"SetRichPresence","data":{"largeImage":{"clear":true}}}"#));
        assert_eq!(p.large_image, Slot::Empty);

        // Cleared stays cleared through a command that does not mention it --
        // Bloxstrap's documentation is explicit that showing it again takes an
        // explicit reset.
        p.apply(one(r#"{"command":"SetRichPresence","data":{"state":"x"}}"#));
        assert_eq!(p.large_image, Slot::Empty);

        p.apply(one(r#"{"command":"SetRichPresence","data":{"largeImage":{"reset":true}}}"#));
        assert_eq!(p.large_image, Slot::Default);
    }

    /// An asset id is a path component downstream, so it is digits or nothing.
    #[test]
    fn a_non_numeric_asset_id_is_refused() {
        let err = parse_line(
            r#"[BloxstrapRPC] {"command":"SetRichPresence","data":{"largeImage":{"assetId":"../../etc"}}}"#,
        )
        .unwrap_err();
        assert!(err.contains("assetId"), "{err}");
    }

    #[test]
    fn the_string_form_of_an_asset_id_is_accepted() {
        let Some(Command::SetRichPresence(rp)) = parse_line(
            r#"[BloxstrapRPC] {"command":"SetRichPresence","data":{"smallImage":{"assetId":"13409122839"}}}"#,
        )
        .unwrap() else {
            panic!("expected a rich presence command");
        };
        assert_eq!(
            rp.small_image,
            Update::Set(Image { asset_id: "13409122839".into(), hover_text: None })
        );
    }

    /// The bug this guards against is real and has happened here before: a
    /// byte-slice behind a length check panics on a multi-byte boundary.
    #[test]
    fn an_over_long_field_is_clamped_on_a_character_boundary() {
        let long = "é".repeat(200);
        let line = format!(
            r#"[BloxstrapRPC] {{"command":"SetRichPresence","data":{{"details":"{long}"}}}}"#
        );
        let Some(Command::SetRichPresence(rp)) = parse_line(&line).unwrap() else {
            panic!("expected a rich presence command");
        };
        let Update::Set(details) = &rp.details else { panic!("expected a set") };
        assert_eq!(details.chars().count(), TEXT_FIELD_LIMIT);
        assert_eq!(rp.clamped, vec!["details"]);
    }

    #[test]
    fn launch_data_comes_through_untouched() {
        let Some(Command::SetLaunchData(d)) = parse_line(
            r#"[BloxstrapRPC] {"command":"SetLaunchData","data":"{\"roomId\": 2}"}"#,
        )
        .unwrap() else {
            panic!("expected launch data");
        };
        assert_eq!(d, r#"{"roomId": 2}"#);
    }

    /// A typo has to fail as unknown rather than fall through to something
    /// that happens to pass -- the same rule `protocol::required_capability`
    /// states for method names.
    #[test]
    fn an_unknown_command_is_refused() {
        let err = parse_line(r#"[BloxstrapRPC] {"command":"SetRichPresense","data":{}}"#)
            .unwrap_err();
        assert!(err.contains("SetRichPresense"), "{err}");
    }

    #[test]
    fn malformed_json_is_refused_rather_than_ignored() {
        assert!(parse_line("[BloxstrapRPC] not json at all").is_err());
        assert!(parse_line("[BloxstrapRPC]").is_err());
    }

    #[test]
    fn a_command_that_asks_for_nothing_is_recognisable_as_such() {
        let Some(Command::SetRichPresence(rp)) =
            parse_line(r#"[BloxstrapRPC] {"command":"SetRichPresence","data":{}}"#).unwrap()
        else {
            panic!("expected a rich presence command");
        };
        assert!(rp.is_empty());
    }

    fn one(body: &str) -> Command {
        parse_line(&format!("[BloxstrapRPC] {body}")).unwrap().unwrap()
    }
}
