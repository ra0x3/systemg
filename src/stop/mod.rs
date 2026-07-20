//! The `stop` command, rebuilt from first principles.
//!
//! - [`crate::stop::plan`] — resolves stop's selectors into one exhaustive
//!   [`crate::stop::StopPlan`].
//! - [`crate::stop::diagnostics`] — typed diagnostics for stop failures.

pub mod diagnostics;
pub mod plan;

pub use diagnostics::{project_not_found, service_not_found};
pub use plan::{StopPlan, StopPlanError, resolve_plan};
