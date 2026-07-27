//! Offline migration of legacy `__loose__` state into per-manifest projects.
//!
//! Loose services used to share one `__loose__` project directory. They now each
//! key under a project id derived from their manifest, so the state left behind
//! by the old layout has to be partitioned onto those ids before the supervisor
//! can read it.
//!
//! This runs as an explicit `sysg migrate-state`, never at boot: it moves files
//! the user cannot reconstruct, and boot happens to load state *before* taking
//! `supervisor.lock`, so a second invocation could observe a half-migrated tree.
//! Requiring the operator to run it means it happens once, with no supervisor
//! alive, and with its output in front of them.
//!
//! The `journal` submodule holds the crash-safety model; `plan` holds how legacy
//! artifacts are attributed to the manifests that own them.

pub mod journal;
pub mod plan;

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

pub use journal::{MigrationJournal, Phase, pending_journal};
pub use plan::{Attribution, Candidate, MigrationPlan, Unresolved};

use crate::{
    config::load_config,
    loose_registry::{LooseEntry, LooseRegistry},
    state_store::LOOSE_PROJECT_ID,
    status::ProjectRunMode,
};

/// What a migration would do, or did.
#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    /// Services placed under a derived project id.
    pub migrated_services: BTreeMap<String, String>,
    /// Log files placed under a derived project id.
    pub migrated_logs: BTreeMap<String, String>,
    /// Artifacts archived because they could not be attributed.
    pub quarantined: Vec<(String, String, String)>,
    /// Registry entries the migration will publish.
    pub registry_entries: Vec<LooseEntry>,
    /// Where archived copies of every source were written.
    pub archive_dir: Option<PathBuf>,
}

impl MigrationReport {
    /// Whether the migration has anything to do.
    pub fn is_empty(&self) -> bool {
        self.migrated_services.is_empty()
            && self.migrated_logs.is_empty()
            && self.quarantined.is_empty()
    }
}

/// Scans `units_dir` for manifests that could own legacy loose state.
///
/// Only project-less manifests are candidates — one that names its own project
/// never had state under `__loose__`. Unreadable or unparseable files are
/// skipped rather than failing the scan: a broken manifest sitting in the units
/// directory should not block migrating everything else.
pub fn scan_candidates(units_dir: &Path) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(units_dir) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_manifest = path
            .extension()
            .is_some_and(|ext| ext == "yaml" || ext == "yml");
        if !is_manifest {
            continue;
        }
        let canonical = crate::config::canonical_manifest_path(&path);
        let Ok(config) = load_config(Some(&canonical.to_string_lossy())) else {
            continue;
        };
        if let Some(candidate) = Candidate::from_config(&config, &canonical) {
            candidates.push(candidate);
        }
    }
    candidates.sort_by(|a, b| a.config_path.cmp(&b.config_path));
    candidates
}

/// Builds a plan for the legacy state under `state_dir` / `log_dir`.
pub fn plan_migration(
    state_dir: &Path,
    log_dir: &Path,
    candidates: &[Candidate],
) -> io::Result<MigrationPlan> {
    let mut migration = MigrationPlan::default();

    let legacy_dir = plan::legacy_project_dir(state_dir);
    for name in legacy_service_names(&legacy_dir)? {
        let attribution = plan::attribute_service(&name, candidates);
        migration.services.insert(name, attribution);
    }
    for (hash, service_name) in legacy_cron_jobs(&legacy_dir)? {
        let attribution = plan::attribute_cron_job(&service_name, &hash, candidates);
        migration.cron_jobs.insert(hash, attribution);
    }
    for file_name in legacy_log_files(&plan::legacy_log_dir(log_dir))? {
        let attribution = plan::attribute_log(&file_name, candidates);
        migration.logs.insert(file_name, attribution);
    }

    Ok(migration)
}

