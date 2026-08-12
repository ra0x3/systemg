//! Invariant oracle: `check_world()`.
//!
//! The oracle reads the persisted runtime state across every project and
//! checks the invariants that must hold for a healthy supervisor world. It is
//! the multiplier that other hardening techniques drive: any harness that
//! mutates the supervisor (fuzz sequences, model tests, soak) can call this
//! after each step and let it catch the violation, instead of hand-writing a
//! bespoke assertion per scenario.
//!
//! Findings are structured so `sysg doctor` can render them and tests can
//! branch on them. Safety-critical violations are `Error`; suspicious but
//! recoverable observations are `Warn`.

use std::{fmt, fs};

use serde::Serialize;

use crate::{
    daemon::{PidFile, ServiceLifecycleStatus, ServiceStateFile},
    runtime::{self, RuntimeMode},
    state_store::{PROJECTS_DIR, StateStore},
};

/// Severity of an invariant finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A safety invariant is violated; the world is in an unsafe state.
    Error,
    /// Suspicious but recoverable; worth surfacing, not a hard failure.
    Warn,
}

/// A single invariant violation.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Severity of the violation.
    pub severity: Severity,
    /// Stable machine-readable rule name (e.g. `running-pid-dead`).
    pub rule: String,
    /// Project this finding concerns, if scoped to one.
    pub project: Option<String>,
    /// Service this finding concerns, if scoped to one.
    pub service: Option<String>,
    /// Human-readable description of what is wrong.
    pub detail: String,
}

/// The result of one invariant sweep.
#[derive(Debug, Clone, Serialize)]
pub struct WorldReport {
    /// Runtime mode the sweep ran against.
    pub mode: String,
    /// Number of projects inspected.
    pub projects: usize,
    /// All findings, most severe first.
    pub findings: Vec<Finding>,
}

impl WorldReport {
    /// Whether any `Error`-severity invariant is violated.
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Whether the world is fully consistent (no findings at all).
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

impl fmt::Display for WorldReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_clean() {
            return write!(
                f,
                "doctor: {} project(s) consistent in {} mode",
                self.projects, self.mode
            );
        }
        writeln!(
            f,
            "doctor: {} finding(s) across {} project(s) in {} mode",
            self.findings.len(),
            self.projects,
            self.mode
        )?;
        for finding in &self.findings {
            let sev = match finding.severity {
                Severity::Error => "error",
                Severity::Warn => "warn",
            };
            let scope = match (&finding.project, &finding.service) {
                (Some(p), Some(s)) => format!(" [{p}/{s}]"),
                (Some(p), None) => format!(" [{p}]"),
                _ => String::new(),
            };
            writeln!(f, "  {sev} {}{scope}: {}", finding.rule, finding.detail)?;
        }
        Ok(())
    }
}

/// Whether a pid is addressable (alive, possibly a zombie).
fn pid_addressable(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

/// Whether a pid is a zombie/dead per procfs (Linux) — an addressable pid that
/// is not actually a live process.
#[cfg(target_os = "linux")]
fn pid_is_zombie(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // The state char follows the `(comm)` field; find the last ')'.
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next())
        .is_some_and(|c| c == 'Z' || c == 'X')
}

#[cfg(not(target_os = "linux"))]
fn pid_is_zombie(_pid: u32) -> bool {
    false
}

/// Runs the invariant sweep over the current runtime's persisted state. Pure
/// with respect to the world: it only reads, never mutates.
pub fn check_world() -> WorldReport {
    let mode = match runtime::mode() {
        RuntimeMode::System => "system",
        RuntimeMode::User => "user",
    };
    let mut findings = Vec::new();

    // Invariant: in system mode the runtime lives under the system paths, never
    // a user home. A --sys world writing into ~/.local would be a mode leak.
    let state_dir = runtime::state_dir();
    if runtime::mode() == RuntimeMode::System
        && !state_dir.starts_with("/var/lib")
        && !state_dir.starts_with("/Library")
    {
        findings.push(Finding {
            severity: Severity::Error,
            rule: "system-mode-user-path".into(),
            project: None,
            service: None,
            detail: format!("system-mode state dir is {state_dir:?}, not a system path"),
        });
    }

    let projects_root = state_dir.join(PROJECTS_DIR);
    let entries = match fs::read_dir(&projects_root) {
        Ok(entries) => entries,
        Err(_) => {
            return WorldReport {
                mode: mode.into(),
                projects: 0,
                findings,
            };
        }
    };

    let mut project_count = 0;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        project_count += 1;
        let project = entry.file_name().to_string_lossy().to_string();
        let store = StateStore::at(entry.path());
        check_project(&project, store, &mut findings);
    }

    findings.sort_by_key(|f| match f.severity {
        Severity::Error => 0,
        Severity::Warn => 1,
    });

    WorldReport {
        mode: mode.into(),
        projects: project_count,
        findings,
    }
}

