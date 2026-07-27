//! Durable record of the loose manifests a supervisor is managing.
//!
//! A loose manifest declares no project of its own, so its project id is derived
//! from its path. `config_hint` holds one path and cannot describe a set, which
//! left every loose config but the last unrecoverable across a cold boot. This
//! registry is that missing set: boot re-registers each entry, so N loose
//! manifests come back as the N projects they were.
//!
//! Writes go through a temp file and a rename so a reader never observes a
//! half-written registry, and a crash mid-write leaves the previous one intact.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{runtime, status::ProjectRunMode};

/// File name of the registry within the state root.
pub const REGISTRY_FILE_NAME: &str = "loose_registry.json";

/// Schema version, so a future layout change can be detected rather than
/// silently misread as the current shape.
pub const REGISTRY_VERSION: u32 = 1;

/// One loose manifest the supervisor is managing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LooseEntry {
    /// Canonical absolute path of the manifest.
    pub config_path: String,
    /// Project id derived from that path.
    pub project_id: String,
    /// How the project was started.
    #[serde(default)]
    pub mode: ProjectRunMode,
}

/// The set of loose manifests, keyed by canonical manifest path.
///
/// Keying by path rather than project id keeps the registry faithful to what it
/// records: the path is the input identity is derived from, so one file can
/// never occupy two slots, and a stale id from an older derivation cannot shadow
/// the manifest it came from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LooseRegistry {
    /// Schema version of this file.
    #[serde(default)]
    pub version: u32,
    /// Entries by canonical manifest path.
    #[serde(default)]
    pub entries: BTreeMap<String, LooseEntry>,
}

/// Why a registry could not be read.
#[derive(Debug)]
pub enum RegistryError {
    /// The file exists but is not valid registry JSON.
    Malformed(String),
    /// The file was written by a newer schema than this binary understands.
    UnsupportedVersion(u32),
    /// The file could not be read or written.
    Io(io::Error),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(detail) => write!(f, "loose registry is malformed: {detail}"),
            Self::UnsupportedVersion(version) => write!(
                f,
                "loose registry schema v{version} is newer than supported v{REGISTRY_VERSION}"
            ),
            Self::Io(err) => write!(f, "loose registry io error: {err}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Path of the registry file within the state root.
pub fn registry_path() -> PathBuf {
    runtime::state_dir().join(REGISTRY_FILE_NAME)
}

/// A writer-private temp path beside `path`, so concurrent saves never share one
/// staging file.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| REGISTRY_FILE_NAME.to_string());
    let temp_name = format!(".{name}.{}.tmp", std::process::id());
    path.with_file_name(temp_name)
}

impl LooseRegistry {
    /// Loads the registry, returning an empty one when absent.
    ///
    /// An absent file is normal — no loose manifest has been registered yet. A
    /// file that exists but cannot be understood is an error rather than an
    /// empty registry: treating it as empty would let the next save overwrite
    /// records the supervisor still needs to restore its projects.
    pub fn load() -> Result<Self, RegistryError> {
        Self::load_from(&registry_path())
    }

