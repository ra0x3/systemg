//! Tracks the supervisor's current in-flight operation so reads can report what
//! a busy mutation is waiting on instead of leaving the caller in the dark.
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

/// Snapshot of what the supervisor is doing right now, sent to the CLI when a
/// command times out so the wait is never opaque.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpReport {
    /// Short label naming the operation, e.g. "starting gamecast-prod".
    pub label: String,
    /// Finer-grained detail, e.g. "waiting on dependency 'migrations'".
    pub detail: Option<String>,
    /// Seconds elapsed since the operation began.
    pub elapsed_secs: u64,
}

impl OpReport {
    /// Renders the report as a single human-readable line.
    pub fn describe(&self) -> String {
        match &self.detail {
            Some(detail) => {
                format!("{} — {} ({}s)", self.label, detail, self.elapsed_secs)
            }
            None => format!("{} ({}s)", self.label, self.elapsed_secs),
        }
    }
}

struct Op {
    label: String,
    detail: Option<String>,
    started_at: SystemTime,
}

/// Shared slot holding the supervisor's current operation, if any.
#[derive(Clone, Default)]
pub struct OpSlot {
    inner: Arc<Mutex<Option<Op>>>,
}

impl OpSlot {
    /// Creates an empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the start of an operation, clearing any previous detail.
    pub fn begin(&self, label: impl Into<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some(Op {
                label: label.into(),
                detail: None,
                started_at: SystemTime::now(),
            });
        }
    }

    /// Updates the detail line of the active operation without resetting its clock.
    pub fn detail(&self, detail: impl Into<String>) {
        if let Ok(mut guard) = self.inner.lock()
            && let Some(op) = guard.as_mut()
        {
            op.detail = Some(detail.into());
        }
    }

    /// Clears the slot once the operation finishes.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = None;
        }
    }

    /// Returns a report of the active operation, if one is running.
    pub fn report(&self) -> Option<OpReport> {
        let guard = self.inner.lock().ok()?;
        let op = guard.as_ref()?;
        let elapsed = op.started_at.elapsed().unwrap_or(Duration::ZERO).as_secs();
        Some(OpReport {
            label: op.label.clone(),
            detail: op.detail.clone(),
            elapsed_secs: elapsed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_slot_reports_nothing() {
        assert!(OpSlot::new().report().is_none());
    }

    #[test]
    fn begin_then_detail_is_reported() {
        let slot = OpSlot::new();
        slot.begin("starting proj");
        slot.detail("waiting on dep");
        let report = slot.report().expect("report present");
        assert_eq!(report.label, "starting proj");
        assert_eq!(report.detail.as_deref(), Some("waiting on dep"));
        assert!(report.describe().contains("waiting on dep"));
    }

    #[test]
    fn clear_empties_the_slot() {
        let slot = OpSlot::new();
        slot.begin("work");
        slot.clear();
        assert!(slot.report().is_none());
    }

    #[test]
    fn begin_resets_detail() {
        let slot = OpSlot::new();
        slot.begin("first");
        slot.detail("phase one");
        slot.begin("second");
        let report = slot.report().expect("report present");
        assert_eq!(report.label, "second");
        assert!(report.detail.is_none());
    }
}