/// Checks one project's persisted state against the invariants.
fn check_project(project: &str, store: StateStore, findings: &mut Vec<Finding>) {
    let pid_file = PidFile::load(store.clone()).ok();
    let state_file = ServiceStateFile::load(store).ok();

    let push = |findings: &mut Vec<Finding>,
                severity: Severity,
                rule: &str,
                service: Option<&str>,
                detail: String| {
        findings.push(Finding {
            severity,
            rule: rule.into(),
            project: Some(project.to_string()),
            service: service.map(str::to_string),
            detail,
        });
    };

    // Invariant: a service recorded Running must have a live, non-zombie pid.
    // This is the "status lied" bug class — the single most consequential
    // invariant for a root supervisor.
    if let Some(state) = &state_file {
        for (key, record) in state.services() {
            let service = key.rsplit(':').next().unwrap_or(key);
            if record.status == ServiceLifecycleStatus::Running {
                match record.pid {
                    None => push(
                        findings,
                        Severity::Error,
                        "running-without-pid",
                        Some(service),
                        "recorded Running but has no pid".into(),
                    ),
                    Some(pid) if !pid_addressable(pid) => push(
                        findings,
                        Severity::Error,
                        "running-pid-dead",
                        Some(service),
                        format!("recorded Running but pid {pid} is not alive"),
                    ),
                    Some(pid) if pid_is_zombie(pid) => push(
                        findings,
                        Severity::Error,
                        "running-pid-zombie",
                        Some(service),
                        format!("recorded Running but pid {pid} is a zombie"),
                    ),
                    Some(_) => {}
                }
            }
        }
    }

    // Invariant: every pid in the pid map must be addressable. A stale pidfile
    // entry for a dead process is the "ghost"/stale-state class.
    if let Some(pids) = &pid_file {
        for (service, pid) in pids.services() {
            if !pid_addressable(*pid) {
                push(
                    findings,
                    Severity::Warn,
                    "pidfile-stale-entry",
                    Some(service),
                    format!("pid map lists {pid} which is not alive"),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ServiceLifecycleStatus;

    /// A pid that is guaranteed dead: our own pid + a large offset is almost
    /// never a live process, and we assert it is unaddressable.
    fn a_dead_pid() -> u32 {
        let candidate = std::process::id() + 1_000_000;
        assert!(!pid_addressable(candidate), "test pid unexpectedly alive");
        candidate
    }

    fn temp_store() -> (tempfile::TempDir, StateStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StateStore::at(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn running_with_dead_pid_is_an_error() {
        let (_dir, store) = temp_store();
        let mut state = ServiceStateFile::load(store.clone()).expect("load state");
        state.set_in_memory(
            "v2:demo:web",
            ServiceLifecycleStatus::Running,
            Some(a_dead_pid()),
            None,
            None,
        );
        state.save().expect("save state");

        let mut findings = Vec::new();
        check_project("demo", store, &mut findings);
        assert!(
            findings
                .iter()
                .any(|f| f.rule == "running-pid-dead" && f.severity == Severity::Error),
            "expected running-pid-dead error, got {findings:?}"
        );
    }

    #[test]
    fn running_without_pid_is_an_error() {
        let (_dir, store) = temp_store();
        let mut state = ServiceStateFile::load(store.clone()).expect("load state");
        state.set_in_memory(
            "v2:demo:web",
            ServiceLifecycleStatus::Running,
            None,
            None,
            None,
        );
        state.save().expect("save state");

        let mut findings = Vec::new();
        check_project("demo", store, &mut findings);
        assert!(
            findings.iter().any(|f| f.rule == "running-without-pid"),
            "expected running-without-pid, got {findings:?}"
        );
    }

    #[test]
    fn running_with_live_pid_is_clean() {
        let (_dir, store) = temp_store();
        let mut state = ServiceStateFile::load(store.clone()).expect("load state");
        // Our own pid is live and not a zombie.
        state.set_in_memory(
            "v2:demo:web",
            ServiceLifecycleStatus::Running,
            Some(std::process::id()),
            None,
            None,
        );
        state.save().expect("save state");

        let mut findings = Vec::new();
        check_project("demo", store, &mut findings);
        assert!(
            findings.is_empty(),
            "a live Running service should be clean, got {findings:?}"
        );
    }

    #[test]
    fn stopped_service_with_no_pid_is_clean() {
        let (_dir, store) = temp_store();
        let mut state = ServiceStateFile::load(store.clone()).expect("load state");
        state.set_in_memory(
            "v2:demo:web",
            ServiceLifecycleStatus::Stopped,
            None,
            Some(0),
            None,
        );
        state.save().expect("save state");

        let mut findings = Vec::new();
        check_project("demo", store, &mut findings);
        assert!(findings.is_empty(), "stopped is fine, got {findings:?}");
    }
}
