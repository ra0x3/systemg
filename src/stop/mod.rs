//! The `stop` command, rebuilt from first principles.
//!
//! - [`plan`] — resolves stop's selectors into one exhaustive [`StopPlan`].
//! - [`diagnostics`] — typed diagnostics for stop failures.

pub mod diagnostics;
pub mod plan;

pub use diagnostics::service_not_found;
pub use plan::{StopPlan, StopPlanError, resolve_plan};
