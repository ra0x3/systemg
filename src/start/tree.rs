//! Reduces a boot frame stream into the nested rows shown while an operation
//! runs.
//!
//! The renderer draws whatever this produces, so the ordering and resolution
//! rules live here rather than in the terminal code: a unit appears when it
//! starts, its steps nest beneath it, and each row resolves independently —
//! the health check turns ✔ before the service it belongs to does.
//!
//! Kept free of terminal concerns (no ANSI, no widths, no cursor) so the whole
//! reduction is unit-testable without a TTY.

use crate::start::{BootFrame, Outcome, StepState};

/// How a row should be marked when drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    /// In flight; the renderer spins this row.
    Active,
    /// Finished successfully; marked ✔.
    Done,
    /// Finished unsuccessfully; marked ✗.
    Failed,
}

/// One line of the progress tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    /// Nesting level: 0 for a unit, 1 for a step under it.
    pub depth: usize,
    /// Text to show, without symbol or indent.
    pub label: String,
    /// Mark to draw beside it.
    pub state: RowState,
}

/// Accumulates frames into rows, preserving the order units were worked in.
///
/// A snapshot is taken every render tick, so this holds the whole operation
/// rather than the latest frame: a client attaching mid-operation replays the
/// stream from the start and still sees everything already finished.
#[derive(Debug, Default)]
pub struct TreeState {
    units: Vec<Unit>,
}

#[derive(Debug)]
struct Unit {
    /// Project the unit belongs to; two projects may declare the same name.
    project: String,
    service: String,
    state: RowState,
    steps: Vec<Step>,
}

#[derive(Debug)]
struct Step {
    id: String,
    label: String,
    state: RowState,
}

impl TreeState {
    /// An empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one frame into the tree.
    pub fn apply(&mut self, frame: &BootFrame) {
        match frame {
            BootFrame::UnitStarting { project, service } => {
                let unit = self.unit_mut(project, service);
                unit.state = RowState::Active;
                // A unit can be worked twice in one operation: a restart whose
                // manifest diff bounces a dependent reconciles it, then the
                // cascade revisits it. The second pass starts from nothing, so
                // the first pass's resolved steps must not sit beneath a row
                // that is running again — they describe work already over.
                unit.steps.clear();
            }
            BootFrame::Unit {
                project,
                service,
                outcome,
            } => {
                let state = match outcome {
                    Outcome::Failed(_) => RowState::Failed,
                    _ => RowState::Done,
                };
                let unit = self.unit_mut(project, service);
                unit.state = state;
                // A unit cannot finish with a step still in flight: the step
                // ended, its terminal frame just did not arrive (a path that
                // returns early, or a stream cut short). Leaving it Active
                // would spin a row forever under a finished unit.
                for step in &mut unit.steps {
                    if step.state == RowState::Active {
                        step.state = state;
                    }
                }
            }
            BootFrame::UnitStep {
                project,
                service,
                id,
                label,
                state,
            } => {
                let state = match state {
                    StepState::Active => RowState::Active,
                    StepState::Done => RowState::Done,
                    StepState::Failed => RowState::Failed,
                };
                let unit = self.unit_mut(project, service);
                match unit.steps.iter_mut().find(|step| step.id == *id) {
                    // Matched on the step's stable id so a progressing step
                    // replaces its own row instead of appending a new one per
                    // health-check attempt.
                    Some(existing) => {
                        existing.label = label.clone();
                        existing.state = state;
                    }
                    None => unit.steps.push(Step {
                        id: id.clone(),
                        label: label.clone(),
                        state,
                    }),
                }
            }
            BootFrame::Done { .. } => {}
        }
    }

    /// The rows to draw, units in the order they were worked.
    pub fn rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for unit in &self.units {
            rows.push(TreeRow {
                depth: 0,
                label: unit.service.clone(),
                state: unit.state,
            });
            for step in &unit.steps {
                rows.push(TreeRow {
                    depth: 1,
                    label: step.label.clone(),
                    state: step.state,
                });
            }
        }
        rows
    }

    /// Whether any row is still in flight.
    pub fn is_active(&self) -> bool {
        self.units.iter().any(|unit| {
            unit.state == RowState::Active
                || unit.steps.iter().any(|step| step.state == RowState::Active)
        })
    }

    /// Finds a unit by name, appending it when first seen so units keep the
    /// order the supervisor worked them in.
    fn unit_mut(&mut self, project: &str, service: &str) -> &mut Unit {
        if let Some(index) = self
            .units
            .iter()
            .position(|unit| unit.service == service && unit.project == project)
        {
            return &mut self.units[index];
        }
        self.units.push(Unit {
            project: project.to_string(),
            service: service.to_string(),
            state: RowState::Active,
            steps: Vec::new(),
        });
        self.units.last_mut().expect("just pushed")
    }
}

