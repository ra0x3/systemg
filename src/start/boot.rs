//! The boot journal: a race-free record of per-unit boot progress.
//!
//! The supervisor boots its services unconditionally — it never waits for a
//! client. As each unit is attempted it appends a [`BootFrame`] to a shared
//! [`BootJournal`]. A `BootStream` subscriber (the `sysg start` parent) is
//! handed every frame recorded so far, then any live frames until [`BootFrame::Done`],
//! so a client that connects late still sees the whole boot. A client that
//! never connects costs nothing.

use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};

use crate::start::Outcome;

/// Where a step within a unit is in its own lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepState {
    /// Running now; the client spins this row.
    Active,
    /// Finished successfully; the client marks it ✔.
    Done,
    /// Gave up; the client marks it ✗.
    Failed,
}

/// One event in a project's boot. Frames are line-delimited JSON on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BootFrame {
    /// A unit's start has been attempted; its outcome is being determined.
    UnitStarting {
        /// The project the unit belongs to.
        project: String,
        /// The service name.
        service: String,
    },
    /// A unit reached its terminal boot outcome.
    Unit {
        /// The project the unit belongs to.
        project: String,
        /// The service name.
        service: String,
        /// Whether it came up, completed, or failed.
        outcome: Outcome,
    },
    /// Progress on one step WITHIN a unit that has not finished yet: a
    /// dependency wait, a pre-start hook, a health check.
    ///
    /// Rides the same ordered stream as the unit it belongs to so a client can
    /// nest it under that unit and resolve it independently — the health check
    /// ticks, turns ✔, and only then does its service. Carried here rather than
    /// in `OpReport` because that slot holds a single snapshot which clears
    /// when the operation ends, so a terminal step state is easily missed.
    UnitStep {
        /// The project the unit belongs to.
        project: String,
        /// The service the step runs under.
        service: String,
        /// Stable identity for the step, so a later state replaces the earlier
        /// row rather than appending a second one.
        id: String,
        /// Human-readable description, e.g. `health check (attempt 8, 14s/300s)`.
        label: String,
        /// Where the step is in its own lifecycle.
        state: StepState,
    },
    /// Boot finished. Terminal frame; nothing follows it.
    Done {
        /// Count of units that came up or completed.
        started: usize,
        /// Count of units that failed to come up.
        failed: usize,
    },
}

impl BootFrame {
    /// Whether this is the terminal frame.
    pub fn is_done(&self) -> bool {
        matches!(self, BootFrame::Done { .. })
    }
}

#[derive(Default)]
struct Inner {
    frames: Vec<BootFrame>,
    done: bool,
}

/// A shared, append-only record of a single boot, with wakeups for subscribers.
///
/// Cloning shares the same underlying record (it is an `Arc` inside), so the
/// booting thread and any subscriber thread observe the same journal.
#[derive(Clone)]
pub struct BootJournal {
    inner: Arc<(Mutex<Inner>, Condvar)>,
}

impl Default for BootJournal {
    fn default() -> Self {
        Self::new()
    }
}