    /// Loads the registry from an explicit path.
    pub fn load_from(path: &Path) -> Result<Self, RegistryError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(err) => return Err(RegistryError::Io(err)),
        };
        let registry: Self = serde_json::from_str(&raw)
            .map_err(|err| RegistryError::Malformed(err.to_string()))?;
        if registry.version != REGISTRY_VERSION {
            return Err(RegistryError::UnsupportedVersion(registry.version));
        }
        Ok(registry)
    }

    /// An empty registry at the current schema version.
    pub fn empty() -> Self {
        Self {
            version: REGISTRY_VERSION,
            entries: BTreeMap::new(),
        }
    }

    /// Records a loose manifest, replacing any entry for the same path.
    pub fn insert(&mut self, entry: LooseEntry) {
        self.entries.insert(entry.config_path.clone(), entry);
    }

    /// Drops the entry for `config_path`, reporting whether one was present.
    pub fn remove(&mut self, config_path: &str) -> bool {
        self.entries.remove(config_path).is_some()
    }

    /// Drops every entry whose derived project id is `project_id`, reporting
    /// whether any was present.
    pub fn remove_project(&mut self, project_id: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| entry.project_id != project_id);
        self.entries.len() != before
    }

    /// The entry whose derived project id is `project_id`, if any.
    pub fn by_project(&self, project_id: &str) -> Option<&LooseEntry> {
        self.entries
            .values()
            .find(|entry| entry.project_id == project_id)
    }

    /// Every recorded entry, ordered by project id.
    pub fn entries(&self) -> impl Iterator<Item = &LooseEntry> {
        self.entries.values()
    }

    /// Persists the registry to the state root.
    pub fn save(&self) -> io::Result<()> {
        self.save_to(&registry_path())
    }

    /// Persists the registry to an explicit path, atomically.
    ///
    /// The temp file sits beside the target so the rename stays within one
    /// filesystem, where it is atomic; a cross-device temp would degrade to a
    /// copy and reintroduce the torn read this exists to prevent. Its name
    /// carries the writer's pid so two concurrent writers cannot land on one
    /// temp and rename each other's half-written bytes into place.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            runtime::create_private_dir(parent)?;
        }
        let mut serialized = serde_json::to_vec_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        serialized.push(b'\n');

        let temp = temp_path(path);
        runtime::write_private_file(&temp, &serialized)?;
        match std::fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = std::fs::remove_file(&temp);
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, path: &str) -> LooseEntry {
        LooseEntry {
            config_path: path.to_string(),
            project_id: id.to_string(),
            mode: ProjectRunMode::Daemon,
        }
    }

    #[test]
    fn absent_registry_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            LooseRegistry::load_from(&dir.path().join("missing.json")).unwrap();
        assert_eq!(registry.entries().count(), 0);
        assert_eq!(registry.version, REGISTRY_VERSION);
    }

    #[test]
    fn entries_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(REGISTRY_FILE_NAME);
        let mut registry = LooseRegistry::empty();
        registry.insert(entry("ngrok-tunnel-8d7b", "/units/ngrok-tunnel-8d7b.yaml"));
        registry.insert(entry(
            "gamecast-tunnel-3a2a",
            "/units/gamecast-tunnel-3a2a.yaml",
        ));
        registry.save_to(&path).unwrap();

        let loaded = LooseRegistry::load_from(&path).unwrap();
        let ids: Vec<&str> = loaded.entries().map(|e| e.project_id.as_str()).collect();
        assert_eq!(ids, vec!["gamecast-tunnel-3a2a", "ngrok-tunnel-8d7b"]);
    }

    #[test]
    fn many_loose_manifests_coexist() {
        let mut registry = LooseRegistry::empty();
        for i in 0..8 {
            registry.insert(entry(
                &format!("unit-{i}"),
                &format!("/units/unit-{i}.yaml"),
            ));
        }
        assert_eq!(registry.entries().count(), 8);
    }

    #[test]
    fn sibling_units_sharing_a_stem_each_keep_a_slot() {
        // The three gamecast-tunnel units are the case that used to evict each
        // other; they must occupy three registry slots, not one.
        let mut registry = LooseRegistry::empty();
        for hash in ["3a2a1f8c6425", "5a9ce32857ea", "6df683933138"] {
            registry.insert(entry(
                &format!("gamecast-tunnel-{hash}-deadbeefdeadbeef"),
                &format!("/units/gamecast-tunnel-{hash}.yaml"),
            ));
        }
        assert_eq!(registry.entries().count(), 3);
    }

    #[test]
    fn reinserting_the_same_path_replaces_rather_than_duplicates() {
        let mut registry = LooseRegistry::empty();
        registry.insert(entry("unit", "/units/unit.yaml"));
        registry.insert(entry("unit", "/units/unit.yaml"));
        assert_eq!(registry.entries().count(), 1);
    }

    #[test]
    fn two_paths_sharing_a_filename_do_not_share_a_slot() {
        let mut registry = LooseRegistry::empty();
        registry.insert(entry("svc-aaaa", "/a/svc.yaml"));
        registry.insert(entry("svc-bbbb", "/b/svc.yaml"));
        assert_eq!(registry.entries().count(), 2);
    }

    #[test]
    fn remove_reports_whether_an_entry_was_present() {
        let mut registry = LooseRegistry::empty();
        registry.insert(entry("unit", "/units/unit.yaml"));
        assert!(registry.remove("/units/unit.yaml"));
        assert!(!registry.remove("/units/unit.yaml"));
    }

    #[test]
    fn entries_are_addressable_by_derived_project_id() {
        let mut registry = LooseRegistry::empty();
        registry.insert(entry("unit-abcd", "/units/unit.yaml"));
        assert_eq!(
            registry
                .by_project("unit-abcd")
                .map(|e| e.config_path.as_str()),
            Some("/units/unit.yaml")
        );
        assert!(registry.by_project("missing").is_none());
        assert!(registry.remove_project("unit-abcd"));
        assert!(!registry.remove_project("unit-abcd"));
    }

    #[test]
    fn a_malformed_registry_is_an_error_not_an_empty_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(REGISTRY_FILE_NAME);
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(matches!(
            LooseRegistry::load_from(&path),
            Err(RegistryError::Malformed(_))
        ));
    }

    #[test]
    fn a_future_version_is_an_error_rather_than_being_misparsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(REGISTRY_FILE_NAME);
        std::fs::write(&path, br#"{"version":999,"entries":{}}"#).unwrap();
        assert!(matches!(
            LooseRegistry::load_from(&path),
            Err(RegistryError::UnsupportedVersion(999))
        ));
    }

    #[test]
    fn saving_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(REGISTRY_FILE_NAME);
        LooseRegistry::empty().save_to(&path).unwrap();
        assert!(path.exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left temp files: {leftovers:?}");
    }
}
