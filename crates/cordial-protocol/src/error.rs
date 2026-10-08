//! Error codes, and the one way a received message is refused.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The closed set of error codes a reply can carry (spec section 5).
///
/// **Closed on purpose.** An enumeration a peer sends is checked against the
/// set, and a value outside it is a protocol violation rather than a string to
/// pass along; a peer that wants a new code needs a spec bump. The wire form is
/// the lowercase snake-case word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// The request names a verb or a capability this side does not offer. An
    /// unknown request gets this, never a silent success.
    Unsupported,
    /// The verb is known and the payload is not acceptable.
    Invalid,
    /// The verb is known, the payload is fine, and the effect did not happen.
    Failed,
    /// Try again later; the runtime is occupied.
    Busy,
    /// A request arrived before the handshake finished.
    NotReady,
}

impl Code {
    pub const ALL: [Code; 5] = [Code::Unsupported, Code::Invalid, Code::Failed, Code::Busy, Code::NotReady];

    pub fn as_str(self) -> &'static str {
        match self {
            Code::Unsupported => "unsupported",
            Code::Invalid => "invalid",
            Code::Failed => "failed",
            Code::Busy => "busy",
            Code::NotReady => "not_ready",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A message that breaks a bound or a shape the spec sets. The receiver drops
/// the message and counts it; it does not guess what was meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The field or verb at fault, as a short path (`"p.lines[3]"`).
    pub at: String,
    pub why: String,
}

impl Violation {
    pub fn new(at: impl Into<String>, why: impl Into<String>) -> Self {
        Violation { at: at.into(), why: why.into() }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.why)
    }
}

impl std::error::Error for Violation {}
