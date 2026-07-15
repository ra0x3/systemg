//! The `start` command, rebuilt from first principles.
//!
//! Start is the bedrock: if a service cannot be brought up and reported
//! truthfully, no other command can be trusted. The module is split into small,
//! total pieces:
//!
//! - [`outcome`] — the typed per-unit "came up" ladder every boot step produces.
//! - [`boot`] — the race-free boot journal that records and replays progress.

pub mod boot;
pub mod outcome;
pub mod render;

pub use boot::{BootFrame, BootJournal};
pub use outcome::{Liveness, Outcome, outcome_of};
pub use render::{BootReport, render_boot};
