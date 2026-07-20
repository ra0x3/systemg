//! Tracks the supervisor's current in-flight operation so reads can report what
//! a busy mutation is waiting on instead of leaving the caller in the dark.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
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
    id: u64,
    label: String,
    detail: Option<String>,
    started_at: SystemTime,
    /// Which operation the active detail belongs to. Details are written from
    /// deep inside a project's daemon (dependency waits, health polls) into the
    /// one shared slot, so without an owner a slow project's detail could be
    /// appended to whatever unrelated command happened to be in the slot — the
    /// caller then read a wait belonging to a project it never named.
    owner: Option<String>,
}

/// Shared slot holding the supervisor's current operation, if any.
#[derive(Clone, Default)]
pub struct OpSlot {
    inner: Arc<Mutex<Option<Op>>>,
    next: Arc<AtomicU64>,
}

/// Clears an operation slot when its owning scope ends.
pub struct OpGuard {
    slot: OpSlot,
    id: u64,
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        self.slot.clear_if(self.id);
    }
}

impl OpSlot {
    /// Creates an empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the start of an operation, clearing any previous detail.
    pub fn begin(&self, label: impl Into<String>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut guard) = self.inner.lock()
            && guard.as_ref().is_none_or(|op| op.id < id)
        {
            *guard = Some(Op {
                id,
                label: label.into(),
                detail: None,
                started_at: SystemTime::now(),
                owner: None,
            });
        }
        id
    }

    /// Records an operation and clears it when the returned guard is dropped.
    pub fn guard(&self, label: impl Into<String>) -> OpGuard {
        let id = self.begin(label);
        OpGuard {
            slot: self.clone(),
            id,
        }
    }

    /// Updates the detail line of the active operation without resetting its clock.
    pub fn detail(&self, detail: impl Into<String>) {
        if let Ok(mut guard) = self.inner.lock()
            && let Some(op) = guard.as_mut()
        {
            op.detail = Some(detail.into());
            op.owner = None;
        }
    }

    /// Updates the detail line only when `owner` matches the operation that is
    /// actually in the slot, so a background project's progress is never
    /// attributed to an unrelated command.
    pub fn detail_for(&self, owner: &str, detail: impl Into<String>) {
        if let Ok(mut guard) = self.inner.lock()
            && let Some(op) = guard.as_mut()
            && op.label.contains(owner)
        {
            op.detail = Some(detail.into());
            op.owner = Some(owner.to_string());
        }
    }

    /// Clears the slot once the operation finishes.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = None;
        }
    }

    /// Clears the slot only when `id` still owns the current operation.
    pub fn clear_if(&self, id: u64) {
        if let Ok(mut guard) = self.inner.lock()
            && guard.as_ref().is_some_and(|op| op.id == id)
        {
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
    fn detail_for_ignores_a_foreign_owner() {
        let slot = OpSlot::new();
        slot.begin("starting project 'alpha'");
        slot.detail_for("beta", "waiting on beta's dependency");
        let report = slot.report().expect("report present");
        assert_eq!(report.detail, None, "beta's detail leaked into alpha's op");
    }

    #[test]
    fn detail_for_accepts_the_matching_owner() {
        let slot = OpSlot::new();
        slot.begin("starting project 'alpha'");
        slot.detail_for("alpha", "waiting on dependency 'db'");
        let report = slot.report().expect("report present");
        assert_eq!(report.detail.as_deref(), Some("waiting on dependency 'db'"));
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
