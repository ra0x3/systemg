use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::runtime;

const MAX_FILES: usize = 200;
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Directory holding the unit manifests `sysg start -- <cmd>` stages.
pub fn dir() -> PathBuf {
    runtime::state_dir().join("units")
}

/// Prunes staged unit manifests older than 30 days or beyond the newest 200
/// unprotected ones, never touching a path in `keep`.
///
/// `keep` must name every manifest a loaded or restorable project still reads:
/// a restart re-opens its manifest from disk, so deleting one strands the
/// project behind SG0301.
pub fn prune(units_dir: &Path, keep: &HashSet<PathBuf>) -> io::Result<()> {
    prune_with_limits(units_dir, SystemTime::now(), MAX_FILES, MAX_AGE, keep)
}

/// Prunes with explicit limits.
pub fn prune_with_limits(
    units_dir: &Path,
    now: SystemTime,
    max_files: usize,
    max_age: Duration,
    keep: &HashSet<PathBuf>,
) -> io::Result<()> {
    let keep: HashSet<PathBuf> = keep.iter().map(|path| canonical(path)).collect();
    let mut entries = Vec::new();
    for entry in fs::read_dir(units_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yaml") {
            continue;
        }
        if keep.contains(&canonical(&path)) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        entries.push((path, modified));
    }

    let mut fresh_entries = Vec::new();
    for (path, modified) in entries {
        let is_stale = now
            .duration_since(modified)
            .map(|age| age > max_age)
            .unwrap_or(false);
        if is_stale {
            let _ = fs::remove_file(&path);
        } else {
            fresh_entries.push((path, modified));
        }
    }

    fresh_entries.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (path, _) in fresh_entries.into_iter().skip(max_files) {
        let _ = fs::remove_file(path);
    }

    Ok(())
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_unit(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "version: \"2\"\nservices: {}\n").expect("write unit file");
        path
    }

    fn yaml_count(dir: &Path) -> usize {
        fs::read_dir(dir)
            .expect("read units dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|ext| ext.to_str()) == Some("yaml")
            })
            .count()
    }

    #[test]
    fn prune_respects_max_files() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let now = SystemTime::now();
        for idx in 0..3 {
            write_unit(temp.path(), &format!("unit-{idx}.yaml"));
            std::thread::sleep(Duration::from_millis(10));
        }

        prune_with_limits(
            temp.path(),
            now + Duration::from_secs(1),
            2,
            Duration::from_secs(60),
            &HashSet::new(),
        )
        .expect("prune configs");

        assert_eq!(yaml_count(temp.path()), 2);
    }

    #[test]
    fn prune_respects_max_age() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let path = write_unit(temp.path(), "old.yaml");

        prune_with_limits(
            temp.path(),
            SystemTime::now() + Duration::from_secs(5),
            10,
            Duration::from_secs(1),
            &HashSet::new(),
        )
        .expect("prune configs");

        assert!(!path.exists(), "file older than max age should be pruned");
    }

    #[test]
    fn prune_keeps_loaded_manifests_past_max_age() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let loaded = write_unit(temp.path(), "loaded.yaml");
        let orphan = write_unit(temp.path(), "orphan.yaml");

        prune_with_limits(
            temp.path(),
            SystemTime::now() + Duration::from_secs(5),
            10,
            Duration::from_secs(1),
            &HashSet::from([loaded.clone()]),
        )
        .expect("prune configs");

        assert!(loaded.exists(), "a loaded manifest must survive pruning");
        assert!(!orphan.exists(), "an unreferenced stale manifest is pruned");
    }

    #[test]
    fn prune_keeps_loaded_manifests_past_max_files() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let loaded = write_unit(temp.path(), "loaded.yaml");
        std::thread::sleep(Duration::from_millis(10));
        for idx in 0..3 {
            write_unit(temp.path(), &format!("unit-{idx}.yaml"));
            std::thread::sleep(Duration::from_millis(10));
        }

        prune_with_limits(
            temp.path(),
            SystemTime::now(),
            2,
            Duration::from_secs(60),
            &HashSet::from([loaded.clone()]),
        )
        .expect("prune configs");

        assert!(
            loaded.exists(),
            "the oldest file survives because it is loaded"
        );
        assert_eq!(yaml_count(temp.path()), 3);
    }
}
