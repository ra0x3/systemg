//! Planning and attribution for the legacy loose-state migration.
//!
//! Legacy artifacts under `projects/__loose__/` carry only a service name, while
//! the new layout keys state by the project id derived from the manifest that
//! declared the service. Attribution therefore has to run backwards: scan the
//! candidate manifests, and work out which one owns each service name.
//!
//! That mapping is not always knowable. Three `gamecast-tunnel-*.yaml` files can
//! each declare a service called `gamecast-tunnel`, and a state row saying
//! `gamecast-tunnel` cannot distinguish them. The planner never guesses: an
//! ambiguous artifact is quarantined into the archive and reported, and the user
//! can resolve it explicitly.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use crate::{
    config::{Config, loose_project_id},
    state_store::LOOSE_PROJECT_ID,
};

/// Why a legacy service could not be attributed to exactly one manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    /// No scanned manifest declares a service by this name.
    NoCandidate,
    /// More than one manifest declares it, and nothing distinguishes them.
    Ambiguous(Vec<String>),
}

/// How one legacy service name was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attribution {
    /// Exactly one manifest declares this service.
    Resolved {
        /// Canonical path of the owning manifest.
        config_path: PathBuf,
        /// Project id derived from that path.
        project_id: String,
    },
    /// The service could not be attributed; it is archived instead.
    Quarantined(Unresolved),
}

impl Attribution {
    /// The derived project id when this service was attributed.
    pub fn project_id(&self) -> Option<&str> {
        match self {
            Self::Resolved { project_id, .. } => Some(project_id),
            Self::Quarantined(_) => None,
        }
    }
}

/// A manifest considered as an owner of legacy services.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Canonical path of the manifest.
    pub config_path: PathBuf,
    /// Project id derived from that path.
    pub project_id: String,
    /// Service names the manifest declares.
    pub services: BTreeSet<String>,
    /// Service name to config hash, for matching cron rows.
    pub service_hashes: BTreeMap<String, String>,
}

impl Candidate {
    /// Builds a candidate from a loaded config at `config_path`.
    ///
    /// Only project-less configs are candidates: a manifest that names its own
    /// project never had state under `__loose__` to migrate.
    pub fn from_config(config: &Config, config_path: &Path) -> Option<Self> {
        if !config.project.loose {
            return None;
        }
        Some(Self {
            config_path: config_path.to_path_buf(),
            project_id: loose_project_id(config_path),
            services: config.services.keys().cloned().collect(),
            service_hashes: config
                .service_hashes()
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
        })
    }
}

/// The resolved disposition of every legacy artifact.
#[derive(Debug, Clone, Default)]
pub struct MigrationPlan {
    /// Attribution for each legacy service name found in pid/state files.
    pub services: BTreeMap<String, Attribution>,
    /// Attribution for each legacy cron job, keyed by its stored hash.
    pub cron_jobs: BTreeMap<String, Attribution>,
    /// Attribution for each legacy log file, keyed by file name.
    pub logs: BTreeMap<String, Attribution>,
}

impl MigrationPlan {
    /// Whether anything at all needs migrating.
    pub fn is_empty(&self) -> bool {
        self.services.is_empty() && self.cron_jobs.is_empty() && self.logs.is_empty()
    }

    /// Every distinct project id this plan will write state into.
    pub fn target_projects(&self) -> BTreeSet<&str> {
        self.services
            .values()
            .chain(self.cron_jobs.values())
            .chain(self.logs.values())
            .filter_map(Attribution::project_id)
            .collect()
    }

    /// Artifacts that could not be attributed, as `(kind, name, reason)`.
    pub fn quarantined(&self) -> Vec<(&'static str, &str, &Unresolved)> {
        let groups = [
            ("service", &self.services),
            ("cron", &self.cron_jobs),
            ("log", &self.logs),
        ];
        let mut out = Vec::new();
        for (kind, group) in groups {
            for (name, attribution) in group {
                if let Attribution::Quarantined(reason) = attribution {
                    out.push((kind, name.as_str(), reason));
                }
            }
        }
        out
    }
}

/// Attributes a service name to the single candidate declaring it.
pub fn attribute_service(name: &str, candidates: &[Candidate]) -> Attribution {
    let owners: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.services.contains(name))
        .collect();
    resolve_owners(owners)
}

/// Attributes a cron job to a candidate by its stored config hash, falling back
/// to the job's service name.
///
/// The hash is tried first because it survives a rename: a job whose service was
/// renamed still hashes to the same value. The name is only consulted when no
/// hash matches, which is the common case for rows left over from a manifest
/// that no longer exists.
pub fn attribute_cron_job(
    service_name: &str,
    service_hash: &str,
    candidates: &[Candidate],
) -> Attribution {
    let by_hash: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .service_hashes
                .values()
                .any(|hash| hash == service_hash)
        })
        .collect();
    if !by_hash.is_empty() {
        return resolve_owners(by_hash);
    }
    attribute_service(service_name, candidates)
}

/// Attributes a log file to the candidate declaring the service it is named for.
pub fn attribute_log(file_name: &str, candidates: &[Candidate]) -> Attribution {
    let service = log_service_name(file_name);
    attribute_service(&service, candidates)
}

/// The service name a legacy log file belongs to, stripping the stream suffix
/// and extension the log writer appends.
pub fn log_service_name(file_name: &str) -> String {
    let stem = Path::new(file_name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.to_string());
    for suffix in ["_stdout", "_stderr"] {
        if let Some(base) = stem.strip_suffix(suffix) {
            return base.to_string();
        }
    }
    stem
}