/// Turns a plan into the report `sysg migrate-state` prints, whether or not it
/// is then executed.
pub fn describe(migration: &MigrationPlan, candidates: &[Candidate]) -> MigrationReport {
    let mut report = MigrationReport::default();

    for (name, attribution) in &migration.services {
        if let Some(project) = attribution.project_id() {
            report
                .migrated_services
                .insert(name.clone(), project.to_string());
        }
    }
    for (name, attribution) in &migration.logs {
        if let Some(project) = attribution.project_id() {
            report
                .migrated_logs
                .insert(name.clone(), project.to_string());
        }
    }
    for (kind, name, reason) in migration.quarantined() {
        let detail = match reason {
            Unresolved::NoCandidate => "no manifest declares it".to_string(),
            Unresolved::Ambiguous(paths) => {
                format!(
                    "declared by {} manifests: {}",
                    paths.len(),
                    paths.join(", ")
                )
            }
        };
        report
            .quarantined
            .push((kind.to_string(), name.to_string(), detail));
    }

    let targets = migration.target_projects();
    report.registry_entries = candidates
        .iter()
        .filter(|candidate| targets.contains(candidate.project_id.as_str()))
        .map(|candidate| LooseEntry {
            config_path: candidate.config_path.to_string_lossy().to_string(),
            project_id: candidate.project_id.clone(),
            mode: ProjectRunMode::Daemon,
        })
        .collect();

    report
}

/// The registry the migration publishes, merged onto whatever is already there.
pub fn registry_with(entries: &[LooseEntry]) -> LooseRegistry {
    let mut registry = LooseRegistry::load().unwrap_or_else(|_| LooseRegistry::empty());
    for entry in entries {
        registry.insert(entry.clone());
    }
    registry
}

/// Writes a seeded state file atomically, beside its target.
///
/// A plain write torn by a crash would leave a corrupt file that every retry
/// then skips as "existing state" — the rename makes the file appear whole or
/// not at all.
fn write_seeded_file(target: &Path, contents: String) -> io::Result<()> {
    let temp = target.with_file_name(format!(
        ".{}.{}.tmp",
        target
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "seed".to_string()),
        std::process::id()
    ));
    crate::runtime::write_private_file(&temp, contents)?;
    match std::fs::rename(&temp, target) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            Err(err)
        }
    }
}

/// Seeds a derived project's state files from the legacy directory a pre-0.59
/// resident wrote, for a live-upgrade resume.
///
/// The replacement supervisor verifies handed-off pids against the project's
/// own store — it never writes them — so a project whose identity migrated
/// during the upgrade needs its `__loose__`-era pid/state/cron entries placed
/// under the new id before its daemon is built, or the resume fails on an empty
/// store and rolls the upgrade back.
///
/// Only entries for services the config declares are carried over, state keys
/// are rewritten onto the derived id, existing target files are left alone
/// (they are newer by definition), and the legacy directory is not touched —
/// `migrate-state` remains the tool that retires it.
pub fn seed_project_state_from_legacy(
    state_dir: &Path,
    legacy_id: &str,
    config: &crate::config::Config,
) -> io::Result<()> {
    let legacy_dir = state_dir
        .join(crate::state_store::PROJECTS_DIR)
        .join(legacy_id);
    let target_dir = state_dir
        .join(crate::state_store::PROJECTS_DIR)
        .join(&config.project.id);
    if !legacy_dir.exists() {
        return Ok(());
    }
    crate::runtime::create_private_dir(&target_dir)?;

    let services: Vec<&str> = config.services.keys().map(String::as_str).collect();

    let pid_target = target_dir.join(crate::constants::PID_FILE_NAME);
    if !pid_target.exists()
        && let Ok(raw) =
            std::fs::read_to_string(legacy_dir.join(crate::constants::PID_FILE_NAME))
    {
        let blocks: Vec<String> = ["services", "service_groups", "service_starts"]
            .iter()
            .flat_map(|tag| xml_blocks(&raw, tag))
            .filter(|block| {
                xml_field_values(block, "name")
                    .iter()
                    .any(|name| services.contains(&name.as_str()))
            })
            .collect();
        if !blocks.is_empty() {
            write_seeded_file(
                &pid_target,
                format!("<PidFile>\n{}</PidFile>\n", blocks.join("")),
            )?;
        }
    }

    let state_target = target_dir.join(crate::constants::STATE_FILE_NAME);
    if !state_target.exists()
        && let Ok(raw) =
            std::fs::read_to_string(legacy_dir.join(crate::constants::STATE_FILE_NAME))
    {
        let blocks: Vec<String> = services
            .iter()
            .filter_map(|service| {
                state_block_for(&raw, service)
                    .map(|block| rekey_state_block(&block, &config.state_key(service)))
            })
            .collect();
        if !blocks.is_empty() {
            write_seeded_file(
                &state_target,
                format!(
                    "<ServiceStateFile>\n{}</ServiceStateFile>\n",
                    blocks.join("")
                ),
            )?;
        }
    }

    let cron_target = target_dir.join(crate::state_store::CRON_FILE_NAME);
    if !cron_target.exists()
        && let Ok(raw) =
            std::fs::read_to_string(legacy_dir.join(crate::state_store::CRON_FILE_NAME))
    {
        let hashes: Vec<String> = config.service_hashes().into_values().collect();
        let blocks: Vec<String> = xml_blocks(&raw, "jobs")
            .into_iter()
            .filter(|block| {
                xml_field_values(block, "hash")
                    .iter()
                    .any(|hash| hashes.contains(hash))
            })
            .collect();
        if !blocks.is_empty() {
            write_seeded_file(
                &cron_target,
                format!("<CronStateFile>\n{}</CronStateFile>\n", blocks.join("")),
            )?;
        }
    }

    Ok(())
}

