//! The frame codec: one JSON object per line (spec section 2).
//!
//! ```text
//! {"id":7,"m":"settings.set","p":{"throttle":"off"}}                   request
//! {"id":7,"ok":true,"p":{"applied":["throttle"]}}                       reply
//! {"id":7,"ok":false,"e":{"code":"unsupported","detail":"..."}}        error reply
//! {"ev":"game.joined","n":41,"p":{}}                                    event
//! ```
//!
//! A line is a request if it has `m`, an event if it has `ev`, and a reply if
//! it has `ok`. A line with two of those, or none, is refused rather than
//! guessed at. Unknown fields are ignored, so a later minor can add some.

use crate::error::{Code, Violation};
use crate::lines::MAX_LINE;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

/// Longest string a receiver accepts anywhere in a message, in bytes, unless a
/// capability says otherwise (spec section 2).
pub const MAX_STRING: usize = 512;

/// A call that wants exactly one reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Per direction, and chosen by whoever sends the request.
    pub id: u64,
    /// The verb, such as `"settings.set"`.
    pub m: String,
    /// The payload. `Value::Null` when the line had none.
    pub p: Value,
}

/// The answer to a [`Request`], matched on `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub id: u64,
    pub result: Result<Value, ErrorBody>,
}

/// The `e` of an error reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: Code,
    #[serde(default)]
    pub detail: String,
}

/// Something that happened, runtime to launcher. `bye` is the one event either
/// side may send.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub ev: String,
    /// The sender's own counter; see [`crate::queue::EventQueue`].
    pub n: u64,
    pub p: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Request(Request),
    Reply(Reply),
    Event(Event),
}

impl Reply {
    pub fn ok(id: u64, p: Value) -> Self {
        Reply { id, result: Ok(p) }
    }
    pub fn err(id: u64, code: Code, detail: impl Into<String>) -> Self {
        Reply { id, result: Err(ErrorBody { code, detail: detail.into() }) }
    }
}

impl Request {
    pub fn new(id: u64, m: impl Into<String>, p: Value) -> Self {
        Request { id, m: m.into(), p }
    }
}

impl Event {
    pub fn new(ev: impl Into<String>, n: u64, p: Value) -> Self {
        Event { ev: ev.into(), n, p }
    }
}

// Declared field order is the order on the wire, which keeps a line the same
// shape as the examples in the spec.
#[derive(Serialize)]
struct WireRequest<'a> {
    id: u64,
    m: &'a str,
    #[serde(skip_serializing_if = "Value::is_null")]
    p: &'a Value,
}

#[derive(Serialize)]
struct WireOk<'a> {
    id: u64,
    ok: bool,
    #[serde(skip_serializing_if = "Value::is_null")]
    p: &'a Value,
}

#[derive(Serialize)]
struct WireErr<'a> {
    id: u64,
    ok: bool,
    e: &'a ErrorBody,
}

#[derive(Serialize)]
struct WireEvent<'a> {
    ev: &'a str,
    n: u64,
    #[serde(skip_serializing_if = "Value::is_null")]
    p: &'a Value,
}

#[derive(Deserialize)]
struct RawRequest {
    id: u64,
    m: String,
    #[serde(default)]
    p: Value,
}

#[derive(Deserialize)]
struct RawReply {
    id: u64,
    ok: bool,
    #[serde(default)]
    p: Value,
    e: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct RawEvent {
    ev: String,
    n: u64,
    #[serde(default)]
    p: Value,
}

/// Why a line is not a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Longer than [`MAX_LINE`]; refused before it is parsed.
    TooLong { len: usize },
    NotJson(String),
    /// Valid JSON that is not an object.
    NotObject,
    /// Carries none of `m`, `ev`, `ok`, or more than one.
    Ambiguous,
    /// A field is missing or has the wrong type: a negative or fractional
    /// `id`, a string `n`, an `ok:false` with no `e`, a code outside the set.
    Field(String),
    /// Parsed, but breaks a bound: a string over [`MAX_STRING`] bytes.
    Limit(Violation),
}

impl DecodeError {
    /// A short stable word for the failure, used by the shared vectors.
    pub fn kind(&self) -> &'static str {
        match self {
            DecodeError::TooLong { .. } => "too_long",
            DecodeError::NotJson(_) => "not_json",
            DecodeError::NotObject => "not_object",
            DecodeError::Ambiguous => "ambiguous",
            DecodeError::Field(_) => "field",
            DecodeError::Limit(_) => "limit",
        }
    }
}

