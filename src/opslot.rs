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
    /// Verb naming the mutation, e.g. "restarting".
    ///
    /// Defaulted on decode so a running older supervisor, which sends only the
    /// prose fields, still deserializes against a newer CLI.
    #[serde(default)]
    pub verb: Option<String>,
    /// What the operation acts on as a whole — the project when one is named,
    /// otherwise the lone service.
    #[serde(default)]
    pub target: Option<String>,
    /// The specific service currently being worked, nested under `target`.
    #[serde(default)]
    pub unit: Option<String>,
    /// What that unit is waiting on, with the unit's own name omitted.
    #[serde(default)]
    pub wait: Option<String>,
}

/// Columns of indent added per nesting level in the multi-line render.
const INDENT: usize = 4;

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

    /// Renders the report as nested rows: the operation on the first line, the
    /// unit it is working under that, and the unit's wait under that again.
    ///
    /// Returns `None` when the supervisor did not send the structured fields —
    /// an older daemon — so the caller falls back to [`Self::describe`] rather
    /// than rendering an empty head line.
    pub fn lines(&self, head_prefix: &str) -> Option<Vec<String>> {
        let verb = self.verb.as_deref()?;
        let target = self.target.as_deref()?;

        let mut head = String::from(head_prefix);
        head.push_str(&capitalize(verb));
        head.push_str(&format!(" '{target}' ({}s)", self.elapsed_secs));

        let mut lines = vec![head];
        if let Some(unit) = self.unit.as_deref() {
            lines.push(format!("{:indent$}{unit}", "", indent = INDENT));
        }
        if let Some(wait) = self.wait.as_deref() {
            let depth = if self.unit.is_some() { 2 } else { 1 };
            lines.push(format!("{:indent$}{wait}", "", indent = INDENT * depth));
        }
        Some(lines)
    }
}

/// Uppercases the first character, leaving the rest untouched.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

struct Op {
    id: u64,
    label: String,
    detail: Option<String>,
    /// Structured mirror of `label`, set when the caller knows the parts.
    parts: Option<OpParts>,
    /// Structured mirror of `detail`, with the unit's own name stripped.
    wait: Option<String>,
    started_at: SystemTime,
    /// Which operation the active detail belongs to. Details are written from
    /// deep inside a project's daemon (dependency waits, health polls) into the
    /// one shared slot, so without an owner a slow project's detail could be
    /// appended to whatever unrelated command happened to be in the slot — the
    /// caller then read a wait belonging to a project it never named.
    owner: Option<String>,
}

