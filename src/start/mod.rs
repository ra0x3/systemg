//! The `start` command, rebuilt from first principles.
//!
//! Start is the bedrock: if a service cannot be brought up and reported
//! truthfully, no other command can be trusted. The module is split into small,
//! total pieces:
//!
//! - [`crate::start::outcome`] — the typed per-unit "came up" ladder every boot step produces.
//! - [`crate::start::boot`] — the race-free boot journal that records and replays progress.
//! - [`crate::start::sched`] — dependency-ready dispatch, so units wait only on what they declared.

/// Race-free boot progress recording and replay.
pub mod boot;
/// Typed outcomes and diagnostics for unit startup.
pub mod outcome;
/// Resolution of CLI start requests into explicit execution plans.
pub mod plan;
/// Terminal rendering and startup verdict collection.
pub mod render;
/// Dependency-ready dispatch of a project's units.
pub mod sched;
/// Reduction of boot frames into the nested progress tree.
pub mod tree;

pub use boot::{BootFrame, BootJournal, StepState};
pub use outcome::{
    Liveness, Outcome, ambiguous_service, dependency_unavailable, outcome_of,
    project_mismatch, project_services_not_up, restart_breaker_open, unit_start_failed,
};
pub use plan::{ProjectMismatch, StartPlan, resolve_plan};
pub use render::{BootReport, render_boot};
pub use sched::{Gate, Resolution, Schedule, Units};
pub use tree::{RowState, TreeRow, TreeState, fit_rows};
