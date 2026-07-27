//! Crash-safe record of an in-flight loose-state migration.
//!
//! The migration moves state and logs that a user cannot reconstruct, so it
//! never mutates a source in place. Every source is archived and checksummed
//! first, outputs are staged and verified before being published, and the
//! registry is published last — so the supervisor only starts trusting the new
//! layout once the data behind it is already in place.
//!
//! The journal is what makes that recoverable. It is written before the first
//! byte moves and updated as each phase completes, so an interrupted run leaves
//! a record saying exactly how far it got. A journal that exists but is not
//! `Complete` means a migration is mid-flight: `sysg migrate-state` resumes it,
//! and the supervisor refuses to boot over it rather than reading a layout that
//! is half old and half new.
//!
//! It lives in its own directory beside the state root, not inside it, so that
//! `sysg purge` — which deletes project state — cannot destroy the record of a
//! migration still in progress.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::runtime;

/// Directory holding migration journals and archives.
pub const MIGRATIONS_DIR_NAME: &str = "systemg-migrations";

/// File name of the active journal.
pub const JOURNAL_FILE_NAME: &str = "journal.json";

/// Schema version of the journal format.
pub const JOURNAL_SCHEMA: u32 = 1;

/// How far a migration has progressed.
///
/// Ordering matters: a resume replays from the recorded phase, and every phase
/// is idempotent so replaying one that partly ran is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Plan recorded; nothing has been touched.
    Planned,
    /// Every source copied into the archive and checksum-verified.
    Archived,
    /// New-layout outputs written to staging.
    Staged,
    /// Staged outputs copied into their real locations.
    DataPublished,
    /// Loose registry written; the new layout is now authoritative.
    RegistryPublished,
    /// Migration finished and verified.
    Complete,
}

impl Phase {
    /// Whether the migration has finished.
    pub fn is_complete(self) -> bool {
        self == Self::Complete
    }
}

/// One archived source artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    /// Original path.
    pub path: String,
    /// Byte length at archive time.
    pub len: u64,
    /// SHA-256 of the archived bytes.
    pub sha256: String,
    /// Path within the archive.
    pub archive_path: String,
}

/// One artifact the migration produces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputRecord {
    /// Staging path the content was written to.
    pub stage_path: String,
    /// Final path it is published to.
    pub target_path: String,
    /// SHA-256 of the staged bytes.
    pub sha256: String,
    /// Project id this output belongs to.
    pub project_id: String,
}

/// An artifact that could not be attributed and was archived instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarantineRecord {
    /// What kind of artifact it was (`service`, `cron`, `log`).
    pub kind: String,
    /// Its name or key.
    pub name: String,
    /// Why it could not be placed.
    pub reason: String,
}

/// The durable record of one migration run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationJournal {
    /// Journal schema version.
    pub schema: u32,
    /// Identifier of this run, used to name its archive.
    pub id: String,
    /// How far the run has progressed.
    pub phase: Phase,
    /// State root the run operates on.
    pub state_dir: String,
    /// Log root the run operates on.
    pub log_dir: String,
    /// Manifest path to derived project id.
    pub path_to_id: BTreeMap<String, String>,
    /// Archived sources, keyed by original path.
    pub sources: BTreeMap<String, SourceRecord>,
    /// Outputs, keyed by target path.
    pub outputs: BTreeMap<String, OutputRecord>,
    /// Artifacts archived rather than migrated.
    pub quarantined: Vec<QuarantineRecord>,
}

impl MigrationJournal {
    /// Starts a new journal for a run.
    pub fn new(id: impl Into<String>, state_dir: &Path, log_dir: &Path) -> Self {
        Self {
            schema: JOURNAL_SCHEMA,
            id: id.into(),
            phase: Phase::Planned,
            state_dir: state_dir.to_string_lossy().to_string(),
            log_dir: log_dir.to_string_lossy().to_string(),
            path_to_id: BTreeMap::new(),
            sources: BTreeMap::new(),
            outputs: BTreeMap::new(),
            quarantined: Vec::new(),
        }
    }

    /// Records that the run reached `phase` and persists the journal.
    pub fn advance(&mut self, phase: Phase, path: &Path) -> io::Result<()> {
        self.phase = phase;
        self.save_to(path)
    }

