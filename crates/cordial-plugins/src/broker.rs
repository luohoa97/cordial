//! The capability broker: the only thing that decides whether a call proceeds.
//!
//! Grants are per plugin and set from outside — **a plugin cannot request a
//! capability at runtime and cannot widen its own grant**, whatever it sends
//! down its own pipe. Anything that let it do so would make the grant
//! advisory, and an advisory capability system is decorative. This says
//! nothing about whether the *host* may call [`Broker::grant`] again after
//! construction to replace what is there; `cordial-runtime`'s serving loop
//! does exactly that, re-reading the grants file so a capability turned on in
//! Settings reaches an already-running plugin rather than only the next one
//! `start_all` spawns. The property this module guarantees is narrower and
//! more important than "never changes": whatever the grant is at the moment
//! of a call, it was put there by something other than the plugin asking.
//!
//! Denials are recorded rather than only refused. A plugin quietly failing
//! because it lacks a capability is otherwise indistinguishable from a plugin
//! that is broken, and that distinction is the difference between a two-minute
//! fix and an afternoon.

use crate::capability::Capability;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    pub plugin: String,
    pub capability: Capability,
}

#[derive(Debug, Default)]
pub struct Broker {
    grants: BTreeMap<String, BTreeSet<Capability>>,
    denials: Vec<Denial>,
}

impl Broker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant a set of capabilities to a plugin, replacing any previous grant.
    pub fn grant(&mut self, plugin: &str, caps: impl IntoIterator<Item = Capability>) {
        self.grants.insert(plugin.to_string(), caps.into_iter().collect());
    }

    /// Whether a call may proceed. Records the denial when it may not.
    pub fn allows(&mut self, plugin: &str, cap: Capability) -> bool {
        let ok = self.grants.get(plugin).is_some_and(|g| g.contains(&cap));
        if !ok {
            self.denials.push(Denial { plugin: plugin.to_string(), capability: cap });
        }
        ok
    }

    /// Non-recording query, for presenting a grant rather than acting on one.
    pub fn granted(&self, plugin: &str) -> BTreeSet<Capability> {
        self.grants.get(plugin).cloned().unwrap_or_default()
    }

    pub fn denials(&self) -> &[Denial] {
        &self.denials
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_granted_capability_is_allowed() {
        let mut b = Broker::new();
        b.grant("themer", [Capability::Log]);
        assert!(b.allows("themer", Capability::Log));
    }

    #[test]
    fn an_ungranted_capability_is_refused_and_recorded() {
        let mut b = Broker::new();
        b.grant("themer", [Capability::Log]);
        assert!(!b.allows("themer", Capability::FlagsWrite));
        assert_eq!(
            b.denials(),
            &[Denial { plugin: "themer".into(), capability: Capability::FlagsWrite }]
        );
    }

    #[test]
    fn an_unknown_plugin_has_nothing() {
        let mut b = Broker::new();
        assert!(!b.allows("nobody", Capability::Log));
        assert!(b.granted("nobody").is_empty());
    }

    #[test]
    fn granting_replaces_rather_than_accumulates() {
        // A plugin must not be able to widen its grant by being granted twice —
        // the second grant is the whole grant.
        let mut b = Broker::new();
        b.grant("p", [Capability::Log, Capability::FlagsWrite]);
        b.grant("p", [Capability::Log]);
        assert!(b.allows("p", Capability::Log));
        assert!(!b.allows("p", Capability::FlagsWrite));
    }

    #[test]
    fn writing_a_static_flag_is_a_different_capability_from_writing_a_live_one() {
        // ADR-005: the static families cannot be changed while running, so a
        // grant to change flags at launch must not imply a grant to change one
        // live, or the runtime API would accept calls it cannot honour.
        let mut b = Broker::new();
        b.grant("tuner", [Capability::FlagsWrite]);
        assert!(b.allows("tuner", Capability::FlagsWrite));
        assert!(!b.allows("tuner", Capability::FlagsWriteDynamic));
    }
}
