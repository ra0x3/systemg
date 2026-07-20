//! The `purge` command, rebuilt from first principles.
//!
//! - [`crate::purge::plan`] — resolves selectors into an exhaustive
//!   [`crate::purge::PurgePlan`], with a [`crate::purge::preflight`] that refuses
//!   to wipe state out from under a live supervisor (SG0401) before any deletion.

pub mod plan;

pub use plan::{
    Preflight, PurgePlan, World, incomplete, preflight, project_not_found, resolve_plan,
    supervisor_active,
};
