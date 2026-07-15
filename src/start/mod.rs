//! The `start` command, rebuilt from first principles.
//!
//! Start is the bedrock: if a service cannot be brought up and reported
//! truthfully, no other command can be trusted. The module is split into small,
//! total pieces:
//!
//! - [`outcome`] — the typed per-unit "came up" ladder every boot step produces.

pub mod outcome;

pub use outcome::{Liveness, Outcome};
