#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod error;
pub mod frame;
pub mod lines;
pub mod manifest;
pub mod msg;
pub mod queue;
pub mod settings;
pub mod socket;
pub mod v0;
pub mod version;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use error::{Code, Violation};
pub use frame::{decode_line, encode_line, DecodeError, ErrorBody, Event, Frame, Reply, Request, MAX_STRING};
pub use lines::{LineReader, Next, MAX_LINE};
pub use manifest::{Manifest, Placeholders};
pub use queue::EventQueue;
pub use settings::{Accel, FrameRateLimit, Throttle, TitleBar, Update};
pub use version::{negotiate, Capabilities, Negotiated, Protocol};