impl DecodeError {
    /// Whether a receiver closes the connection on this (spec section 9). A line
    /// that is not a frame at all does. A well-formed frame carrying an
    /// out-of-range value ([`Limit`](DecodeError::Limit)) is dropped and counted,
    /// and the connection stays.
    pub fn closes_connection(&self) -> bool {
        !matches!(self, DecodeError::Limit(_))
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TooLong { len } => write!(f, "line of {len} bytes is over the {MAX_LINE} byte limit"),
            DecodeError::NotJson(e) => write!(f, "not JSON: {e}"),
            DecodeError::NotObject => f.write_str("a frame is a JSON object"),
            DecodeError::Ambiguous => f.write_str("a frame has exactly one of m, ev, ok"),
            DecodeError::Field(e) => write!(f, "bad field: {e}"),
            DecodeError::Limit(v) => write!(f, "over a limit: {v}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Parse one line (with or without its trailing newline).
///
/// The cap is checked first, on the raw length, so an oversized line never
/// reaches the parser. The string bound is checked on the result.
pub fn decode_line(line: &str) -> Result<Frame, DecodeError> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.len() > MAX_LINE {
        return Err(DecodeError::TooLong { len: line.len() });
    }
    let value: Value = serde_json::from_str(line).map_err(|e| DecodeError::NotJson(e.to_string()))?;
    let Value::Object(top) = value else { return Err(DecodeError::NotObject) };
    let kinds = ["m", "ev", "ok"].iter().filter(|k| top.contains_key(**k)).count();
    if kinds != 1 {
        return Err(DecodeError::Ambiguous);
    }
    let value = Value::Object(top);
    let field = |e: serde_json::Error| DecodeError::Field(e.to_string());
    let frame = if value.get("m").is_some() {
        let r: RawRequest = serde_json::from_value(value).map_err(field)?;
        Frame::Request(Request { id: r.id, m: r.m, p: r.p })
    } else if value.get("ev").is_some() {
        let r: RawEvent = serde_json::from_value(value).map_err(field)?;
        Frame::Event(Event { ev: r.ev, n: r.n, p: r.p })
    } else {
        let r: RawReply = serde_json::from_value(value).map_err(field)?;
        match (r.ok, r.e) {
            (true, None) => Frame::Reply(Reply { id: r.id, result: Ok(r.p) }),
            (false, Some(e)) => Frame::Reply(Reply { id: r.id, result: Err(e) }),
            (true, Some(_)) => return Err(DecodeError::Field("an ok reply carries no e".into())),
            (false, None) => return Err(DecodeError::Field("an error reply carries e".into())),
        }
    };
    frame.check_limits().map_err(DecodeError::Limit)?;
    Ok(frame)
}

/// One line, newline-terminated.
pub fn encode_line(frame: &Frame) -> String {
    let mut line = match frame {
        Frame::Request(r) => serde_json::to_string(&WireRequest { id: r.id, m: &r.m, p: &r.p }),
        Frame::Event(e) => serde_json::to_string(&WireEvent { ev: &e.ev, n: e.n, p: &e.p }),
        Frame::Reply(Reply { id, result: Ok(p) }) => serde_json::to_string(&WireOk { id: *id, ok: true, p }),
        Frame::Reply(Reply { id, result: Err(e) }) => serde_json::to_string(&WireErr { id: *id, ok: false, e }),
    }
    .expect("a frame always serialises");
    line.push('\n');
    line
}

impl Frame {
    pub fn decode(line: &str) -> Result<Frame, DecodeError> {
        decode_line(line)
    }

    pub fn encode(&self) -> String {
        encode_line(self)
    }

    /// The string bound, applied to every string and object key in the message
    /// (spec section 2): 512 bytes, or the longer limit a capability sets for its
    /// own verb. The launcher runs it on everything a runtime sends.
    pub fn check_limits(&self) -> Result<(), Violation> {
        use crate::msg::string_limit;
        match self {
            Frame::Request(r) => {
                check_string("m", &r.m, MAX_STRING)?;
                check_value("p", &r.p, string_limit(&r.m))
            }
            Frame::Event(e) => {
                check_string("ev", &e.ev, MAX_STRING)?;
                check_value("p", &e.p, string_limit(&e.ev))
            }
            Frame::Reply(Reply { result: Ok(p), .. }) => check_value("p", p, MAX_STRING),
            Frame::Reply(Reply { result: Err(e), .. }) => check_string("e.detail", &e.detail, MAX_STRING),
        }
    }
}

fn check_string(at: &str, s: &str, max: usize) -> Result<(), Violation> {
    if s.len() > max {
        Err(Violation::new(at, format!("{} bytes is over the {max} byte limit", s.len())))
    } else {
        Ok(())
    }
}

/// Every string and key under `v` is at most `max` bytes.
pub fn check_value(at: &str, v: &Value, max: usize) -> Result<(), Violation> {
    match v {
        Value::String(s) => check_string(at, s, max),
        Value::Array(items) => {
            items.iter().enumerate().try_for_each(|(i, item)| check_value(&format!("{at}[{i}]"), item, max))
        }
        Value::Object(map) => map.iter().try_for_each(|(k, item)| {
            check_string(&format!("{at}.<key>"), k, max)?;
            check_value(&format!("{at}.{k}"), item, max)
        }),
        _ => Ok(()),
    }
}

/// A payload that is absent or `null` reads as `{}`, so a verb with no fields
/// can be sent either way.
pub(crate) fn object_or_empty(p: &Value) -> Value {
    match p {
        Value::Null => Value::Object(Map::new()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_spec_examples_decode_and_re_encode_byte_for_byte() {
        for line in [
            r#"{"id":7,"m":"settings.set","p":{"throttle":"off"}}"#,
            r#"{"id":7,"ok":true,"p":{"applied":["throttle"]}}"#,
            r#"{"id":7,"ok":false,"e":{"code":"unsupported","detail":"..."}}"#,
            r#"{"ev":"game.joined","n":41,"p":{}}"#,
        ] {
            let frame = decode_line(line).unwrap();
            assert_eq!(encode_line(&frame), format!("{line}\n"), "{line}");
        }
    }

    #[test]
    fn a_payload_may_be_absent() {
        let f = decode_line(r#"{"id":1,"m":"state.get"}"#).unwrap();
        assert_eq!(f, Frame::Request(Request::new(1, "state.get", Value::Null)));
        assert_eq!(encode_line(&f), "{\"id\":1,\"m\":\"state.get\"}\n");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let f = decode_line(r#"{"id":1,"m":"x","p":{},"later":[1,2],"trace":"abc"}"#).unwrap();
        assert!(matches!(f, Frame::Request(_)));
        assert!(decode_line(r#"{"ev":"x","n":1,"future":true}"#).is_ok());
        assert!(decode_line(r#"{"id":1,"ok":true,"future":true}"#).is_ok());
    }

    #[test]
    fn the_wrong_shape_is_refused_not_repaired() {
        for (line, kind) in [
            ("", "not_json"),
            ("not json", "not_json"),
            ("[]", "not_object"),
            ("7", "not_object"),
            ("{}", "ambiguous"),
            (r#"{"id":1}"#, "ambiguous"),
            (r#"{"id":1,"m":"a","ev":"b","n":1}"#, "ambiguous"),
            (r#"{"id":-1,"m":"a"}"#, "field"),
            (r#"{"id":1.5,"m":"a"}"#, "field"),
            (r#"{"id":"1","m":"a"}"#, "field"),
            (r#"{"m":"a"}"#, "field"),
            (r#"{"ev":"a"}"#, "field"),
            (r#"{"ev":"a","n":"1"}"#, "field"),
            (r#"{"id":1,"ok":false}"#, "field"),
            (r#"{"id":1,"ok":true,"e":{"code":"failed"}}"#, "field"),
            (r#"{"id":1,"ok":false,"e":{"code":"teapot"}}"#, "field"),
            (r#"{"id":99999999999999999999,"m":"a"}"#, "field"),
        ] {
            assert_eq!(decode_line(line).unwrap_err().kind(), kind, "{line}");
        }
    }

    #[test]
    fn a_line_over_the_cap_is_refused_before_parsing() {
        let line = format!(r#"{{"id":1,"m":"a","p":{{"pad":"{}"}}}}"#, "x".repeat(MAX_LINE));
        assert_eq!(decode_line(&line).unwrap_err().kind(), "too_long");
    }

    #[test]
    fn a_string_over_512_bytes_is_a_limit_violation_anywhere_in_the_message() {
        let long = "x".repeat(MAX_STRING + 1);
        for line in [
            format!(r#"{{"id":1,"m":"a","p":{{"k":"{long}"}}}}"#),
            format!(r#"{{"id":1,"m":"a","p":["fine","{long}"]}}"#),
            format!(r#"{{"id":1,"m":"a","p":{{"{long}":1}}}}"#),
            format!(r#"{{"ev":"{long}","n":1}}"#),
            format!(r#"{{"id":1,"ok":false,"e":{{"code":"failed","detail":"{long}"}}}}"#),
        ] {
            assert_eq!(decode_line(&line).unwrap_err().kind(), "limit", "{}", &line[..40]);
        }
        let exact = "x".repeat(MAX_STRING);
        assert!(decode_line(&format!(r#"{{"id":1,"m":"a","p":{{"k":"{exact}"}}}}"#)).is_ok());
    }

    #[test]
    fn only_an_out_of_range_value_keeps_the_connection() {
        let long = "x".repeat(MAX_STRING + 1);
        let over = decode_line(&format!(r#"{{"ev":"e","n":1,"p":{{"k":"{long}"}}}}"#)).unwrap_err();
        assert!(!over.closes_connection());
        for line in ["nope", "[]", "{}", r#"{"id":-1,"m":"x"}"#] {
            assert!(decode_line(line).unwrap_err().closes_connection(), "{line}");
        }
    }

    #[test]
    fn every_error_code_round_trips() {
        for code in Code::ALL {
            let f = Frame::Reply(Reply::err(3, code, "why"));
            assert_eq!(decode_line(&encode_line(&f)).unwrap(), f);
        }
    }

    #[test]
    fn crlf_is_tolerated_on_input_and_never_produced() {
        let f = decode_line("{\"ev\":\"x\",\"n\":1}\r\n").unwrap();
        assert!(!encode_line(&f).contains('\r'));
    }

    #[test]
    fn an_object_payload_with_a_null_value_survives() {
        let f = Frame::Event(Event::new("x", 2, json!({"a": null})));
        assert_eq!(decode_line(&encode_line(&f)).unwrap(), f);
    }
}
