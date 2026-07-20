//! The `restart` command, rebuilt from first principles.
//!
//! - [`crate::restart::plan`] — resolves selectors into an exhaustive
//!   [`crate::restart::RestartPlan`], with a [`crate::restart::preflight`] that
//!   refuses illegal operations before any side effect.

pub mod plan;
pub mod reconcile;

pub use plan::{
    Preflight, RestartPlan, World, manifest_rejected, preflight, reconcile_incomplete,
    recycle_failed, recycle_refused, resolve_plan,
};
pub use reconcile::ManifestDiff;
