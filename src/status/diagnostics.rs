//! Typed diagnostics for the `status` command.
//!
//! `status` is read-only, so these are not failures so much as honest labels on
//! degraded readings: the supervisor is gone (SG0206), or alive but not
//! answering (SG0205), or the persisted state disagrees with what the process
//! table actually shows (SG0009). None of them are silent — a stale HEALTHY is
//! the one outcome the rebuild refuses to produce.

use crate::diag::{Diagnostic, SgCode};

/// The supervisor is not running, so `status` is reading persisted state off
/// disk. Any process still alive is unsupervised. This is a warning, not a hard
/// error: the reading is shown, clearly labelled offline.
pub fn supervisor_offline() -> Diagnostic {
    Diagnostic::warn(
        SgCode::SupervisorOffline,
        "no supervisor is running; the state below was read from disk and is unsupervised",
    )
    .note("processes shown as running survived the supervisor and are now orphaned")
    .help_cmd("resume supervision", "sysg start --daemonize")
    .help_docs()
}

/// The supervisor's process is alive but did not answer within the probe window,
/// so `status` could not fetch a fresh snapshot.
pub fn supervisor_not_responding() -> Diagnostic {
    Diagnostic::warn(
        SgCode::SupervisorNotResponding,
        "the supervisor is running but did not answer its control socket in time",
    )
    .note("it may be shutting down or wedged; the reading below may be stale")
    .help_cmd("force it down", "sysg stop --supervisor")
    .help_docs()
}

/// The persisted state and the live process table disagree — a unit recorded as
/// running whose process is gone, or vice versa.
pub fn state_inconsistent(detail: impl Into<String>) -> Diagnostic {
    Diagnostic::warn(
        SgCode::StatusStateInconsistent,
        "recorded state disagrees with the live process table",
    )
    .note(detail)
    .help_cmd("see what's running", "sysg status --live")
    .help_docs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_is_sg0206_and_names_orphans() {
        let diag = supervisor_offline();
        assert_eq!(diag.code, SgCode::SupervisorOffline);
        assert!(diag.render(false).contains("orphaned"));
    }

    #[test]
    fn not_responding_is_sg0205() {
        assert_eq!(
            supervisor_not_responding().code,
            SgCode::SupervisorNotResponding
        );
    }

    #[test]
    fn inconsistent_is_sg0009_and_carries_detail() {
        let diag = state_inconsistent("web recorded running but pid 12 is gone");
        assert_eq!(diag.code, SgCode::StatusStateInconsistent);
        assert!(diag.render(false).contains("pid 12 is gone"));
    }
}
