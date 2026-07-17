//! The `logs` command's plan layer, rebuilt from first principles.
//!
//! - [`plan`] — resolves the mode flags + selectors into one exhaustive
//!   [`LogsPlan`], or a typed [`LogsPlanError`].
//! - [`diagnostics`] — typed diagnostics for the mode-resolution failures.
//!
//! The log I/O itself (tailing, streaming, rotation, formatting) lives in
//! [`crate::logs`]; this module only decides which mode an invocation is.

pub mod diagnostics;
pub mod plan;

pub use diagnostics::{
    conflicting_modes, follow_with_mode, loose_service_not_found, prune_bound_missing,
    supervisor_with_selector, target_required, unsupported_format,
};
pub use plan::{LogsPlan, LogsPlanError, Modes, resolve_plan};