fn resolve_owners(owners: Vec<&Candidate>) -> Attribution {
    match owners.as_slice() {
        [] => Attribution::Quarantined(Unresolved::NoCandidate),
        [only] => Attribution::Resolved {
            config_path: only.config_path.clone(),
            project_id: only.project_id.clone(),
        },
        many => Attribution::Quarantined(Unresolved::Ambiguous(
            many.iter()
                .map(|candidate| candidate.config_path.to_string_lossy().to_string())
                .collect(),
        )),
    }
}

/// Whether legacy loose state exists under `state_dir` or `log_dir`.
///
/// Both are passed in rather than derived: in system mode logs live outside the
/// state root, so one cannot be computed from the other.
pub fn legacy_state_present(state_dir: &Path, log_dir: &Path) -> bool {
    legacy_project_dir(state_dir).exists() || legacy_log_dir(log_dir).exists()
}

/// The legacy loose state directory.
pub fn legacy_project_dir(state_dir: &Path) -> PathBuf {
    state_dir
        .join(crate::state_store::PROJECTS_DIR)
        .join(LOOSE_PROJECT_ID)
}

/// The legacy loose log directory.
pub fn legacy_log_dir(log_dir: &Path) -> PathBuf {
    log_dir.join(LOOSE_PROJECT_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str, services: &[(&str, &str)]) -> Candidate {
        Candidate {
            config_path: PathBuf::from(path),
            project_id: loose_project_id(Path::new(path)),
            services: services.iter().map(|(name, _)| name.to_string()).collect(),
            service_hashes: services
                .iter()
                .map(|(name, hash)| (name.to_string(), hash.to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_uniquely_declared_service_is_attributed() {
        let candidates = vec![
            candidate("/units/ngrok-tunnel-8d7b.yaml", &[("ngrok-tunnel", "aaaa")]),
            candidate("/units/other.yaml", &[("other", "bbbb")]),
        ];
        let attribution = attribute_service("ngrok-tunnel", &candidates);
        assert_eq!(
            attribution.project_id(),
            Some(loose_project_id(Path::new("/units/ngrok-tunnel-8d7b.yaml")).as_str())
        );
    }

    #[test]
    fn a_service_declared_by_several_manifests_is_never_guessed() {
        // The three gamecast units all declare `gamecast-tunnel`; nothing in the
        // legacy row says which one owns it.
        let candidates = vec![
            candidate(
                "/units/gamecast-tunnel-3a2a.yaml",
                &[("gamecast-tunnel", "a")],
            ),
            candidate(
                "/units/gamecast-tunnel-5a9c.yaml",
                &[("gamecast-tunnel", "b")],
            ),
            candidate(
                "/units/gamecast-tunnel-6df6.yaml",
                &[("gamecast-tunnel", "c")],
            ),
        ];
        let attribution = attribute_service("gamecast-tunnel", &candidates);
        match attribution {
            Attribution::Quarantined(Unresolved::Ambiguous(paths)) => {
                assert_eq!(paths.len(), 3);
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn a_service_no_manifest_declares_is_quarantined() {
        let candidates = vec![candidate("/units/a.yaml", &[("a", "aaaa")])];
        assert_eq!(
            attribute_service("vanished", &candidates),
            Attribution::Quarantined(Unresolved::NoCandidate)
        );
    }

    #[test]
    fn a_cron_row_matches_by_hash_before_name() {
        // The row's name no longer matches any manifest, but its hash does —
        // this is the renamed-service case, and the hash must win.
        let candidates = vec![candidate(
            "/units/a.yaml",
            &[("renamed", "de98291cbf657443")],
        )];
        let attribution = attribute_cron_job("old-name", "de98291cbf657443", &candidates);
        assert!(matches!(attribution, Attribution::Resolved { .. }));
    }

    #[test]
    fn a_cron_row_matching_nothing_is_quarantined() {
        // Both of the user's real `test_service` rows land here: no current
        // manifest declares them and no hash matches.
        let candidates = vec![candidate("/units/a.yaml", &[("a", "aaaa")])];
        assert_eq!(
            attribute_cron_job("test_service", "de98291cbf657443", &candidates),
            Attribution::Quarantined(Unresolved::NoCandidate)
        );
    }

    #[test]
    fn log_names_strip_stream_suffixes() {
        assert_eq!(log_service_name("gamecast-tunnel.log"), "gamecast-tunnel");
        assert_eq!(log_service_name("demo_stdout.log"), "demo");
        assert_eq!(log_service_name("demo_stderr.log"), "demo");
    }

    #[test]
    fn a_log_for_an_ambiguous_service_is_quarantined() {
        let candidates = vec![
            candidate(
                "/units/gamecast-tunnel-3a2a.yaml",
                &[("gamecast-tunnel", "a")],
            ),
            candidate(
                "/units/gamecast-tunnel-5a9c.yaml",
                &[("gamecast-tunnel", "b")],
            ),
        ];
        assert!(matches!(
            attribute_log("gamecast-tunnel.log", &candidates),
            Attribution::Quarantined(Unresolved::Ambiguous(_))
        ));
    }

    #[test]
    fn a_plan_reports_every_quarantined_artifact() {
        let mut plan = MigrationPlan::default();
        plan.services.insert(
            "gamecast-tunnel".into(),
            Attribution::Quarantined(Unresolved::Ambiguous(vec!["a".into(), "b".into()])),
        );
        plan.cron_jobs.insert(
            "de98291cbf657443".into(),
            Attribution::Quarantined(Unresolved::NoCandidate),
        );
        plan.services.insert(
            "ngrok-tunnel".into(),
            Attribution::Resolved {
                config_path: PathBuf::from("/units/ngrok.yaml"),
                project_id: "ngrok-abcd".into(),
            },
        );

        let quarantined = plan.quarantined();
        assert_eq!(quarantined.len(), 2);
        assert_eq!(plan.target_projects().len(), 1);
        assert!(!plan.is_empty());
    }
}