/// One artifact the migration writes into a derived project's directory.
#[derive(Debug, Clone)]
pub struct PublishItem {
    /// Where the content is written.
    pub target: PathBuf,
    /// The content itself.
    pub contents: Vec<u8>,
    /// The project the artifact belongs to.
    pub project_id: String,
}

/// Builds the per-project state the migration publishes.
///
/// Only attributed artifacts appear. A quarantined one has no project to be
/// written into — that is what quarantine means — so it stays in the archive
/// and the legacy tree, and nothing is invented for it.
pub fn publish_items(
    migration: &MigrationPlan,
    state_dir: &Path,
    log_dir: &Path,
) -> io::Result<Vec<PublishItem>> {
    let legacy_dir = plan::legacy_project_dir(state_dir);
    let mut items = Vec::new();

    let pid_raw =
        std::fs::read_to_string(legacy_dir.join(crate::constants::PID_FILE_NAME))
            .unwrap_or_default();
    let state_raw =
        std::fs::read_to_string(legacy_dir.join(crate::constants::STATE_FILE_NAME))
            .unwrap_or_default();

    let mut by_project: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (service, attribution) in &migration.services {
        if let Some(project) = attribution.project_id() {
            by_project.entry(project).or_default().push(service);
        }
    }

    for (project, services) in by_project {
        let project_dir = state_dir
            .join(crate::state_store::PROJECTS_DIR)
            .join(project);

        let pid_entries: Vec<String> = services
            .iter()
            .filter_map(|service| pid_block_for(&pid_raw, service))
            .collect();
        if !pid_entries.is_empty() {
            items.push(PublishItem {
                target: project_dir.join(crate::constants::PID_FILE_NAME),
                contents: format!("<PidFile>\n{}</PidFile>\n", pid_entries.join(""))
                    .into_bytes(),
                project_id: project.to_string(),
            });
        }

        // State rows are re-keyed onto the derived project: a legacy row reads
        // `v2:none:<service>`, and nothing would ever look it up under that key
        // again once the service belongs to a real project.
        let state_entries: Vec<String> = services
            .iter()
            .filter_map(|service| {
                state_block_for(&state_raw, service).map(|block| {
                    let new_key = crate::config::state_key(
                        crate::config::Version::V2,
                        project,
                        service,
                    );
                    rekey_state_block(&block, &new_key)
                })
            })
            .collect();
        if !state_entries.is_empty() {
            items.push(PublishItem {
                target: project_dir.join(crate::constants::STATE_FILE_NAME),
                contents: format!(
                    "<ServiceStateFile>\n{}</ServiceStateFile>\n",
                    state_entries.join("")
                )
                .into_bytes(),
                project_id: project.to_string(),
            });
        }
    }

    // Cron rows are keyed by config hash, which is independent of the project,
    // so an attributed row moves across unchanged apart from the file it lands
    // in. Rows that could not be attributed stay behind with the legacy tree.
    let cron_raw =
        std::fs::read_to_string(legacy_dir.join(crate::state_store::CRON_FILE_NAME))
            .unwrap_or_default();
    let mut cron_by_project: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (hash, attribution) in &migration.cron_jobs {
        let Some(project) = attribution.project_id() else {
            continue;
        };
        if let Some(block) = cron_block_for(&cron_raw, hash) {
            cron_by_project.entry(project).or_default().push(block);
        }
    }
    for (project, blocks) in cron_by_project {
        items.push(PublishItem {
            target: state_dir
                .join(crate::state_store::PROJECTS_DIR)
                .join(project)
                .join(crate::state_store::CRON_FILE_NAME),
            contents: format!("<CronStateFile>\n{}</CronStateFile>\n", blocks.join(""))
                .into_bytes(),
            project_id: project.to_string(),
        });
    }

    let legacy_logs = plan::legacy_log_dir(log_dir);
    for (file_name, attribution) in &migration.logs {
        let Some(project) = attribution.project_id() else {
            continue;
        };
        let source = legacy_logs.join(file_name);
        let Ok(contents) = std::fs::read(&source) else {
            continue;
        };
        items.push(PublishItem {
            target: log_dir.join(project).join(file_name),
            contents,
            project_id: project.to_string(),
        });
    }

    Ok(items)
}