    /// Loads a journal from an explicit path, if one exists.
    ///
    /// A journal that cannot be parsed is an error, never a silent absence: it
    /// records data movement the operator must be told about.
    pub fn load_from(path: &Path) -> io::Result<Option<Self>> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let journal: Self = serde_json::from_str(&raw)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if journal.schema != JOURNAL_SCHEMA {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "migration journal schema v{} is not supported (expected v{JOURNAL_SCHEMA})",
                    journal.schema
                ),
            ));
        }
        Ok(Some(journal))
    }

    /// Persists the journal atomically.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            runtime::create_private_dir(parent)?;
        }
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        bytes.push(b'\n');
        let temp = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| JOURNAL_FILE_NAME.to_string()),
            std::process::id()
        ));
        runtime::write_private_file(&temp, &bytes)?;
        match std::fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = std::fs::remove_file(&temp);
                Err(err)
            }
        }
    }
}

/// The migrations directory, a sibling of the state root so a purge cannot take
/// an in-flight migration's record with it.
pub fn migrations_dir(state_dir: &Path) -> PathBuf {
    state_dir
        .parent()
        .map(|parent| parent.join(MIGRATIONS_DIR_NAME))
        .unwrap_or_else(|| PathBuf::from(MIGRATIONS_DIR_NAME))
}

/// Path of the active journal.
pub fn journal_path(state_dir: &Path) -> PathBuf {
    migrations_dir(state_dir).join(JOURNAL_FILE_NAME)
}

/// A migration that started but has not completed, if any.
///
/// This is the boot gate: a `Some` here means the on-disk layout is part legacy
/// and part migrated, and nothing should read it until the run finishes.
pub fn pending_journal(state_dir: &Path) -> io::Result<Option<MigrationJournal>> {
    Ok(MigrationJournal::load_from(&journal_path(state_dir))?
        .filter(|journal| !journal.phase.is_complete()))
}

/// SHA-256 of a file's contents, as lowercase hex.
pub fn file_digest(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_order_from_planned_to_complete() {
        assert!(Phase::Planned < Phase::Archived);
        assert!(Phase::Archived < Phase::Staged);
        assert!(Phase::Staged < Phase::DataPublished);
        assert!(Phase::DataPublished < Phase::RegistryPublished);
        assert!(Phase::RegistryPublished < Phase::Complete);
        assert!(Phase::Complete.is_complete());
        assert!(!Phase::Staged.is_complete());
    }

    #[test]
    fn a_journal_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(JOURNAL_FILE_NAME);
        let mut journal =
            MigrationJournal::new("run-1", Path::new("/state"), Path::new("/logs"));
        journal
            .path_to_id
            .insert("/units/a.yaml".into(), "a-abcd".into());
        journal.save_to(&path).unwrap();

        let loaded = MigrationJournal::load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.id, "run-1");
        assert_eq!(loaded.phase, Phase::Planned);
        assert_eq!(loaded.path_to_id.get("/units/a.yaml").unwrap(), "a-abcd");
    }

    #[test]
    fn an_absent_journal_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            MigrationJournal::load_from(&dir.path().join("missing.json"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_malformed_journal_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(JOURNAL_FILE_NAME);
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(MigrationJournal::load_from(&path).is_err());
    }

    #[test]
    fn an_incomplete_journal_is_pending_and_a_complete_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let path = journal_path(&state_dir);

        let mut journal = MigrationJournal::new("run-1", &state_dir, Path::new("/logs"));
        journal.advance(Phase::Staged, &path).unwrap();
        assert!(pending_journal(&state_dir).unwrap().is_some());

        journal.advance(Phase::Complete, &path).unwrap();
        assert!(pending_journal(&state_dir).unwrap().is_none());
    }

    #[test]
    fn the_migrations_dir_sits_outside_the_state_root() {
        let migrations = migrations_dir(Path::new("/home/u/.local/share/systemg"));
        assert!(!migrations.starts_with("/home/u/.local/share/systemg"));
        assert!(migrations.ends_with(MIGRATIONS_DIR_NAME));
    }

    #[test]
    fn digests_distinguish_contents() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"alpha").unwrap();
        std::fs::write(&b, b"beta").unwrap();
        assert_ne!(file_digest(&a).unwrap(), file_digest(&b).unwrap());
        assert_eq!(file_digest(&a).unwrap(), file_digest(&a).unwrap());
    }
}
