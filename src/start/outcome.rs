//! The per-unit "came up" outcome — the single typed answer to "did this
//! service start?" that the boot path produces and the CLI renders.
//!
//! sysg is not the service; it can only assert observable process facts. The
//! ladder is deliberate and total:
//!
//! 1. no PID assigned                         -> [`Outcome::Failed`] (start).
//! 2. PID assigned, exited:
//!      - clean exit (0) on a finite unit      -> [`Outcome::Completed`].
//!      - otherwise                            -> [`Outcome::Failed`].
//! 3. PID alive past the settle window:
//!      - health check configured and passed   -> [`Outcome::Up`].
//!      - health check configured and failed   -> [`Outcome::Failed`] (SG0104).
//!      - no health check                      -> [`Outcome::Up`] (Running).
//!
//! `Up` means the process is alive. It is deliberately *not* a claim of
//! readiness or correctness — [`Liveness`] and health are distinct so the two
//! can never be conflated by accident, which was the original assumption bug.

use serde::{Deserialize, Serialize};

use crate::diag::{Diagnostic, SgCode};

/// The strongest claim sysg can make about a started process: it is alive. This
/// is intentionally a distinct type from any notion of health or readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Liveness {
    /// The live process id.
    pub pid: u32,
}

/// The terminal boot outcome for one unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Outcome {
    /// The unit is running: a live PID, past the settle window, and — if a
    /// health check is configured — it passed. `Up` asserts liveness only.
    Up(Liveness),
    /// A finite unit (one-shot/cron) ran and exited cleanly. Not a failure.
    Completed,
    /// The unit did not come up. Carries the diagnostic to show the user.
    Failed(Diagnostic),
}

impl Outcome {
    /// Whether this outcome represents a unit that successfully came up or
    /// finished. `false` only for [`Outcome::Failed`].
    pub fn succeeded(&self) -> bool {
        !matches!(self, Outcome::Failed(_))
    }

    /// The diagnostic, when the unit failed to come up.
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            Outcome::Failed(diag) => Some(diag),
            _ => None,
        }
    }
}

/// Builds the SG0102 diagnostic for a unit that exited immediately at start.
pub fn immediate_exit(service: &str, exit_code: Option<i32>) -> Diagnostic {
    let detail = match exit_code {
        Some(code) => format!("it exited with code {code} before it could come up"),
        None => "it exited before it could come up".to_string(),
    };
    Diagnostic::error(
        SgCode::UnitImmediateExit,
        format!("service `{service}` exited immediately at start"),
    )
    .note(detail)
    .help_cmd("view logs", format!("sysg logs -s {service}"))
    .help_docs()
}

/// Builds the SG0103 diagnostic for a failed `pre_start` command.
pub fn pre_start_failed(service: &str, exit_code: Option<i32>) -> Diagnostic {
    let detail = match exit_code {
        Some(code) => format!("the pre_start command exited with code {code}"),
        None => "the pre_start command failed".to_string(),
    };
    Diagnostic::error(
        SgCode::PreStartFailed,
        format!("pre_start for `{service}` failed, so it was not started"),
    )
    .note(detail)
    .help_cmd("view logs", format!("sysg logs -s {service}"))
    .help_docs()
}

/// Builds the SG0104 diagnostic for a unit that never became healthy.
pub fn health_unmet(service: &str, attempts: u32) -> Diagnostic {
    Diagnostic::error(
        SgCode::HealthUnmet,
        format!("service `{service}` never passed its health check"),
    )
    .note(format!(
        "the process is running but {attempts} health checks did not pass"
    ))
    .help_cmd("view logs", format!("sysg logs -s {service}"))
    .help_docs()
}

/// Builds the generic SG0008 diagnostic for a unit that failed to start for a
/// reason without a more specific code.
pub fn unit_start_failed(service: &str, reason: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        SgCode::UnitStartFailed,
        format!("service `{service}` failed to start"),
    )
    .note(reason)
    .help_cmd("view logs", format!("sysg logs -s {service}"))
    .help_docs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_and_completed_succeed_failed_does_not() {
        assert!(Outcome::Up(Liveness { pid: 42 }).succeeded());
        assert!(Outcome::Completed.succeeded());
        assert!(!Outcome::Failed(immediate_exit("web", Some(1))).succeeded());
    }

    #[test]
    fn failed_carries_its_diagnostic_code() {
        let out = Outcome::Failed(pre_start_failed("api", Some(3)));
        assert_eq!(out.diagnostic().unwrap().code, SgCode::PreStartFailed);
    }

    #[test]
    fn outcome_round_trips_over_serde() {
        let out = Outcome::Up(Liveness { pid: 7 });
        let json = serde_json::to_string(&out).unwrap();
        assert_eq!(serde_json::from_str::<Outcome>(&json).unwrap(), out);

        let failed = Outcome::Failed(health_unmet("db", 5));
        let json = serde_json::to_string(&failed).unwrap();
        assert_eq!(serde_json::from_str::<Outcome>(&json).unwrap(), failed);
    }
}