/// Selects the rows that fit `height`, keeping the head, whatever is active,
/// and the most recent finished work.
///
/// A project with more units than the terminal has lines would otherwise draw
/// past the bottom, and the cursor-up arithmetic that repaints in place would
/// then walk to the wrong row and smear the screen. Older finished rows are
/// dropped first and replaced by a count, since the active subtree is what the
/// user is waiting on.
pub fn fit_rows(rows: &[TreeRow], height: usize) -> (Vec<TreeRow>, usize) {
    // No room at all hides everything and says so, rather than returning rows
    // the caller has nowhere to draw.
    if height == 0 {
        return (Vec::new(), rows.len());
    }
    if rows.len() <= height {
        return (rows.to_vec(), 0);
    }

    // Never sever a step from its unit: a step row whose unit was dropped would
    // render as an orphan with no context. Walking FORWARD to the next unit can
    // discard the active subtree entirely, so the cut walks BACKWARD to the unit
    // that owns the first kept row — the tail is what the user is waiting on.
    let mut start = rows.len() - height;
    while start > 0 && rows[start].depth > 0 {
        start -= 1;
    }

    // Walking backward ADDS rows, so the kept tail can now exceed the height it
    // was supposed to fit — which is the overflow this function exists to
    // prevent. When the owning unit's subtree cannot fit, drop it whole and
    // start at the next unit rather than emitting an orphan or overflowing.
    if rows.len() - start > height {
        let owner = start;
        let mut next = start + 1;
        while next < rows.len() && rows[next].depth > 0 {
            next += 1;
        }
        if rows.len() - next <= height && next < rows.len() {
            // The following units fit on their own, so drop this oversized
            // subtree whole rather than showing a fragment of it.
            return (rows[next..].to_vec(), next);
        }
        // Nothing after it fits either, so this subtree is all there is to
        // show. Keep its unit row and the LAST of its steps: the unit is what
        // gives the steps meaning, and the tail is the live end of the work.
        // With a single line to draw, the unit alone is the honest row.
        if height == 1 {
            return (vec![rows[owner].clone()], rows.len() - 1);
        }
        let mut kept = vec![rows[owner].clone()];
        let tail = rows.len() - (height - 1);
        kept.extend_from_slice(&rows[tail..]);
        return (kept, rows.len() - height);
    }

    (rows[start..].to_vec(), start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start::Liveness;

    fn starting(service: &str) -> BootFrame {
        BootFrame::UnitStarting {
            project: "p".into(),
            service: service.into(),
        }
    }

    fn finished(service: &str, outcome: Outcome) -> BootFrame {
        BootFrame::Unit {
            project: "p".into(),
            service: service.into(),
            outcome,
        }
    }

    fn step(service: &str, id: &str, label: &str, state: StepState) -> BootFrame {
        BootFrame::UnitStep {
            project: "p".into(),
            service: service.into(),
            id: id.into(),
            label: label.into(),
            state,
        }
    }

    fn reduce(frames: &[BootFrame]) -> Vec<TreeRow> {
        let mut tree = TreeState::new();
        for frame in frames {
            tree.apply(frame);
        }
        tree.rows()
    }

    #[test]
    fn units_keep_the_order_they_were_worked() {
        let rows = reduce(&[
            starting("migrations"),
            finished("migrations", Outcome::Completed),
            starting("api"),
            starting("worker"),
        ]);

        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["migrations", "api", "worker"]);
        assert_eq!(rows[0].state, RowState::Done);
        assert_eq!(rows[1].state, RowState::Active);
    }

    #[test]
    fn reworking_a_unit_drops_the_previous_pass_steps() {
        let rows = reduce(&[
            starting("api"),
            step("api", "health", "health check (attempt 1)", StepState::Done),
            finished("api", Outcome::Up(Liveness { pid: 7 })),
            starting("api"),
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "api");
        assert_eq!(rows[0].state, RowState::Active);
    }

    #[test]
    fn a_step_nests_under_its_unit_and_resolves_before_it() {
        let rows = reduce(&[
            starting("api"),
            step(
                "api",
                "health",
                "health check (attempt 8)",
                StepState::Active,
            ),
        ]);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[1].state, RowState::Active);

        // The step finishes first; its unit is still coming up.
        let rows = reduce(&[
            starting("api"),
            step(
                "api",
                "health",
                "health check (attempt 8)",
                StepState::Active,
            ),
            step("api", "health", "health check", StepState::Done),
        ]);
        assert_eq!(rows[1].state, RowState::Done);
        assert_eq!(
            rows[0].state,
            RowState::Active,
            "the unit is still working after its step resolved"
        );
    }

    #[test]
    fn a_progressing_step_replaces_its_row_rather_than_appending() {
        let rows = reduce(&[
            starting("api"),
            step(
                "api",
                "health",
                "health check (attempt 1)",
                StepState::Active,
            ),
            step(
                "api",
                "health",
                "health check (attempt 2)",
                StepState::Active,
            ),
            step(
                "api",
                "health",
                "health check (attempt 3)",
                StepState::Active,
            ),
        ]);

        assert_eq!(rows.len(), 2, "one unit row and ONE step row");
        assert_eq!(rows[1].label, "health check (attempt 3)");
    }

    #[test]
    fn a_failed_health_check_marks_the_step_and_its_unit() {
        let rows = reduce(&[
            starting("api"),
            step("api", "health", "health check", StepState::Active),
            step(
                "api",
                "health",
                "health check (7 attempts)",
                StepState::Failed,
            ),
            finished(
                "api",
                Outcome::Failed(crate::start::unit_start_failed("api", "unhealthy")),
            ),
        ]);

        assert_eq!(rows[0].state, RowState::Failed);
        assert_eq!(rows[1].state, RowState::Failed);
    }

    #[test]
    fn a_skipped_unit_is_done_not_failed() {
        let rows = reduce(&[finished("legacy", Outcome::Skipped)]);
        assert_eq!(
            rows[0].state,
            RowState::Done,
            "an intentional skip is not a failure"
        );
    }

    #[test]
    fn a_stopped_unit_is_done() {
        let rows = reduce(&[starting("api"), finished("api", Outcome::Stopped)]);
        assert_eq!(rows[0].state, RowState::Done);
    }

    #[test]
    fn a_unit_that_finishes_never_leaves_a_step_spinning() {
        let rows = reduce(&[
            starting("api"),
            step("api", "dep", "dependency 'migrations'", StepState::Active),
            finished("api", Outcome::Up(Liveness { pid: 42 })),
        ]);

        assert_eq!(rows[1].state, RowState::Done);
        assert!(
            !TreeState::new().is_active(),
            "an empty tree has nothing in flight"
        );
    }

    #[test]
    fn is_active_tracks_whether_anything_is_still_running() {
        let mut tree = TreeState::new();
        tree.apply(&starting("api"));
        assert!(tree.is_active());

        tree.apply(&finished("api", Outcome::Up(Liveness { pid: 1 })));
        assert!(!tree.is_active());
    }

    #[test]
    fn fit_rows_keeps_everything_when_it_fits() {
        let rows = reduce(&[starting("a"), starting("b")]);
        let (shown, hidden) = fit_rows(&rows, 10);
        assert_eq!(shown.len(), 2);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn fit_rows_drops_oldest_finished_work_first() {
        let rows = reduce(&[
            finished("a", Outcome::Completed),
            finished("b", Outcome::Completed),
            finished("c", Outcome::Completed),
            starting("d"),
        ]);

        let (shown, hidden) = fit_rows(&rows, 2);
        assert_eq!(hidden, 2, "the two oldest rows are elided");
        assert_eq!(shown.len(), 2);
        assert_eq!(shown.last().expect("a row").label, "d");
    }

    #[test]
    fn fit_rows_never_shows_a_step_without_its_unit() {
        let rows = reduce(&[
            finished("a", Outcome::Completed),
            starting("b"),
            step("b", "health", "health check", StepState::Active),
        ]);

        // A naive tail would start at the step and orphan it from unit `b`.
        let (shown, _) = fit_rows(&rows, 2);
        assert!(
            shown.first().is_none_or(|row| row.depth == 0),
            "the first shown row must be a unit, never a dangling step"
        );
    }

    #[test]
    fn fit_rows_never_exceeds_height_when_walking_back_to_a_unit() {
        let rows = reduce(&[
            finished("a", Outcome::Completed),
            starting("b"),
            step("b", "dep", "waiting for dep", StepState::Active),
            step("b", "health", "health check", StepState::Active),
        ]);

        // Walking backward from the tail reaches unit `b`, whose subtree is 3
        // rows — one more than fits. Returning it anyway drew past the bottom
        // and smeared the repaint.
        let (shown, hidden) = fit_rows(&rows, 2);
        assert!(shown.len() <= 2, "kept {} rows for height 2", shown.len());
        assert_eq!(
            hidden + shown.len(),
            rows.len(),
            "every row is kept or counted"
        );
        assert!(
            shown.first().is_none_or(|row| row.depth == 0),
            "the first shown row must be a unit, never a dangling step"
        );
    }

    #[test]
    fn fit_rows_fits_a_single_subtree_taller_than_the_terminal() {
        let rows = reduce(&[
            starting("solo"),
            step("solo", "one", "step one", StepState::Active),
            step("solo", "two", "step two", StepState::Active),
            step("solo", "three", "step three", StepState::Active),
        ]);

        // There is no next unit to fall forward to, so the tail is all that can
        // be shown — but it still must not overflow.
        let (shown, hidden) = fit_rows(&rows, 2);
        assert_eq!(shown.len(), 2);
        assert_eq!(hidden, 2);
    }
}
