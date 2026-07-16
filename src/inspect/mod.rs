//! The `inspect` command's plan layer, rebuilt from first principles.
//!
//! - [`plan`] — resolves the selector into an [`InspectPlan`] for the one service
//!   to detail, rejecting selectors inspect cannot serve.
//! - [`diagnostics`] — typed diagnostics for inspect failures.
//!
//! The payload collection and rendering live in the binary; this module only
//! decides which service is being inspected.

pub mod diagnostics;
pub mod plan;

pub use diagnostics::{invalid_stream_duration, service_not_found};
pub use plan::{InspectPlan, InspectPlanError, resolve_plan};
