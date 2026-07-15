//! Per-project on-disk state layout.
//!
//! Every project owns a directory `{state_dir}/projects/{project_id}/` holding
//! its own `pid.xml`, `state.xml`, and `cron_state.xml`. A `StateStore` is the
//! single source of those paths — nothing else in the codebase should join a
//! state-file name onto the raw state dir. Project-less ("loose") services live
//! under the `__loose__` directory so the layout is uniform.

use std::path::PathBuf;

use crate::{
    constants::{PID_FILE_NAME, PID_LOCK_SUFFIX, STATE_FILE_NAME},
    runtime,
};

/// Directory name holding every project's state directory.
pub const PROJECTS_DIR: &str = "projects";

/// Project id used for the project-less ("loose") service bundle.
pub const LOOSE_PROJECT_ID: &str = "__loose__";

/// Name of the cron state file within a project directory.
pub const CRON_FILE_NAME: &str = "cron_state.xml";

/// Resolves the on-disk paths for a single project's state files.
///
/// The [`Default`] value is an empty, unusable placeholder — it exists only so
/// state-file structs can carry a `#[serde(skip)]` store field. Every real
/// handle is bound to a concrete store by `load`/`reload` before any IO.
#[derive(Debug, Clone, Default)]
pub struct StateStore {
    dir: PathBuf,
}

impl StateStore {
    /// Store for `project_id`, rooted at `{state_dir}/projects/{project_id}/`.
    /// An empty id maps to the loose bundle so callers never produce a bare
    /// `projects/` path.
    pub fn for_project(project_id: &str) -> Self {
        let id = if project_id.is_empty() {
            LOOSE_PROJECT_ID
        } else {
            project_id
        };
        Self::at(runtime::state_dir().join(PROJECTS_DIR).join(id))
    }

    /// Store for the loose service bundle.
    pub fn loose() -> Self {
        Self::for_project(LOOSE_PROJECT_ID)
    }

    /// Store rooted at an explicit directory (tests, custom homes).
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The project's state directory.
    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    /// Path to the project's PID file.
    pub fn pid_path(&self) -> PathBuf {
        self.dir.join(PID_FILE_NAME)
    }

    /// Path to the PID file's lock.
    pub fn pid_lock_path(&self) -> PathBuf {
        self.dir
            .join(format!("{}{}", PID_FILE_NAME, PID_LOCK_SUFFIX))
    }

    /// Path to the project's service-state file.
    pub fn state_path(&self) -> PathBuf {
        self.dir.join(STATE_FILE_NAME)
    }

    /// Path to the service-state file's lock.
    pub fn state_lock_path(&self) -> PathBuf {
        self.dir
            .join(format!("{}{}", STATE_FILE_NAME, PID_LOCK_SUFFIX))
    }

    /// Path to the project's cron-state file.
    pub fn cron_path(&self) -> PathBuf {
        self.dir.join(CRON_FILE_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_id_maps_to_loose() {
        let a = StateStore::for_project("");
        let b = StateStore::loose();
        assert_eq!(a.dir(), b.dir());
        assert!(a.dir().ends_with(LOOSE_PROJECT_ID));
    }

    #[test]
    fn distinct_projects_get_distinct_dirs() {
        let a = StateStore::for_project("alpha");
        let b = StateStore::for_project("beta");
        assert_ne!(a.dir(), b.dir());
        assert!(a.pid_path() != b.pid_path());
        assert!(a.state_path() != b.state_path());
        assert!(a.cron_path() != b.cron_path());
    }

    #[test]
    fn paths_nest_under_project_dir() {
        let s = StateStore::at(PathBuf::from("/x/projects/alpha"));
        assert_eq!(s.pid_path(), PathBuf::from("/x/projects/alpha/pid.xml"));
        assert_eq!(
            s.pid_lock_path(),
            PathBuf::from("/x/projects/alpha/pid.xml.lock")
        );
        assert_eq!(s.state_path(), PathBuf::from("/x/projects/alpha/state.xml"));
        assert_eq!(
            s.cron_path(),
            PathBuf::from("/x/projects/alpha/cron_state.xml")
        );
    }
}
