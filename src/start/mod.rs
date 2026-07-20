//! The `start` command, rebuilt from first principles.
//!
//! Start is the bedrock: if a service cannot be brought up and reported
//! truthfully, no other command can be trusted. The module is split into small,
//! total pieces:
//!
//! - [`crate::start::outcome`] — the typed per-unit "came up" ladder every boot step produces.
//! - [`crate::start::boot`] — the race-free boot journal that records and replays progress.

/// Race-free boot progress recording and replay.
pub mod boot;
/// Typed outcomes and diagnostics for unit startup.
pub mod outcome;
/// Resolution of CLI start requests into explicit execution plans.
pub mod plan;
/// Terminal rendering and startup verdict collection.
pub mod render;

pub use boot::{BootFrame, BootJournal};
pub use outcome::{
    Liveness, Outcome, ambiguous_service, dependency_unavailable, outcome_of,
    project_mismatch, project_services_not_up, unit_start_failed,
};
pub use plan::{ProjectMismatch, StartPlan, resolve_plan};
pub use render::{BootReport, render_boot};