/// The `<jobs>` block whose hash is `hash` in a legacy cron file.
fn cron_block_for(raw: &str, hash: &str) -> Option<String> {
    xml_blocks(raw, "jobs")
        .into_iter()
        .find(|block| xml_field_values(block, "hash").iter().any(|h| h == hash))
}

/// The `<services>` block naming `service` in a legacy pid file.
fn pid_block_for(raw: &str, service: &str) -> Option<String> {
    xml_blocks(raw, "services")
        .into_iter()
        .find(|block| xml_field_values(block, "name").iter().any(|n| n == service))
}

/// The `<services>` block whose state key names `service`.
fn state_block_for(raw: &str, service: &str) -> Option<String> {
    xml_blocks(raw, "services").into_iter().find(|block| {
        xml_field_values(block, "name")
            .iter()
            .any(|key| service_from_state_key(key) == service)
    })
}

/// Replaces a state block's `<name>` with `new_key`.
fn rekey_state_block(block: &str, new_key: &str) -> String {
    let Some(start) = block.find("<name>") else {
        return block.to_string();
    };
    let Some(end) = block[start..].find("</name>").map(|idx| start + idx) else {
        return block.to_string();
    };
    let escaped = xml_escape(new_key);
    format!("{}<name>{escaped}{}", &block[..start], &block[end..])
}

/// Every `<tag>…</tag>` block, including its delimiters.
fn xml_blocks(raw: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut blocks = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start..];
        let Some(end) = after.find(&close) else {
            break;
        };
        blocks.push(format!("  {}\n", after[..end + close.len()].trim()));
        rest = &after[end + close.len()..];
    }
    blocks
}

/// Service names appearing in the legacy pid and state files.
fn legacy_service_names(legacy_dir: &Path) -> io::Result<Vec<String>> {
    use std::collections::BTreeSet;
    let mut names: BTreeSet<String> = BTreeSet::new();

    let pid_path = legacy_dir.join(crate::constants::PID_FILE_NAME);
    if let Ok(raw) = std::fs::read_to_string(&pid_path) {
        names.extend(xml_field_values(&raw, "name"));
    }

    let state_path = legacy_dir.join(crate::constants::STATE_FILE_NAME);
    if let Ok(raw) = std::fs::read_to_string(&state_path) {
        for key in xml_field_values(&raw, "name") {
            names.insert(service_from_state_key(&key));
        }
    }

    Ok(names.into_iter().collect())
}

/// Legacy cron jobs as `(hash, service_name)`.
fn legacy_cron_jobs(legacy_dir: &Path) -> io::Result<Vec<(String, String)>> {
    let cron_path = legacy_dir.join(crate::state_store::CRON_FILE_NAME);
    let Ok(raw) = std::fs::read_to_string(&cron_path) else {
        return Ok(Vec::new());
    };
    let hashes = xml_field_values(&raw, "hash");
    let services = xml_field_values(&raw, "service_name");
    Ok(hashes.into_iter().zip(services).collect())
}