/// The pieces of a mutation label, kept apart so the CLI can nest them instead
/// of reading one long prose line.
#[derive(Debug, Clone)]
pub struct OpParts {
    /// Verb naming the mutation, e.g. "restarting".
    pub verb: String,
    /// Project when one is named, otherwise the lone service.
    pub target: String,
    /// Service nested under `target`, when a project owns the head line.
    pub unit: Option<String>,
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
        self.begin_parts(label, None)
    }

    /// Records the start of an operation along with the structured pieces of
    /// its label, so the CLI can render the operation as nested rows.
    pub fn begin_parts(&self, label: impl Into<String>, parts: Option<OpParts>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut guard) = self.inner.lock()
            && guard.as_ref().is_none_or(|op| op.id < id)
        {
            *guard = Some(Op {
                id,
                label: label.into(),
                detail: None,
                parts,
                wait: None,
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

    /// Records a structured operation and clears it when the guard is dropped.
    pub fn guard_parts(&self, label: impl Into<String>, parts: OpParts) -> OpGuard {
        let id = self.begin_parts(label, Some(parts));
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

    /// Updates both the prose detail and its structured form, where `unit` is
    /// the service being worked and `wait` describes what it waits on without
    /// repeating that service's name.
    pub fn detail_for_unit(
        &self,
        owner: &str,
        unit: &str,
        detail: impl Into<String>,
        wait: impl Into<String>,
    ) {
        if let Ok(mut guard) = self.inner.lock()
            && let Some(op) = guard.as_mut()
            && op.label.contains(owner)
        {
            op.detail = Some(detail.into());
            op.wait = Some(wait.into());
            op.owner = Some(owner.to_string());
            if let Some(parts) = op.parts.as_mut() {
                // Always follow the service now reporting. Latching the first
                // one would pair every later service's wait with the wrong
                // name once a project restart walks through its units.
                parts.unit = (parts.target != unit).then(|| unit.to_string());
            }
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
            verb: op.parts.as_ref().map(|parts| parts.verb.clone()),
            target: op.parts.as_ref().map(|parts| parts.target.clone()),
            unit: op.parts.as_ref().and_then(|parts| parts.unit.clone()),
            wait: op.wait.clone(),
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
    fn lines_nest_unit_and_wait_under_the_project() {
        let slot = OpSlot::new();
        slot.begin_parts(
            "restarting 'gamecast__dev' in project 'arbitration-dev'",
            Some(OpParts {
                verb: "restarting".into(),
                target: "arbitration-dev".into(),
                unit: None,
            }),
        );
        slot.detail_for_unit(
            "arbitration-dev",
            "gamecast__dev",
            "health check for 'gamecast__dev' (attempt 3, 4s/60s)",
            "health check (attempt 3, 4s/60s)",
        );

        let lines = slot
            .report()
            .expect("report")
            .lines("")
            .expect("structured");
        assert_eq!(lines[0], "Restarting 'arbitration-dev' (0s)");
        assert_eq!(lines[1], "    gamecast__dev");
        assert_eq!(lines[2], "        health check (attempt 3, 4s/60s)");
    }

    #[test]
    fn lines_do_not_repeat_a_service_that_is_already_the_target() {
        let slot = OpSlot::new();
        slot.begin_parts(
            "restarting 'gamecast__dev'",
            Some(OpParts {
                verb: "restarting".into(),
                target: "gamecast__dev".into(),
                unit: None,
            }),
        );
        slot.detail_for_unit(
            "gamecast__dev",
            "gamecast__dev",
            "health check for 'gamecast__dev' (attempt 1, 0s/60s)",
            "health check (attempt 1, 0s/60s)",
        );

        let lines = slot
            .report()
            .expect("report")
            .lines("")
            .expect("structured");
        assert_eq!(lines.len(), 2, "service must not appear as its own child");
        assert_eq!(lines[0], "Restarting 'gamecast__dev' (0s)");
        assert_eq!(lines[1], "    health check (attempt 1, 0s/60s)");
    }

    #[test]
    fn unit_follows_the_service_currently_reporting() {
        let slot = OpSlot::new();
        slot.begin_parts(
            "restarting all services in project 'arbitration-dev'",
            Some(OpParts {
                verb: "restarting".into(),
                target: "arbitration-dev".into(),
                unit: None,
            }),
        );

        slot.detail_for_unit("arbitration-dev", "migrations", "d", "waiting on 'db'");
        slot.detail_for_unit("arbitration-dev", "gamecast__dev", "d", "health check");

        let lines = slot
            .report()
            .expect("report")
            .lines("")
            .expect("structured");
        assert_eq!(
            lines[1], "    gamecast__dev",
            "a later service must not render under the first service's name"
        );
        assert_eq!(lines[2], "        health check");
    }

    #[test]
    fn lines_fall_back_when_the_supervisor_sent_no_parts() {
        let slot = OpSlot::new();
        slot.begin("restarting 'alpha' in project 'beta'");
        slot.detail("health check");
        assert!(
            slot.report().expect("report").lines("").is_none(),
            "an older supervisor must fall back to the single-line form"
        );
    }

    #[test]
    fn describe_still_renders_the_single_line_form() {
        let slot = OpSlot::new();
        slot.begin_parts(
            "restarting 'gamecast__dev' in project 'arbitration-dev'",
            Some(OpParts {
                verb: "restarting".into(),
                target: "arbitration-dev".into(),
                unit: None,
            }),
        );
        slot.detail_for_unit(
            "arbitration-dev",
            "gamecast__dev",
            "health check for 'gamecast__dev' (attempt 3, 4s/60s)",
            "health check (attempt 3, 4s/60s)",
        );

        assert_eq!(
            slot.report().expect("report").describe(),
            "restarting 'gamecast__dev' in project 'arbitration-dev' — health check for 'gamecast__dev' (attempt 3, 4s/60s) (0s)"
        );
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
