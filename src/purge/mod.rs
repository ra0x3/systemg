//! The `purge` command, rebuilt from first principles.
//!
//! - [`plan`] — resolves selectors into an exhaustive [`PurgePlan`], with a
//!   [`preflight`](plan::preflight) that refuses to wipe state out from under a
//!   live supervisor (SG0401) before any deletion.

pub mod plan;

pub use plan::{
    Preflight, PurgePlan, World, incomplete, preflight, project_not_found, resolve_plan,
    supervisor_active,
};
