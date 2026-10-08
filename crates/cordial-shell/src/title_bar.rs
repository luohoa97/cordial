//! Shared title-bar preference for the launcher settings and game window.
//!
//! The type moved to `cordial-protocol` with the live-settings wire, because the
//! wire carries it and a second copy would drift. This is the path every caller
//! already uses, so none of them changed.

pub use cordial_protocol::TitleBar;
