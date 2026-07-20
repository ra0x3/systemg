//! Resolving `purge`'s selectors into one plan, plus a preflight that refuses to
//! wipe state out from under a live supervisor.
//!
//! Purge deletes on-disk state, so it is irreversible — the guard matters more
//! than the deletion. Two ideas keep it honest:
//!
//! - [`PurgePlan`] — an exhaustive enum of *what* to wipe: the whole state root,
//!   every project a config declares, or one project.
//! - [`preflight`] — a total check of *whether the world permits it*. A purge is
//!   refused (SG0401) when a supervisor is serving and still managing units,
//!   unless `--force` is set. Nothing is deleted until preflight passes.

use crate::{
    diag::{Diagnostic, SgCode},
    selector::{ProjectMismatch, Target, resolve_target},
};

/// What a `purge` invocation wipes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurgePlan {
    /// The whole state root: every project, `__loose__`, logs, runtime files.
    Everything,
    /// Every project a config declares (resolved to project ids by the caller).
    Config {
        /// The project ids the config declares.
        projects: Vec<String>,
    },
    /// One project's state directory.
    Project {
        /// The project id.
        project: String,
    },
}

/// Resolves the selectors into a base [`PurgePlan`], before preflight.
///
/// No selector wipes everything. A `-p <id>` scopes to one project. A `-c` with
/// no `-p` is expanded by the caller into the config's project ids and passed as
/// `config_projects`; here it becomes [`PurgePlan::Config`]. A `-s` selector is
/// meaningless for purge (state is per-project, not per-service) and is rejected.
pub fn resolve_plan(
    service: Option<&str>,
    project: Option<&str>,
    config_projects: Option<Vec<String>>,
) -> Result<PurgePlan, ProjectMismatch> {
    Ok(match resolve_target(service, project)? {
        Target::Everything => match config_projects {
            Some(projects) => PurgePlan::Config { projects },
            None => PurgePlan::Everything,
        },
        Target::Project { project } => PurgePlan::Project { project },
        Target::Service { service, project } => PurgePlan::Project {
            project: project.unwrap_or(service),
        },
    })
}

/// A snapshot of the world that preflight inspects. Kept small and explicit so
/// preflight stays a pure decision over known facts.
#[derive(Debug, Clone, Copy)]
pub struct World {
    /// Whether a supervisor is alive and answering its control socket.
    pub supervisor_serving: bool,
    /// How many units the resident supervisor is currently managing.
    pub managed_units: usize,
    /// Whether `--force` was passed, overriding the live-supervisor refusal.
    pub force: bool,
}

/// The outcome of preflight: a plan cleared to delete, or a refusal.
#[derive(Debug)]
pub enum Preflight {
    /// The plan passed preflight and may delete its targets.
    Ready(PurgePlan),
    /// The plan is refused; render this diagnostic and delete nothing.
    Refused(Box<Diagnostic>),
}

/// Checks whether `plan` is legal given `world`, before any deletion.
///
/// A live supervisor still managing processes owns the state purge would delete;
/// wiping it mid-flight strands those processes and corrupts the supervisor's
/// view. Refuse with SG0401 unless `--force` says the caller accepts the
/// teardown. A supervisor that is down, or serving with nothing managed, is safe
/// to purge.
pub fn preflight(plan: PurgePlan, world: World) -> Preflight {
    if world.supervisor_serving && world.managed_units > 0 && !world.force {
        return Preflight::Refused(Box::new(supervisor_active(world.managed_units)));
    }
    Preflight::Ready(plan)
}

/// Builds the SG0401 diagnostic for a purge refused because a live supervisor is
/// still managing processes.
pub fn supervisor_active(managed_units: usize) -> Diagnostic {
    Diagnostic::error(
        SgCode::PurgeSupervisorActive,
        "refused to purge: a supervisor is still managing processes",
    )
    .note(format!(
        "{managed_units} unit(s) are live under the running supervisor; purging its state now would strand them"
    ))
    .help_cmd("stop the supervisor first", "sysg stop --supervisor")
    .help_cmd("then purge", "sysg purge")
    .help_cmd("or force it (stops + wipes)", "sysg purge --force")
    .help_docs()
}

/// Builds the SG0402 diagnostic for a purge that removed some state but hit an IO
/// error before finishing, so the on-disk state may be partial.
pub fn incomplete(detail: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        SgCode::PurgeIncomplete,
        "purge removed some state but did not finish; the remaining state may be partial",
    )
    .note(detail)
    .help_cmd("retry the purge", "sysg purge")
    .help_docs()
}

/// Builds the SG0403 diagnostic for a scoped purge naming a project with no state
/// on disk.
pub fn project_not_found(project: &str) -> Diagnostic {
    Diagnostic::error(
        SgCode::PurgeProjectNotFound,
        format!("no state on disk for project '{project}'"),
    )
    .note("nothing was deleted; check the project id")
    .help_cmd("list what has state", "sysg status")
    .help_docs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_selector_wipes_everything() {
        assert_eq!(
            resolve_plan(None, None, None).unwrap(),
            PurgePlan::Everything
        );
    }

    #[test]
    fn config_projects_become_a_config_purge() {
        assert_eq!(
            resolve_plan(None, None, Some(vec!["a".into(), "b".into()])).unwrap(),
            PurgePlan::Config {
                projects: vec!["a".into(), "b".into()]
            }
        );
    }

    #[test]
    fn project_selector_scopes_to_one_project() {
        assert_eq!(
            resolve_plan(None, Some("demo"), None).unwrap(),
            PurgePlan::Project {
                project: "demo".into()
            }
        );
    }

    #[test]
    fn preflight_refuses_a_live_managing_supervisor() {
        let world = World {
            supervisor_serving: true,
            managed_units: 3,
            force: false,
        };
        match preflight(PurgePlan::Everything, world) {
            Preflight::Refused(diag) => {
                assert_eq!(diag.code, SgCode::PurgeSupervisorActive)
            }
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn preflight_allows_force_over_a_live_supervisor() {
        let world = World {
            supervisor_serving: true,
            managed_units: 3,
            force: true,
        };
        assert!(matches!(
            preflight(PurgePlan::Everything, world),
            Preflight::Ready(_)
        ));
    }

    #[test]
    fn preflight_allows_a_down_supervisor() {
        let world = World {
            supervisor_serving: false,
            managed_units: 0,
            force: false,
        };
        assert!(matches!(
            preflight(PurgePlan::Everything, world),
            Preflight::Ready(_)
        ));
    }

    #[test]
    fn preflight_allows_a_serving_but_empty_supervisor() {
        let world = World {
            supervisor_serving: true,
            managed_units: 0,
            force: false,
        };
        assert!(matches!(
            preflight(PurgePlan::Everything, world),
            Preflight::Ready(_)
        ));
    }
}
