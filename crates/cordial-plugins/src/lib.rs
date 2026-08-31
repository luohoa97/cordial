//! Cordial's plugin host.
//!
//! Plugins are separate processes speaking newline-delimited JSON over stdio,
//! with every call gated by a named capability. See
//! [ADR-003](../../../docs/adr/ADR-003-plugin-isolation.md) for why isolation is
//! by process rather than by a restricted in-process API, and
//! [ADR-005](../../../docs/adr/ADR-005-flag-service.md) for why flag writes are
//! split across two capabilities.
//!
//! What a plugin is allowed to do, and anything it remembers, belong to the
//! profile rather than to the machine — see
//! [ADR-013](../../../docs/adr/ADR-013-per-profile-configuration.md).

pub mod broker;
pub mod capability;
pub mod consent;
pub mod core_events;
pub mod denials;
pub mod enablement;
pub mod events;
pub mod flag_document;
pub mod grants;
pub mod health;
pub mod host;
pub mod manifest;
pub mod marketplace;
pub mod notify;
pub mod plugin_data;
pub mod preferences;
pub mod presence;
pub mod protocol;
pub mod sandbox;
pub mod registry;
pub mod resolve;
pub mod settings;
pub mod sign;
pub mod source;
pub mod unpack;
pub mod urlopen;
