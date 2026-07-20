//! The `logs` command's plan layer, rebuilt from first principles.
//!
//! - [`crate::logs_cmd::plan`] — resolves the mode flags + selectors into one
//!   exhaustive [`crate::logs_cmd::LogsPlan`], or a typed
//!   [`crate::logs_cmd::LogsPlanError`].
//! - [`crate::logs_cmd::diagnostics`] — typed diagnostics for mode resolution.
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
