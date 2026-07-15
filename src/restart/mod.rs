//! The `restart` command, rebuilt from first principles.
//!
//! - [`plan`] — resolves selectors into an exhaustive [`RestartPlan`], with a
//!   [`preflight`](plan::preflight) that refuses illegal operations before any
//!   side effect.

pub mod plan;
pub mod reconcile;

pub use plan::{
    Preflight, RestartPlan, World, manifest_rejected, preflight, reconcile_incomplete,
    resolve_plan,
};
pub use reconcile::ManifestDiff;