/// Log file names under the legacy log directory.
fn legacy_log_files(legacy_log_dir: &Path) -> io::Result<Vec<String>> {
    let Ok(entries) = std::fs::read_dir(legacy_log_dir) else {
        return Ok(Vec::new());
    };
    let mut files: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    files.sort();
    Ok(files)
}

/// The service portion of a `{version}:{project}:{service}` state key.
///
/// Legacy loose keys look like `v2:none:gamecast-tunnel`; a key written before
/// the scheme existed is treated as a bare service name.
fn service_from_state_key(key: &str) -> String {
    key.splitn(3, ':').nth(2).unwrap_or(key).to_string()
}

/// Values of every `<field>…</field>` in a state document, entity-decoded.
///
/// The state files are written by this crate's own XML serializer, so a
/// targeted scan is enough and avoids constructing the typed state handles —
/// which would take the project lock and rewrite the very files being migrated.
/// The serializer escapes text content, so a service literally named
/// `api&worker` is stored as `api&amp;worker`; values are decoded back before
/// being compared with raw names from a parsed config.
fn xml_field_values(raw: &str, field: &str) -> Vec<String> {
    let open = format!("<{field}>");
    let close = format!("</{field}>");
    let mut values = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        values.push(xml_unescape(after[..end].trim()));
        rest = &after[end + close.len()..];
    }
    values
}

/// Decodes the five XML entities the state serializer produces.
fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Encodes text for placement inside an XML text node, matching the state
/// serializer's escaping so seeded files parse exactly like written ones.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Builds the SG0602 diagnostic for a migration refused because a supervisor is
/// live.
pub fn supervisor_active() -> crate::diag::Diagnostic {
    use crate::diag::{Diagnostic, SgCode};
    Diagnostic::error(
        SgCode::MigrationSupervisorActive,
        "a supervisor is running; state cannot be migrated underneath it",
    )
    .note("the migration moves the files a live supervisor is reading and writing")
    .help_cmd("stop it first", "sysg stop --supervisor")
    .help_cmd("then migrate", "sysg migrate-state")
    .help_docs()
}

/// Builds the SG0604 diagnostic for a migration that did not finish.
pub fn incomplete(phase: Phase) -> crate::diag::Diagnostic {
    use crate::diag::{Diagnostic, SgCode};
    Diagnostic::error(
        SgCode::MigrationIncomplete,
        "a previous state migration did not finish",
    )
    .note(format!("it stopped after the {phase:?} phase"))
    .note("the layout is part legacy and part migrated until it is resumed")
    .help_cmd("resume it", "sysg migrate-state")
    .help_docs()
}

/// Builds the SG0601 diagnostic for legacy state that still needs migrating.
pub fn required() -> crate::diag::Diagnostic {
    use crate::diag::{Diagnostic, SgCode};
    Diagnostic::error(
        SgCode::MigrationRequired,
        "legacy `__loose__` state is present and has not been migrated",
    )
    .note(
        "project-less manifests each own a project derived from their path; the \
         state under `__loose__` predates that and must be placed first",
    )
    .help_cmd("see what would move", "sysg migrate-state --dry-run")
    .help_cmd("migrate it", "sysg migrate-state")
    .help_docs()
}