impl BootJournal {
    /// A fresh, empty journal.
    pub fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(Inner::default()), Condvar::new())),
        }
    }

    /// Appends a frame and wakes any waiting subscribers. Appending after
    /// [`BootFrame::Done`] is ignored so the terminal frame stays terminal.
    pub fn push(&self, frame: BootFrame) {
        let (lock, cvar) = &*self.inner;
        let mut guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.done {
            return;
        }
        if frame.is_done() {
            guard.done = true;
        }
        guard.frames.push(frame);
        cvar.notify_all();
    }

    /// Records `outcome` for a unit as a [`BootFrame::Unit`].
    pub fn record(&self, project: &str, service: &str, outcome: Outcome) {
        self.push(BootFrame::Unit {
            project: project.to_string(),
            service: service.to_string(),
            outcome,
        });
    }

    /// Whether boot has finished (a `Done` frame was recorded).
    pub fn is_done(&self) -> bool {
        self.inner
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .done
    }

    /// Every frame in the order recorded, done-or-not. For replay to a new
    /// subscriber.
    pub fn snapshot(&self) -> Vec<BootFrame> {
        self.inner
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frames
            .clone()
    }

    /// Counts of units that succeeded and failed, as `(started, failed)`.
    ///
    /// Derived from the unit frames actually recorded, so a `Done` built from
    /// this reports the operation that happened rather than a placeholder.
    ///
    /// Counts UNITS, not frames: a unit re-reported keeps its latest outcome
    /// instead of being tallied twice. `Skipped` and `Stopped` count as
    /// neither: `started` means "came up or completed", and a unit that never
    /// ran — or that was brought down — did neither.
    pub fn tally(&self) -> (usize, usize) {
        let mut latest: Vec<(&str, &str, &Outcome)> = Vec::new();
        let guard = self
            .inner
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for frame in guard.frames.iter() {
            if let BootFrame::Unit {
                project,
                service,
                outcome,
            } = frame
            {
                match latest
                    .iter_mut()
                    .find(|(p, s, _)| *p == project && *s == service)
                {
                    Some(entry) => entry.2 = outcome,
                    None => latest.push((project, service, outcome)),
                }
            }
        }
        let mut started = 0;
        let mut failed = 0;
        for (_, _, outcome) in latest {
            match outcome {
                Outcome::Skipped | Outcome::Stopped => {}
                other if other.succeeded() => started += 1,
                _ => failed += 1,
            }
        }
        (started, failed)
    }

    /// Blocks until at least `from` frames exist or boot is done, then returns
    /// the frames from index `from` onward. A subscriber loops:
    /// `let next = j.wait_from(seen); seen += next.len();` until it sees `Done`.
    pub fn wait_from(&self, from: usize) -> Vec<BootFrame> {
        let (lock, cvar) = &*self.inner;
        let mut guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while guard.frames.len() <= from && !guard.done {
            guard = cvar
                .wait(guard)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        guard.frames.get(from..).unwrap_or(&[]).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;
    use crate::start::Liveness;

    fn up(service: &str) -> BootFrame {
        BootFrame::Unit {
            project: "p".into(),
            service: service.into(),
            outcome: Outcome::Up(Liveness { pid: 1 }),
        }
    }

    #[test]
    fn tally_counts_units_once_and_excludes_skipped() {
        let j = BootJournal::new();
        j.push(up("a"));
        // The same unit re-reported keeps its latest outcome rather than
        // counting twice.
        j.push(up("a"));
        j.push(BootFrame::Unit {
            project: "p".into(),
            service: "b".into(),
            outcome: Outcome::Skipped,
        });
        j.push(BootFrame::Unit {
            project: "p".into(),
            service: "c".into(),
            outcome: Outcome::Failed(crate::diag::Diagnostic::error(
                crate::diag::SgCode::UnitStartFailed,
                "boom",
            )),
        });
        // Same service name in another project is a different unit.
        j.push(BootFrame::Unit {
            project: "q".into(),
            service: "a".into(),
            outcome: Outcome::Completed,
        });

        assert_eq!(j.tally(), (2, 1));
    }

    #[test]
    fn tally_excludes_stopped_units() {
        let j = BootJournal::new();
        j.push(BootFrame::Unit {
            project: "p".into(),
            service: "a".into(),
            outcome: Outcome::Stopped,
        });
        j.push(up("b"));

        // A stopped unit did not come up or complete, so it is neither a start
        // nor a failure.
        assert_eq!(j.tally(), (1, 0));
    }

    #[test]
    fn tally_takes_the_latest_outcome_for_a_unit() {
        let j = BootJournal::new();
        j.push(up("a"));
        j.push(BootFrame::Unit {
            project: "p".into(),
            service: "a".into(),
            outcome: Outcome::Failed(crate::diag::Diagnostic::error(
                crate::diag::SgCode::UnitStartFailed,
                "died after coming up",
            )),
        });

        assert_eq!(j.tally(), (0, 1));
    }

    #[test]
    fn snapshot_replays_all_recorded_frames() {
        let j = BootJournal::new();
        j.push(up("a"));
        j.push(up("b"));
        j.push(BootFrame::Done {
            started: 2,
            failed: 0,
        });
        let snap = j.snapshot();
        assert_eq!(snap.len(), 3);
        assert!(snap[2].is_done());
        assert!(j.is_done());
    }

    #[test]
    fn push_after_done_is_ignored() {
        let j = BootJournal::new();
        j.push(BootFrame::Done {
            started: 0,
            failed: 0,
        });
        j.push(up("late"));
        assert_eq!(j.snapshot().len(), 1);
    }

    #[test]
    fn wait_from_blocks_until_a_new_frame_arrives() {
        let j = BootJournal::new();
        let producer = j.clone();
        let handle = thread::spawn(move || {
            let first = producer.clone();
            first.push(up("a"));
            first.push(BootFrame::Done {
                started: 1,
                failed: 0,
            });
        });
        // Drain from the start; must eventually observe both frames + Done.
        let mut seen = 0;
        let mut all = Vec::new();
        loop {
            let batch = j.wait_from(seen);
            seen += batch.len();
            let done = batch.iter().any(BootFrame::is_done);
            all.extend(batch);
            if done {
                break;
            }
        }
        handle.join().unwrap();
        assert!(
            all.iter()
                .any(|f| matches!(f, BootFrame::Unit { service, .. } if service == "a"))
        );
        assert!(all.last().unwrap().is_done());
    }
}
