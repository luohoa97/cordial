//! Version negotiation (spec section 3).
//!
//! The major is written once, in the manifest's `spec`. The minor lives only in
//! the handshake, so the two cannot disagree. A **minor** adds optional
//! capabilities, events and fields; a **major** breaks. A capability's version
//! is an integer that is additive within a major, so taking the lower of two is
//! always safe.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// The string a manifest's `spec` field carries for this major.
pub const SPEC: &str = "cordial.runtime/1";

/// What a side offers, capability name to integer version.
pub type Capabilities = BTreeMap<String, u32>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Protocol {
    pub major: u32,
    pub minor: u32,
}

impl Protocol {
    /// What this crate speaks.
    pub const CURRENT: Protocol = Protocol { major: 1, minor: 0 };

    pub const fn new(major: u32, minor: u32) -> Self {
        Protocol { major, minor }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The agreed protocol and the live capability set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    /// The shared major and the lower of the two minors.
    pub protocol: Protocol,
    /// The intersection of the two capability sets, each at the lower version.
    pub caps: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NegotiateError {
    /// The majors differ. The launcher refuses the runtime and names it.
    Major { ours: u32, theirs: u32 },
}

impl fmt::Display for NegotiateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NegotiateError::Major { ours, theirs } => {
                write!(f, "speaks protocol major {theirs}, this side speaks {ours}")
            }
        }
    }
}

impl std::error::Error for NegotiateError {}

/// The capabilities both sides have, each at the lower of its two versions.
///
/// A capability one side lacks is simply absent from the result, and a caller
/// never sends a request for it: unsupported is never faked.
pub fn intersect(ours: &Capabilities, theirs: &Capabilities) -> Capabilities {
    ours.iter()
        .filter_map(|(name, a)| theirs.get(name).map(|b| (name.clone(), (*a).min(*b))))
        .collect()
}

/// Agree on a protocol and a capability set.
pub fn negotiate(
    ours: Protocol,
    our_caps: &Capabilities,
    theirs: Protocol,
    their_caps: &Capabilities,
) -> Result<Negotiated, NegotiateError> {
    if ours.major != theirs.major {
        return Err(NegotiateError::Major { ours: ours.major, theirs: theirs.major });
    }
    Ok(Negotiated {
        protocol: Protocol { major: ours.major, minor: ours.minor.min(theirs.minor) },
        caps: intersect(our_caps, their_caps),
    })
}

/// The major named by a manifest's `spec` (`"cordial.runtime/1"` is `Some(1)`),
/// or `None` for anything that is not that shape.
pub fn spec_major(spec: &str) -> Option<u32> {
    let n = spec.strip_prefix("cordial.runtime/")?;
    // Digits only, no leading zero: `+1` and `01` are not spellings of 1.
    if n.is_empty() || (n.starts_with('0') && n.len() > 1) || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

impl Negotiated {
    /// Whether `capability` is live, and at what version.
    pub fn version_of(&self, capability: &str) -> Option<u32> {
        self.caps.get(capability).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(list: &[(&str, u32)]) -> Capabilities {
        list.iter().map(|(n, v)| (n.to_string(), *v)).collect()
    }

    #[test]
    fn the_live_set_is_the_intersection_at_the_lower_version() {
        let ours = caps(&[("lifecycle", 1), ("settings", 3), ("flags", 1)]);
        let theirs = caps(&[("lifecycle", 1), ("settings", 2), ("diagnostics", 1)]);
        let n = negotiate(Protocol::new(1, 4), &ours, Protocol::new(1, 2), &theirs).unwrap();
        assert_eq!(n.caps, caps(&[("lifecycle", 1), ("settings", 2)]));
        assert_eq!(n.protocol, Protocol::new(1, 2), "the lower minor");
        // And it does not depend on which side asks.
        let back = negotiate(Protocol::new(1, 2), &theirs, Protocol::new(1, 4), &ours).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn a_different_major_is_refused_and_named() {
        let e = negotiate(Protocol::new(1, 0), &caps(&[]), Protocol::new(2, 0), &caps(&[])).unwrap_err();
        assert_eq!(e, NegotiateError::Major { ours: 1, theirs: 2 });
        assert!(e.to_string().contains("major 2"));
    }

    #[test]
    fn spec_names_a_major_in_one_spelling() {
        assert_eq!(spec_major(SPEC), Some(1));
        assert_eq!(spec_major("cordial.runtime/12"), Some(12));
        for bad in [
            "",
            "cordial.runtime/",
            "cordial.runtime/1.2",
            "cordial.runtime/01",
            "cordial.runtime/+1",
            "other/1",
            "cordial.runtime/-1",
        ] {
            assert_eq!(spec_major(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn current_protocol_matches_the_spec_string() {
        assert_eq!(spec_major(SPEC), Some(Protocol::CURRENT.major));
    }
}