/// Whether the legacy loose project id is the one given.
pub fn is_legacy_loose(project_id: &str) -> bool {
    project_id == LOOSE_PROJECT_ID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_coding_is_order_safe_and_round_trips() {
        assert_eq!(xml_unescape("api&amp;worker"), "api&worker");
        // A literal `&lt;` arrives as `&amp;lt;` and must decode to the four
        // characters, not to `<`.
        assert_eq!(xml_unescape("&amp;lt;"), "&lt;");
        assert_eq!(xml_unescape("&amp;amp;"), "&amp;");
        assert_eq!(xml_escape("a&b<c"), "a&amp;b&lt;c");
        assert_eq!(xml_unescape(&xml_escape("&lt;&amp;")), "&lt;&amp;");
    }

    #[test]
    fn seeding_matches_names_the_serializer_escaped() {
        // The state serializer writes `api&worker` as `api&amp;worker`;
        // comparisons run against decoded values or such services silently
        // fail to seed and the resume rolls back.
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();
        let legacy = state_dir
            .join(crate::state_store::PROJECTS_DIR)
            .join(crate::state_store::LOOSE_PROJECT_ID);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join(crate::constants::PID_FILE_NAME),
            "<PidFile>\n  <services>\n    <name>api&amp;worker</name>\n    \
             <pid>4242</pid>\n  </services>\n</PidFile>\n",
        )
        .unwrap();

        let manifest = dir.path().join("amp-1111.yaml");
        std::fs::write(
            &manifest,
            "version: \"2\"\nservices:\n  \"api&worker\":\n    command: 'echo hi'\n",
        )
        .unwrap();
        let config = load_config(Some(&manifest.to_string_lossy())).unwrap();

        seed_project_state_from_legacy(
            state_dir,
            crate::state_store::LOOSE_PROJECT_ID,
            &config,
        )
        .unwrap();

        let target = state_dir
            .join(crate::state_store::PROJECTS_DIR)
            .join(&config.project.id)
            .join(crate::constants::PID_FILE_NAME);
        let seeded = std::fs::read_to_string(target).expect("escaped name must seed");
        assert!(seeded.contains("<pid>4242</pid>"));
        // Carried verbatim: still escaped on disk, as the serializer writes it.
        assert!(seeded.contains("api&amp;worker"));
    }

    #[test]
    fn rekeying_escapes_what_it_writes() {
        let block = "  <services>\n    <name>v2:none:api&amp;worker</name>\n    \
                     <state>\n      <status>running</status>\n    </state>\n  \
                     </services>\n";
        let rekeyed = rekey_state_block(block, "v2:proj-abcd:api&worker");
        assert!(rekeyed.contains("<name>v2:proj-abcd:api&amp;worker</name>"));
        assert_eq!(
            xml_field_values(&rekeyed, "name"),
            vec!["v2:proj-abcd:api&worker"]
        );
    }

    #[test]
    fn seeding_carries_only_declared_services_and_rekeys_their_state() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();
        let legacy = state_dir
            .join(crate::state_store::PROJECTS_DIR)
            .join(crate::state_store::LOOSE_PROJECT_ID);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join(crate::constants::PID_FILE_NAME),
            "<PidFile>\n  <services>\n    <name>mine</name>\n    <pid>4242</pid>\n  \
             </services>\n  <services>\n    <name>other</name>\n    <pid>9999</pid>\n  \
             </services>\n</PidFile>\n",
        )
        .unwrap();
        std::fs::write(
            legacy.join(crate::constants::STATE_FILE_NAME),
            "<ServiceStateFile>\n  <services>\n    <name>v2:none:mine</name>\n    \
             <state>\n      <status>running</status>\n    </state>\n  </services>\n  \
             <services>\n    <name>v2:none:other</name>\n    <state>\n      \
             <status>running</status>\n    </state>\n  </services>\n\
             </ServiceStateFile>\n",
        )
        .unwrap();

        let manifest = dir.path().join("mine-1111.yaml");
        std::fs::write(
            &manifest,
            "version: \"2\"\nservices:\n  mine:\n    command: 'echo hi'\n",
        )
        .unwrap();
        let config = load_config(Some(&manifest.to_string_lossy())).unwrap();

        seed_project_state_from_legacy(
            state_dir,
            crate::state_store::LOOSE_PROJECT_ID,
            &config,
        )
        .unwrap();

        let target = state_dir
            .join(crate::state_store::PROJECTS_DIR)
            .join(&config.project.id);
        let pid = std::fs::read_to_string(target.join(crate::constants::PID_FILE_NAME))
            .unwrap();
        assert!(pid.contains("<pid>4242</pid>"));
        // A sibling loose service the config does not declare stays behind.
        assert!(!pid.contains("9999"));

        let state =
            std::fs::read_to_string(target.join(crate::constants::STATE_FILE_NAME))
                .unwrap();
        assert!(state.contains(&format!("<name>{}</name>", config.state_key("mine"))));
        assert!(!state.contains("v2:none:"));

        // The legacy tree is untouched — retiring it stays migrate-state's job.
        assert!(legacy.join(crate::constants::PID_FILE_NAME).exists());
    }

    #[test]
    fn seeding_never_clobbers_state_the_new_identity_already_has() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();
        let legacy = state_dir
            .join(crate::state_store::PROJECTS_DIR)
            .join(crate::state_store::LOOSE_PROJECT_ID);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join(crate::constants::PID_FILE_NAME),
            "<PidFile>\n  <services>\n    <name>mine</name>\n    <pid>4242</pid>\n  \
             </services>\n</PidFile>\n",
        )
        .unwrap();

        let manifest = dir.path().join("mine-1111.yaml");
        std::fs::write(
            &manifest,
            "version: \"2\"\nservices:\n  mine:\n    command: 'echo hi'\n",
        )
        .unwrap();
        let config = load_config(Some(&manifest.to_string_lossy())).unwrap();

        let target = state_dir
            .join(crate::state_store::PROJECTS_DIR)
            .join(&config.project.id);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join(crate::constants::PID_FILE_NAME),
            b"<PidFile>NEWER</PidFile>",
        )
        .unwrap();

        seed_project_state_from_legacy(
            state_dir,
            crate::state_store::LOOSE_PROJECT_ID,
            &config,
        )
        .unwrap();

        let kept = std::fs::read_to_string(target.join(crate::constants::PID_FILE_NAME))
            .unwrap();
        assert_eq!(kept, "<PidFile>NEWER</PidFile>");
    }

    #[test]
    fn rekeying_a_state_block_produces_wellformed_xml() {
        let block = "  <services>\n    <name>v2:none:ngrok-tunnel</name>\n    \
                     <state>\n      <status>running</status>\n    </state>\n  </services>\n";
        let rekeyed = rekey_state_block(block, "v2:ngrok-abcd:ngrok-tunnel");

        assert!(rekeyed.contains("<name>v2:ngrok-abcd:ngrok-tunnel</name>"));
        // The closing tag must survive intact — an off-by-one here writes state
        // that no reader can parse back.
        assert_eq!(rekeyed.matches("</name>").count(), 1);
        assert_eq!(rekeyed.matches("<name>").count(), 1);
        assert!(rekeyed.contains("<status>running</status>"));
        assert_eq!(
            xml_field_values(&rekeyed, "name"),
            vec!["v2:ngrok-abcd:ngrok-tunnel"]
        );
    }

    #[test]
    fn published_state_is_readable_back_by_the_same_scanner() {
        let raw = "<ServiceStateFile>\n  <services>\n    \
                   <name>v2:none:svc</name>\n    <state>\n      \
                   <status>running</status>\n    </state>\n  </services>\n\
                   </ServiceStateFile>";
        let block = state_block_for(raw, "svc").expect("block");
        let rekeyed = rekey_state_block(&block, "v2:proj-abcd:svc");
        let doc = format!("<ServiceStateFile>\n{rekeyed}</ServiceStateFile>\n");

        assert_eq!(
            xml_field_values(&doc, "name")
                .iter()
                .map(|key| service_from_state_key(key))
                .collect::<Vec<_>>(),
            vec!["svc"]
        );
    }

    #[test]
    fn state_keys_yield_their_service_name() {
        assert_eq!(
            service_from_state_key("v2:none:gamecast-tunnel"),
            "gamecast-tunnel"
        );
        assert_eq!(
            service_from_state_key("v2:arbitration-dev:arb_rs"),
            "arb_rs"
        );
        assert_eq!(service_from_state_key("bare-name"), "bare-name");
    }

    #[test]
    fn xml_fields_are_extracted_in_order() {
        let raw = "<PidFile>\n  <services>\n    <name>alpha</name>\n    <pid>1</pid>\n  \
                   </services>\n  <services>\n    <name>beta</name>\n    <pid>2</pid>\n  \
                   </services>\n</PidFile>";
        assert_eq!(xml_field_values(raw, "name"), vec!["alpha", "beta"]);
        assert_eq!(xml_field_values(raw, "pid"), vec!["1", "2"]);
        assert!(xml_field_values(raw, "absent").is_empty());
    }

    #[test]
    fn the_users_real_legacy_state_shape_is_read_correctly() {
        // Verbatim shape of the user's projects/__loose__/state.xml.
        let raw = "<ServiceStateFile>\n  <services>\n    \
                   <name>v2:none:gamecast-tunnel</name>\n    <state>\n      \
                   <status>stopped</status>\n    </state>\n  </services>\n  \
                   <services>\n    <name>v2:none:ngrok-tunnel</name>\n    <state>\n      \
                   <status>running</status>\n      <pid>19223</pid>\n    </state>\n  \
                   </services>\n</ServiceStateFile>";
        let services: Vec<String> = xml_field_values(raw, "name")
            .iter()
            .map(|key| service_from_state_key(key))
            .collect();
        assert_eq!(services, vec!["gamecast-tunnel", "ngrok-tunnel"]);
    }

    #[test]
    fn the_users_real_cron_state_shape_is_read_correctly() {
        // Verbatim shape of the user's cron_state.xml: two jobs, same name.
        let raw = "<CronStateFile>\n  <jobs>\n    <hash>de98291cbf657443</hash>\n    \
                   <state>\n      <service_name>test_service</service_name>\n    \
                   </state>\n  </jobs>\n  <jobs>\n    <hash>eb78b21f76b9fe8f</hash>\n    \
                   <state>\n      <service_name>test_service</service_name>\n    \
                   </state>\n  </jobs>\n</CronStateFile>";
        assert_eq!(
            xml_field_values(raw, "hash"),
            vec!["de98291cbf657443", "eb78b21f76b9fe8f"]
        );
        assert_eq!(
            xml_field_values(raw, "service_name"),
            vec!["test_service", "test_service"]
        );
    }

    #[test]
    fn a_plan_over_the_users_real_artifacts_quarantines_only_what_is_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let log_dir = dir.path().join("logs");
        let legacy = plan::legacy_project_dir(&state_dir);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(plan::legacy_log_dir(&log_dir)).unwrap();

        std::fs::write(
            legacy.join(crate::constants::STATE_FILE_NAME),
            "<ServiceStateFile><services><name>v2:none:gamecast-tunnel</name></services>\
             <services><name>v2:none:ngrok-tunnel</name></services></ServiceStateFile>",
        )
        .unwrap();
        std::fs::write(
            legacy.join(crate::state_store::CRON_FILE_NAME),
            "<CronStateFile><jobs><hash>de98291cbf657443</hash>\
             <state><service_name>test_service</service_name></state></jobs></CronStateFile>",
        )
        .unwrap();
        std::fs::write(
            plan::legacy_log_dir(&log_dir).join("gamecast-tunnel.log"),
            b"data",
        )
        .unwrap();

        let candidates = vec![
            candidate_for("/units/gamecast-tunnel-3a2a.yaml", "gamecast-tunnel"),
            candidate_for("/units/gamecast-tunnel-5a9c.yaml", "gamecast-tunnel"),
            candidate_for("/units/gamecast-tunnel-6df6.yaml", "gamecast-tunnel"),
            candidate_for("/units/ngrok-tunnel-8d7b.yaml", "ngrok-tunnel"),
        ];

        let migration = plan_migration(&state_dir, &log_dir, &candidates).unwrap();

        // ngrok is uniquely declared and migrates.
        assert!(migration.services["ngrok-tunnel"].project_id().is_some());
        // gamecast is declared three times and must not be guessed.
        assert!(migration.services["gamecast-tunnel"].project_id().is_none());
        // Its log is equally ambiguous.
        assert!(migration.logs["gamecast-tunnel.log"].project_id().is_none());
        // The orphan cron row matches nothing.
        assert!(
            migration.cron_jobs["de98291cbf657443"]
                .project_id()
                .is_none()
        );

        let report = describe(&migration, &candidates);
        assert_eq!(report.migrated_services.len(), 1);
        assert_eq!(report.quarantined.len(), 3);
        assert_eq!(report.registry_entries.len(), 1);
    }

    fn candidate_for(path: &str, service: &str) -> Candidate {
        use std::collections::{BTreeMap, BTreeSet};
        let path = PathBuf::from(path);
        Candidate {
            project_id: crate::config::loose_project_id(&path),
            config_path: path,
            services: BTreeSet::from([service.to_string()]),
            service_hashes: BTreeMap::from([(
                service.to_string(),
                "deadbeef".to_string(),
            )]),
        }
    }
}
