//! Typed diagnostics for the `status` command.
//!
//! `status` is read-only, so these are not failures so much as honest labels on
//! degraded readings: the supervisor is gone (SG0206), or alive but not
//! answering (SG0205), or the persisted state disagrees with what the process
//! table actually shows (SG0009). None of them are silent — a stale HEALTHY is
//! the one outcome the rebuild refuses to produce.

use crate::diag::{Diagnostic, SgCode};

/// No supervisor is running AND there is no manifest to read state from, so
/// `status` has nothing to report at all.
///
/// The sibling of [`supervisor_offline`], which still shows a disk reading.
/// Here there is no reading to label: without a config there is no project to
/// resolve, so this is a hard error rather than a warning over some data.
pub fn supervisor_not_started() -> Diagnostic {
    Diagnostic::error(
        SgCode::SupervisorOffline,
        "no supervisor is running, and no manifest was given to read state from",
    )
    .note(
        "`status` falls back to persisted state when the supervisor is down, \
         but that needs a project to read: none was named and none is loaded",
    )
    .help_cmd("start a supervisor", "sysg start --daemonize")
    .help_cmd("read a project off disk", "sysg status -c <config>.yaml")
    .help_docs()
}

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

/// The boot finished but its snapshot could not be published, so the served
/// cache still describes the world as it was before the boot.
///
/// Startup itself ran — its units may have come up or failed — and only the
/// reading is behind. Reported so a caller is never handed a silent success
/// over a pre-boot snapshot.
pub fn snapshot_unavailable(detail: impl Into<String>) -> Diagnostic {
    Diagnostic::warn(
        SgCode::StatusStateInconsistent,
        "the boot completed but its status snapshot could not be published",
    )
    .note(detail)
    .note(
        "service startup ran; until the next refresh `sysg status` may still \
         report those units as not yet running",
    )
    .help_cmd("read live state instead", "sysg status --live")
    .help_docs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_unavailable_is_sg0009_and_does_not_claim_units_came_up() {
        let diag = snapshot_unavailable("lock poisoned");
        assert_eq!(diag.code, SgCode::StatusStateInconsistent);
        let rendered = diag.render(false);
        assert!(rendered.contains("service startup ran"));
        assert!(rendered.contains("lock poisoned"));
    }

    #[test]
    fn offline_is_sg0206_and_names_orphans() {
        let diag = supervisor_offline();
        assert_eq!(diag.code, SgCode::SupervisorOffline);
        assert!(diag.render(false).contains("orphaned"));
    }

    #[test]
    fn not_started_is_sg0206_and_says_how_to_start_one() {
        let diag = supervisor_not_started();
        assert_eq!(diag.code, SgCode::SupervisorOffline);
        let rendered = diag.render(false);
        assert!(rendered.contains("no supervisor is running"));
        assert!(rendered.contains("sysg start --daemonize"));
        // Distinct from `supervisor_offline`: there is no reading to label, so
        // it must not claim anything survived the supervisor.
        assert!(!rendered.contains("orphaned"));
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
