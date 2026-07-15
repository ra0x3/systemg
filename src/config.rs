//! Configuration management for Systemg.
use std::{
    collections::{BTreeSet, HashMap},
    env, fmt, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use regex::Regex;
use serde::{Deserialize, Deserializer, de::Error as _};
use sha2::{Digest, Sha256};
use strum_macros::AsRefStr;
use tracing::warn;

use crate::{
    error::ProcessManagerError,
    metrics::{MetricsSettings, SpilloverSettings},
};

/// Current manifest schema version used by the runtime after migration.
pub const CURRENT_MANIFEST_VERSION: Version = Version::V1;

/// Supported manifest schema versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
    /// Initial Systemg manifest schema.
    V1,
}

impl Version {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "1" => Ok(Self::V1),
            other => Err(format!(
                "unsupported manifest version '{other}'; supported versions: 1"
            )),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V1 => f.write_str("1"),
        }
    }
}

impl serde::Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VersionVisitor;

        impl<'de> serde::de::Visitor<'de> for VersionVisitor {
            type Value = Version;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a manifest version as a string or integer")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Version::parse(value).map_err(E::custom)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Version::parse(&value.to_string()).map_err(E::custom)
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value < 0 {
                    return Err(E::custom(format!(
                        "unsupported manifest version '{value}'; supported versions: 1"
                    )));
                }
                Version::parse(&value.to_string()).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(VersionVisitor)
    }
}

/// Represents the structure of the configuration file.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// Configuration version.
    pub version: Version,
    /// Stable project namespace and display metadata.
    #[serde(default)]
    pub project: ProjectConfig,
    /// Map of service names to their respective configurations.
    pub services: HashMap<String, ServiceConfig>,
    /// Root directory from which relative paths are resolved.
    pub project_dir: Option<String>,
    /// Optional environment variables that apply to all services by default.
    /// Service-level env configurations override these root-level settings.
    pub env: Option<EnvConfig>,
    /// Metrics collection configuration.
    #[serde(default)]
    pub metrics: MetricsConfig,
    /// Service output logging defaults.
    #[serde(default)]
    pub logs: LogsConfig,
    /// Status and inspect snapshot collection configuration.
    #[serde(default)]
    pub status: StatusConfig,
}

#[derive(Debug, Deserialize)]
struct ManifestHeader {
    version: Version,
}

/// Version 1 manifest schema as accepted from YAML before migration.
#[derive(Debug, Deserialize)]
pub struct ConfigV1 {
    /// Configuration version.
    pub version: Version,
    /// Deprecated single-project declaration. Superseded by `projects`, but still
    /// accepted (with a warning) so existing manifests keep working.
    #[serde(default)]
    pub project: Option<ProjectConfigInput>,
    /// Canonical multi-project map keyed by project id. Each entry groups its own
    /// services; a file may declare one or many.
    #[serde(default)]
    pub projects: Option<HashMap<String, ProjectEntry>>,
    /// Services with no project (a loose bundle). With `projects`, these are the
    /// project-less units; with the legacy `project`, these are its services.
    #[serde(default)]
    pub services: HashMap<String, ServiceConfig>,
    /// Root directory from which relative paths are resolved.
    pub project_dir: Option<String>,
    /// Optional environment variables that apply to all services by default.
    pub env: Option<EnvConfig>,
    /// Metrics collection configuration.
    #[serde(default)]
    pub metrics: MetricsConfig,
    /// Service output logging defaults.
    #[serde(default)]
    pub logs: LogsConfig,
    /// Status and inspect snapshot collection configuration.
    #[serde(default)]
    pub status: StatusConfig,
}

/// One project inside a `projects:` map. The map key supplies the id; the entry
/// carries its display name and services, plus optional per-project overrides.
#[derive(Debug, Deserialize)]
pub struct ProjectEntry {
    /// Human-friendly display name. Defaults to the project id (the map key).
    #[serde(default)]
    pub name: Option<String>,
    /// Services belonging to this project.
    pub services: HashMap<String, ServiceConfig>,
    /// Optional per-project environment overrides.
    #[serde(default)]
    pub env: Option<EnvConfig>,
    /// Optional per-project logging defaults.
    #[serde(default)]
    pub logs: Option<LogsConfig>,
}

impl TryFrom<ConfigV1> for Config {
    type Error = String;

    fn try_from(value: ConfigV1) -> Result<Self, Self::Error> {
        if value.version != Version::V1 {
            return Err(format!(
                "cannot migrate manifest version {} as version 1",
                value.version
            ));
        }

        if value.project.is_some() && value.projects.is_some() {
            return Err(
                "a manifest may use 'project' or 'projects', not both; \
                 run 'sysg migrate' to convert 'project' to 'projects'"
                    .to_string(),
            );
        }

        // Single-value callers (the CLI, validation, status fallbacks) get the
        // FIRST project of a multi-project file. The supervisor uses the
        // fan-out path (`into_configs`) to load every project; this keeps the
        // 40+ `load_config` call sites working without each needing to know
        // about multi-project files.
        let mut projects = value.into_configs()?;
        if projects.is_empty() {
            return Ok(Config::default());
        }
        Ok(projects.remove(0))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_MANIFEST_VERSION,
            project: ProjectConfig::default(),
            services: HashMap::new(),
            project_dir: None,
            env: None,
            metrics: MetricsConfig::default(),
            logs: LogsConfig::default(),
            status: StatusConfig::default(),
        }
    }
}

impl ConfigV1 {
    /// Fans a v1 manifest into one `Config` per declared project, plus one for
    /// any project-less (loose) top-level services. Each returned `Config` is a
    /// normal single-project config the supervisor already knows how to load.
    fn into_configs(self) -> Result<Vec<Config>, String> {
        if self.project.is_some() && self.projects.is_some() {
            return Err(
                "a manifest may use 'project' or 'projects', not both".to_string(),
            );
        }

        let mut configs = Vec::new();

        if let Some(projects) = self.projects {
            let mut entries: Vec<(String, ProjectEntry)> = projects.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (id, entry) in entries {
                let mut project_services = entry.services;
                tag_project_scope(&mut project_services, &id);
                configs.push(Config {
                    version: CURRENT_MANIFEST_VERSION,
                    project: ProjectConfig {
                        name: entry.name.unwrap_or_else(|| id.clone()),
                        id,
                    },
                    services: project_services,
                    project_dir: self.project_dir.clone(),
                    env: entry.env.or_else(|| self.env.clone()),
                    metrics: self.metrics.clone(),
                    logs: entry.logs.unwrap_or_else(|| self.logs.clone()),
                    status: self.status.clone(),
                });
            }

            if !self.services.is_empty() {
                let mut loose = self.services;
                tag_project_scope(&mut loose, LOOSE_PROJECT_SCOPE);
                configs.push(Config {
                    version: CURRENT_MANIFEST_VERSION,
                    project: ProjectConfig::default(),
                    services: loose,
                    project_dir: self.project_dir,
                    env: self.env,
                    metrics: self.metrics,
                    logs: self.logs,
                    status: self.status,
                });
            }

            return Ok(configs);
        }

        configs.push(Config {
            version: CURRENT_MANIFEST_VERSION,
            project: self.project.map(Into::into).unwrap_or_default(),
            services: self.services,
            project_dir: self.project_dir,
            env: self.env,
            metrics: self.metrics,
            logs: self.logs,
            status: self.status,
        });
        Ok(configs)
    }
}
const METRICS_DEFAULT_RETENTION_MINUTES: u64 = 720; // 12 hours
const METRICS_DEFAULT_SAMPLE_INTERVAL_SECS: u64 = 1;
const METRICS_DEFAULT_MAX_MEMORY_BYTES: usize = 10 * 1024 * 1024;
const METRICS_DEFAULT_SPILLOVER_SEGMENT_BYTES: u64 = 256 * 1024;
const STATUS_DEFAULT_SNAPSHOT_INTERVAL_SECS: u64 = 5;
/// Default maximum size, in bytes, for an active service log file before rotation.
pub const LOGS_DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Default number of rotated service log files retained per active log.
pub const LOGS_DEFAULT_MAX_FILES: usize = 5;

/// Stable project namespace and display metadata.
#[derive(Debug, Clone, serde::Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Durable project identifier used to group runtime state.
    pub id: String,
    /// Human-friendly display name. Changing this does not change identity.
    pub name: String,
}

/// YAML shapes accepted for top-level project metadata before normalization.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ProjectConfigInput {
    /// Shorthand project declaration: `project: my_project`.
    Id(String),
    /// Explicit project declaration with stable id and display name.
    Fields {
        /// Durable project identifier used to group runtime state.
        id: String,
        /// Optional human-friendly display name.
        name: Option<String>,
    },
}

impl From<ProjectConfigInput> for ProjectConfig {
    fn from(input: ProjectConfigInput) -> Self {
        match input {
            ProjectConfigInput::Id(id) => Self {
                name: id.clone(),
                id,
            },
            ProjectConfigInput::Fields { id, name } => Self {
                name: name.unwrap_or_else(|| id.clone()),
                id,
            },
        }
    }
}

fn validate_project_id(id: &str) -> Result<(), ProcessManagerError> {
    if id.is_empty() {
        return Err(ProcessManagerError::ConfigParseError(
            serde_yaml::Error::custom("project.id must not be empty"),
        ));
    }

    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(ProcessManagerError::ConfigParseError(
            serde_yaml::Error::custom(
                "project.id may only contain ASCII letters, numbers, '_', '-', or '.'",
            ),
        ));
    }

    Ok(())
}

fn resolve_project_config(
    mut project: ProjectConfig,
    base_path: &Path,
) -> Result<ProjectConfig, ProcessManagerError> {
    if project.id.is_empty() {
        let canonical = base_path
            .canonicalize()
            .unwrap_or_else(|_| base_path.to_path_buf());
        let mut hasher = Sha256::new();
        hasher.update(canonical.to_string_lossy().as_bytes());
        let result = hasher.finalize();
        project.id = format!(
            "legacy-{:016x}",
            u64::from_be_bytes(result[0..8].try_into().unwrap())
        );
        project.name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(project.id.as_str())
            .to_string();
    } else {
        validate_project_id(&project.id)?;
        if project.name.trim().is_empty() {
            project.name = project.id.clone();
        }
    }

    Ok(project)
}

/// Output sink for supervised service stdout/stderr.
#[derive(Debug, Deserialize, Clone, Copy, serde::Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogSink {
    /// Persist service output to systemg-managed log files.
    #[default]
    File,
    /// Discard service output without creating log writer threads or files.
    None,
}

/// Logging configuration shared by global and service-level config blocks.
#[derive(Debug, Deserialize, Clone, serde::Serialize, Default)]
#[serde(default)]
pub struct LogsConfig {
    /// Where service stdout/stderr should be sent.
    pub sink: Option<LogSink>,
    /// Maximum active log-file size before rotation.
    pub max_bytes: Option<u64>,
    /// Number of rotated files to retain per active log.
    pub max_files: Option<usize>,
}

/// Fully resolved logging policy for a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveLogsConfig {
    /// Where service stdout/stderr should be sent.
    pub sink: LogSink,
    /// Maximum active log-file size before rotation.
    pub max_bytes: u64,
    /// Number of rotated files to retain per active log.
    pub max_files: usize,
}

impl Default for EffectiveLogsConfig {
    fn default() -> Self {
        Self {
            sink: LogSink::File,
            max_bytes: LOGS_DEFAULT_MAX_BYTES,
            max_files: LOGS_DEFAULT_MAX_FILES,
        }
    }
}

impl LogsConfig {
    /// Resolves this logging block over the built-in defaults.
    pub fn to_effective(&self) -> EffectiveLogsConfig {
        Self::merge(None, Some(self))
    }

    /// Resolves service logging over global logging and built-in defaults.
    pub fn merge(
        global: Option<&LogsConfig>,
        service: Option<&LogsConfig>,
    ) -> EffectiveLogsConfig {
        let defaults = EffectiveLogsConfig::default();
        EffectiveLogsConfig {
            sink: service
                .and_then(|logs| logs.sink)
                .or_else(|| global.and_then(|logs| logs.sink))
                .unwrap_or(defaults.sink),
            max_bytes: service
                .and_then(|logs| logs.max_bytes)
                .or_else(|| global.and_then(|logs| logs.max_bytes))
                .unwrap_or(defaults.max_bytes),
            max_files: service
                .and_then(|logs| logs.max_files)
                .or_else(|| global.and_then(|logs| logs.max_files))
                .unwrap_or(defaults.max_files),
        }
    }
}

/// Snapshot collection mode for status and inspect views.
#[derive(Debug, Deserialize, Clone, Copy, serde::Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum StatusSnapshotMode {
    /// Use persisted state only; no background live process snapshot refresh.
    Off,
    /// Collect cheap service state without process tree expansion.
    #[default]
    Summary,
    /// Collect the full process tree and runtime command details.
    Detailed,
}

/// Status and inspect snapshot configuration.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
#[serde(default)]
pub struct StatusConfig {
    /// Snapshot collection mode.
    pub snapshot_mode: StatusSnapshotMode,
    /// Interval between background status snapshot refreshes.
    pub snapshot_interval_secs: u64,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            snapshot_mode: StatusSnapshotMode::Summary,
            snapshot_interval_secs: STATUS_DEFAULT_SNAPSHOT_INTERVAL_SECS,
        }
    }
}

impl StatusConfig {
    /// Returns the clamped snapshot refresh interval.
    pub fn snapshot_interval(&self) -> Duration {
        Duration::from_secs(self.snapshot_interval_secs.clamp(1, 300))
    }
}

/// Top-level metrics configuration block.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct MetricsConfig {
    /// Number of minutes to retain in-memory samples (minimum: 1).
    pub retention_minutes: u64,
    /// Sampling interval in seconds (clamped between 1 and 60).
    pub sample_interval_secs: u64,
    /// Maximum memory used across all ring buffers (bytes).
    pub max_memory_bytes: usize,
    /// Optional directory path for spillover segments.
    pub spillover_path: Option<String>,
    /// Maximum bytes to persist on disk for spillover segments.
    pub spillover_max_bytes: Option<u64>,
    /// Preferred segment size when rotating spillover files.
    pub spillover_segment_bytes: Option<u64>,
}

impl Default for MetricsConfig {
    /// Returns the default this item.
    fn default() -> Self {
        Self {
            retention_minutes: METRICS_DEFAULT_RETENTION_MINUTES,
            sample_interval_secs: METRICS_DEFAULT_SAMPLE_INTERVAL_SECS,
            max_memory_bytes: METRICS_DEFAULT_MAX_MEMORY_BYTES,
            spillover_path: None,
            spillover_max_bytes: None,
            spillover_segment_bytes: None,
        }
    }
}

impl MetricsConfig {
    /// Converts the configuration into runtime settings.
    pub fn to_settings(&self, project_dir: Option<&Path>) -> MetricsSettings {
        let retention_minutes = self.retention_minutes.max(1);
        let sample_interval_secs = self.sample_interval_secs.clamp(1, 60);
        let max_memory_bytes = self.max_memory_bytes.max(128 * 1024);

        let spillover = self.spillover_path.as_ref().and_then(|raw| {
            let mut path = PathBuf::from(raw);
            if path.is_relative()
                && let Some(base) = project_dir
            {
                path = base.join(path);
            }

            let max_bytes = self.spillover_max_bytes.unwrap_or(6 * 1024 * 1024);
            if max_bytes == 0 {
                return None;
            }

            Some(SpilloverSettings {
                directory: path,
                max_bytes,
                segment_bytes: self
                    .spillover_segment_bytes
                    .unwrap_or(METRICS_DEFAULT_SPILLOVER_SEGMENT_BYTES),
            })
        });

        MetricsSettings {
            retention: Duration::from_secs(retention_minutes * 60),
            sample_interval: Duration::from_secs(sample_interval_secs),
            max_memory_bytes,
            spillover,
        }
    }
}

/// Skip configuration for a service.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum SkipConfig {
    /// Boolean flag that, when `true`, always skips the service.
    Flag(bool),
    /// Command that decides whether the service should be skipped.
    /// A zero exit status means the service is skipped.
    Command(String),
}

/// Spawn mode configuration for dynamic child process creation.
#[derive(Debug, Deserialize, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpawnMode {
    /// Static mode - no dynamic spawning allowed (default).
    Static,
    /// Dynamic mode - allows runtime spawning of child processes.
    Dynamic,
}

/// Configuration for dynamic process spawning.
#[derive(Debug, Deserialize, Clone, serde::Serialize, Default)]
pub struct SpawnConfig {
    /// Spawn mode (static or dynamic).
    pub mode: Option<SpawnMode>,
    /// Resource and safety limits for dynamically spawned children.
    pub limits: Option<SpawnLimitsConfig>,
}

/// Resource limits and policies for dynamically spawned children.
#[derive(Debug, Deserialize, Clone, serde::Serialize, Default)]
pub struct SpawnLimitsConfig {
    /// Maximum number of direct children allowed.
    pub children: Option<u32>,
    /// Maximum depth of the spawn tree (levels of recursion).
    pub depth: Option<u32>,
    /// Maximum total descendants across all levels.
    pub descendants: Option<u32>,
    /// Total memory limit shared by entire spawn tree.
    pub total_memory: Option<String>,
    /// Policy for handling process tree termination.
    pub termination_policy: Option<TerminationPolicy>,
}

/// Policy for handling process termination in spawn trees.
#[derive(Debug, Deserialize, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TerminationPolicy {
    /// Cascade - terminate all descendants when parent dies.
    Cascade,
    /// Orphan - leave children running when parent dies.
    Orphan,
    /// Reparent - reassign children to init process.
    Reparent,
}

/// Readiness condition a dependency must reach before dependents start.
#[derive(Debug, Default, Deserialize, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DependsOnCondition {
    /// Dependency is running (and passed its health check, if any).
    #[default]
    Started,
    /// Dependency exited successfully before dependents start.
    Completed,
}

/// A single `depends_on` entry: a bare service name or a detailed form
/// with an explicit readiness condition.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub enum DependsOn {
    /// Bare service name; waits for the dependency to start.
    Name(String),
    /// Detailed form selecting how long to wait for the dependency.
    Detailed {
        /// Name of the dependency service.
        service: String,
        /// Condition the dependency must reach.
        #[serde(default)]
        condition: DependsOnCondition,
    },
}

impl DependsOn {
    /// Name of the dependency service.
    pub fn service(&self) -> &str {
        match self {
            DependsOn::Name(name) => name,
            DependsOn::Detailed { service, .. } => service,
        }
    }

    /// Condition the dependency must reach before dependents start.
    pub fn condition(&self) -> DependsOnCondition {
        match self {
            DependsOn::Name(_) => DependsOnCondition::Started,
            DependsOn::Detailed { condition, .. } => *condition,
        }
    }
}

impl From<&str> for DependsOn {
    fn from(name: &str) -> Self {
        DependsOn::Name(name.to_string())
    }
}

impl From<String> for DependsOn {
    fn from(name: String) -> Self {
        DependsOn::Name(name)
    }
}

/// Configuration for an individual service.
#[derive(Debug, Default, Deserialize, Clone, serde::Serialize)]
pub struct ServiceConfig {
    /// Command used to start the service.
    pub command: String,
    /// Optional environment variables for the service.
    pub env: Option<EnvConfig>,
    /// User that should own the running process.
    pub user: Option<String>,
    /// Primary group for the running process.
    pub group: Option<String>,
    /// Supplementary groups to apply after switching users.
    #[serde(default, rename = "supplementary_groups")]
    pub supplementary_groups: Option<Vec<String>>,
    /// Resource limit configuration applied prior to exec.
    pub limits: Option<LimitsConfig>,
    /// Linux capabilities retained for the service when started as root.
    pub capabilities: Option<Vec<String>>,
    /// Namespace and confinement settings for sandboxed execution.
    pub isolation: Option<IsolationConfig>,
    /// Restart policy (e.g., "always", "on-failure", "never").
    pub restart_policy: Option<String>,
    /// Backoff time before restarting a failed service.
    pub backoff: Option<String>,
    /// Maximum number of restart attempts before giving up (None = unlimited).
    pub max_restarts: Option<u32>,
    /// List of services that must start before this service.
    pub depends_on: Option<Vec<DependsOn>>,
    /// Deployment strategy configuration.
    pub deployment: Option<DeploymentConfig>,
    /// Hooks for lifecycle events (e.g., on_start, on_error).
    pub hooks: Option<Hooks>,
    /// Cron configuration for scheduled service execution.
    pub cron: Option<CronConfig>,
    /// Optional skip configuration that determines if the service should be skipped.
    pub skip: Option<SkipConfig>,
    /// Dynamic process spawning configuration.
    pub spawn: Option<SpawnConfig>,
    /// Service output logging overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<LogsConfig>,
    /// Project this service belongs to, injected during multi-project fan-out so
    /// identical service configs in different projects hash distinctly and never
    /// collide in the shared pid/state files. `None` for single-project files, so
    /// their existing state-file keys stay byte-for-byte unchanged (no migration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope: Option<String>,
}

/// Resource limit overrides configured per service.
#[derive(Debug, Deserialize, Clone, serde::Serialize, Default)]
pub struct LimitsConfig {
    /// Maximum number of open file descriptors (`RLIMIT_NOFILE`).
    pub nofile: Option<LimitValue>,
    /// Maximum number of processes (`RLIMIT_NPROC`).
    pub nproc: Option<LimitValue>,
    /// Maximum locked memory in bytes (`RLIMIT_MEMLOCK`).
    pub memlock: Option<LimitValue>,
    /// CPU scheduling priority (`nice` value, -20..19).
    pub nice: Option<i32>,
    /// CPU affinity mask specified as CPU indices.
    pub cpu_affinity: Option<Vec<u16>>,
    /// Optional cgroup v2 parameters applied after spawn.
    pub cgroup: Option<CgroupConfig>,
}

/// Configuration options for cgroup v2 controllers.
#[derive(Debug, Deserialize, Clone, serde::Serialize, Default)]
pub struct CgroupConfig {
    /// Absolute path for the cgroup base; defaults to `/sys/fs/cgroup/systemg` when omitted.
    pub root: Option<String>,
    /// Memory limit written to `memory.max` (e.g., `512M`, `max`).
    pub memory_max: Option<String>,
    /// CPU quota written to `cpu.max` (e.g., `max` or `200000 100000`).
    pub cpu_max: Option<String>,
    /// CPU weight written to `cpu.weight` (between 1 and 10000).
    pub cpu_weight: Option<u64>,
}

/// Value accepted for `setrlimit`-backed configuration entries.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LimitValue {
    /// A fixed numeric soft+hard limit.
    Fixed(u64),
    /// Unlimited (maps to `RLIM_INFINITY`).
    Unlimited,
}

impl<'de> Deserialize<'de> for LimitValue {
    /// Handles deserialize.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Represents limit visitor.
        struct LimitVisitor;

        impl<'de> serde::de::Visitor<'de> for LimitVisitor {
            type Value = LimitValue;

            /// Handles expecting.
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a non-negative integer, an optional size suffix (e.g. 512M), or 'unlimited'",
                )
            }

            /// Visits u64.
            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(LimitValue::Fixed(value))
            }

            /// Visits str.
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match parse_limit(value) {
                    Ok(bytes) => Ok(LimitValue::Fixed(bytes)),
                    Err(LimitParseError::Unlimited) => Ok(LimitValue::Unlimited),
                    Err(LimitParseError::Invalid(_)) => {
                        Err(E::invalid_value(serde::de::Unexpected::Str(value), &self))
                    }
                }
            }

            /// Visits i64.
            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value < 0 {
                    return Err(E::invalid_value(
                        serde::de::Unexpected::Signed(value),
                        &"non-negative integer",
                    ));
                }
                Ok(LimitValue::Fixed(value as u64))
            }
        }

        deserializer.deserialize_any(LimitVisitor)
    }
}

#[derive(Debug)]
/// Defines limit parse error values.
enum LimitParseError {
    Unlimited,
    Invalid(String),
}

/// Parses limit.
fn parse_limit(input: &str) -> Result<u64, LimitParseError> {
    let trimmed = input.trim();
    if trimmed.eq_ignore_ascii_case("unlimited") {
        return Err(LimitParseError::Unlimited);
    }

    let normalized = trimmed.replace('_', "");
    let without_bytes = normalized.trim_end_matches(&['B', 'b'][..]);

    let (number_part, factor) = match without_bytes.chars().last() {
        Some(suffix) if suffix.is_ascii_alphabetic() => {
            let len = without_bytes.len() - suffix.len_utf8();
            let number_part = &without_bytes[..len];
            let multiplier = match suffix.to_ascii_uppercase() {
                'K' => 1u128 << 10,
                'M' => 1u128 << 20,
                'G' => 1u128 << 30,
                'T' => 1u128 << 40,
                _ => return Err(LimitParseError::Invalid(trimmed.to_string())),
            };
            (number_part.trim(), multiplier)
        }
        _ => (without_bytes.trim(), 1u128),
    };

    if number_part.is_empty() {
        return Err(LimitParseError::Invalid(trimmed.to_string()));
    }

    let value = number_part
        .parse::<u128>()
        .map_err(|_| LimitParseError::Invalid(trimmed.to_string()))?;

    let bytes = value
        .checked_mul(factor)
        .ok_or_else(|| LimitParseError::Invalid(trimmed.to_string()))?;

    u64::try_from(bytes).map_err(|_| LimitParseError::Invalid(trimmed.to_string()))
}

impl std::fmt::Display for LimitParseError {
    /// Handles fmt.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LimitParseError::Unlimited => write!(f, "value represents unlimited"),
            LimitParseError::Invalid(value) => write!(f, "invalid limit value '{value}'"),
        }
    }
}

impl std::error::Error for LimitParseError {}

/// Linux namespace and confinement options.
#[derive(Debug, Deserialize, Clone, serde::Serialize, Default)]
pub struct IsolationConfig {
    /// Enable network namespace isolation.
    pub network: Option<bool>,
    /// Enable mount namespace isolation.
    pub mount: Option<bool>,
    /// Enable PID namespace isolation.
    pub pid: Option<bool>,
    /// Enable user namespace isolation.
    pub user: Option<bool>,
    /// Apply seccomp filtering profile.
    pub seccomp: Option<String>,
    /// AppArmor profile to enforce.
    pub apparmor_profile: Option<String>,
    /// SELinux context to apply.
    pub selinux_context: Option<String>,
    /// Restrict device access similar to `PrivateDevices`.
    pub private_devices: Option<bool>,
    /// Restrict temporary directories similar to `PrivateTmp`.
    pub private_tmp: Option<bool>,
}

impl ServiceConfig {
    /// Resolves effective logging settings for this service.
    pub fn effective_logs(&self, global: &LogsConfig) -> EffectiveLogsConfig {
        LogsConfig::merge(Some(global), self.logs.as_ref())
    }

    /// Computes a stable hash of this service configuration, excluding the service name.
    /// This hash is used to identify the service state across renames.
    ///
    /// # Returns
    /// A 16-character hexadecimal string representing the first 64 bits of the SHA256 hash.
    pub fn compute_hash(&self) -> String {
        let json = serde_json::to_string(self)
            .expect("ServiceConfig should always be serializable");
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        let result = hasher.finalize();
        format!(
            "{:016x}",
            u64::from_be_bytes(result[0..8].try_into().unwrap())
        )
    }
}

/// Deployment strategy configuration for a service.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct DeploymentConfig {
    /// Deployment strategy: "rolling" or "immediate".
    pub strategy: Option<String>,
    /// Command to run before starting the new service.
    pub pre_start: Option<String>,
    /// Health check configuration.
    pub health_check: Option<HealthCheckConfig>,
    /// Grace period before stopping the old service instance.
    pub grace_period: Option<String>,
    /// Optional blue/green rollout settings for single-host zero-downtime deployments.
    pub blue_green: Option<BlueGreenDeploymentConfig>,
}

/// Blue/green rollout configuration used by rolling deployments on a single host.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct BlueGreenDeploymentConfig {
    /// Environment variable used to inject the selected slot value (defaults to "PORT").
    pub env_var: Option<String>,
    /// Two slot values (commonly two port numbers) used as alternating targets.
    pub slots: Vec<String>,
    /// Command executed to switch traffic to the candidate slot once healthy.
    pub switch_command: Option<String>,
    /// Optional health check for candidate validation (supports `{slot}` substitution).
    pub candidate_health_check: Option<HealthCheckConfig>,
    /// Optional health check to verify after switch command completes.
    pub switch_verify: Option<HealthCheckConfig>,
    /// Optional path for persisting the active slot state.
    pub state_path: Option<String>,
}

/// Health check configuration used during rolling deployments.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthCheckConfig {
    /// Optional health check URL.
    pub url: Option<String>,
    /// Optional command-based health check.
    pub command: Option<String>,
    /// Time between health check attempts (e.g., "2s").
    pub interval: Option<String>,
    /// Health check timeout duration (e.g., "30s").
    pub timeout: Option<String>,
    /// Number of retries before giving up.
    pub retries: Option<u32>,
}

/// Deserializes the YAML shape accepted for generic health checks before validation.
#[derive(Debug, Deserialize)]
struct RawHealthCheckConfig {
    url: Option<String>,
    command: Option<String>,
    interval: Option<String>,
    timeout: Option<String>,
    retries: Option<u32>,
}

impl<'de> Deserialize<'de> for HealthCheckConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawHealthCheckConfig::deserialize(deserializer)?;
        if raw.url.is_none() && raw.command.is_none() {
            return Err(D::Error::custom(
                "health check requires at least one of 'url' or 'command'",
            ));
        }

        Ok(Self {
            url: raw.url,
            command: raw.command,
            interval: raw.interval,
            timeout: raw.timeout,
            retries: raw.retries,
        })
    }
}

/// Represents environment variables for a service.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct EnvConfig {
    /// Optional path to an environment file.
    pub file: Option<String>,
    /// Key-value pairs of environment variables.
    pub vars: Option<HashMap<String, String>>,
    /// Whether to strip caller/session-scoped variables (e.g. `SSH_AUTH_SOCK`)
    /// from the service environment. Defaults to `true`.
    pub clear_session_vars: Option<bool>,
    /// Additional inherited variables to remove from the service environment.
    pub strip: Option<Vec<String>>,
    /// Whether a privilege-dropped service inherits the supervisor's environment.
    /// Defaults to `false`: services that switch user/group start from a clean
    /// environment so root's variables (secrets, `LD_*`) do not leak across the
    /// privilege boundary. Set `true` to opt back into full inheritance.
    pub inherit_env: Option<bool>,
}

#[derive(Debug, Deserialize)]
/// Deserializes supported `env` block shapes before normalizing them into `EnvConfig`.
struct RawEnvConfig {
    /// Optional path to an environment file.
    file: Option<String>,
    /// Explicit nested environment variables.
    vars: Option<HashMap<String, String>>,
    /// Whether to strip caller/session-scoped variables from the service env.
    clear_session_vars: Option<bool>,
    /// Additional inherited variables to remove from the service env.
    strip: Option<Vec<String>>,
    /// Whether a privilege-dropped service inherits the supervisor environment.
    inherit_env: Option<bool>,
    /// Direct key/value pairs provided alongside `file` or instead of `vars`.
    #[serde(flatten)]
    entries: HashMap<String, String>,
}

impl<'de> Deserialize<'de> for EnvConfig {
    /// Deserializes an environment block, accepting either nested `vars` or direct key/value
    /// entries under `env`.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawEnvConfig::deserialize(deserializer)?;
        let mut vars = raw.entries;

        if let Some(explicit_vars) = raw.vars {
            vars.extend(explicit_vars);
        }

        Ok(Self {
            file: raw.file,
            vars: if vars.is_empty() { None } else { Some(vars) },
            clear_session_vars: raw.clear_session_vars,
            strip: raw.strip,
            inherit_env: raw.inherit_env,
        })
    }
}

impl EnvConfig {
    /// Resolves the full path to the env file based on a base directory.
    pub fn path(&self, base: &Path) -> Option<PathBuf> {
        self.file.as_ref().map(|f| {
            let path = Path::new(f);
            if path.is_absolute() || path.exists() {
                path.to_path_buf()
            } else {
                base.join(path)
            }
        })
    }

    /// Returns the inherited environment variables that must be removed from a
    /// service process. Session-scoped variables are stripped unless
    /// `clear_session_vars` is `false`; `strip` entries are always removed.
    /// A variable explicitly set in `vars` is never stripped.
    pub fn vars_to_strip(&self) -> Vec<String> {
        let mut to_strip: Vec<String> = Vec::new();

        if self.clear_session_vars.unwrap_or(true) {
            to_strip.extend(
                crate::constants::SESSION_SCOPED_ENV_VARS
                    .iter()
                    .map(|var| var.to_string()),
            );
        }

        if let Some(extra) = &self.strip {
            to_strip.extend(extra.iter().cloned());
        }

        if let Some(vars) = &self.vars {
            to_strip.retain(|var| !vars.contains_key(var));
        }

        to_strip.sort();
        to_strip.dedup();
        to_strip
    }

    /// Merges two EnvConfig instances, with the service-level config taking precedence.
    /// Returns a new EnvConfig that combines root and service-level settings.
    pub fn merge(
        root: Option<&EnvConfig>,
        service: Option<&EnvConfig>,
    ) -> Option<EnvConfig> {
        match (root, service) {
            (None, None) => None,
            (Some(r), None) => Some(r.clone()),
            (None, Some(s)) => Some(s.clone()),
            (Some(root_cfg), Some(service_cfg)) => {
                let mut merged_vars = root_cfg.vars.clone().unwrap_or_default();
                if let Some(service_vars) = &service_cfg.vars {
                    merged_vars.extend(service_vars.clone());
                }
                let file = service_cfg.file.clone().or_else(|| root_cfg.file.clone());

                let mut merged_strip = root_cfg.strip.clone().unwrap_or_default();
                if let Some(service_strip) = &service_cfg.strip {
                    merged_strip.extend(service_strip.clone());
                }

                Some(EnvConfig {
                    file,
                    vars: if merged_vars.is_empty() {
                        None
                    } else {
                        Some(merged_vars)
                    },
                    clear_session_vars: service_cfg
                        .clear_session_vars
                        .or(root_cfg.clear_session_vars),
                    strip: if merged_strip.is_empty() {
                        None
                    } else {
                        Some(merged_strip)
                    },
                    inherit_env: service_cfg.inherit_env.or(root_cfg.inherit_env),
                })
            }
        }
    }
}

/// Lifecycle stages for service hooks.
#[derive(Debug, Clone, Copy, AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum HookStage {
    /// Hook triggered when service starts.
    OnStart,
    /// Hook triggered when service stops.
    OnStop,
    /// Hook triggered when service restarts.
    OnRestart,
}

/// Outcomes recorded for a lifecycle stage.
#[derive(Debug, Clone, Copy, AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum HookOutcome {
    /// Hook outcome when service lifecycle event succeeds.
    Success,
    /// Hook outcome when service lifecycle event fails.
    Error,
}

/// Command executed for a hook outcome.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct HookAction {
    /// Shell command to execute for this hook.
    pub command: String,
    /// Optional timeout for the hook command (e.g., "5s", "1m").
    pub timeout: Option<String>,
}

/// Hook commands grouped by outcome for a lifecycle stage.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct HookLifecycleConfig {
    /// Hook action to execute when the lifecycle event succeeds.
    pub success: Option<HookAction>,
    /// Hook action to execute when the lifecycle event fails.
    pub error: Option<HookAction>,
}

/// Hooks that run on specific service lifecycle events.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct Hooks {
    /// Hooks to execute when the service starts.
    pub on_start: Option<HookLifecycleConfig>,
    /// Hooks to execute when the service stops.
    pub on_stop: Option<HookLifecycleConfig>,
    /// Hooks to execute when the service restarts.
    #[serde(default)]
    pub on_restart: Option<HookLifecycleConfig>,
}

impl Hooks {
    /// Returns the configured hook action for a lifecycle stage and outcome.
    pub fn action(&self, stage: HookStage, outcome: HookOutcome) -> Option<&HookAction> {
        let lifecycle = match stage {
            HookStage::OnStart => self.on_start.as_ref(),
            HookStage::OnStop => self.on_stop.as_ref(),
            HookStage::OnRestart => self.on_restart.as_ref(),
        }?;

        match outcome {
            HookOutcome::Success => lifecycle.success.as_ref(),
            HookOutcome::Error => lifecycle.error.as_ref(),
        }
    }
}

/// Cron configuration for scheduled service execution.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct CronConfig {
    /// Cron expression defining the schedule (e.g., "0 * * * * *").
    pub expression: String,
    /// Optional timezone for cron scheduling (defaults to system timezone).
    pub timezone: Option<String>,
}

impl Config {
    /// Computes a mapping from service names to their configuration hashes.
    /// This is used to identify services across renames.
    pub fn service_hashes(&self) -> HashMap<String, String> {
        self.services
            .iter()
            .map(|(name, config)| (name.clone(), config.compute_hash()))
            .collect()
    }

    /// Gets the configuration hash for a specific service by name.
    /// Returns None if the service doesn't exist.
    pub fn get_service_hash(&self, service_name: &str) -> Option<String> {
        self.services
            .get(service_name)
            .map(|cfg| cfg.compute_hash())
    }

    /// Returns services ordered so dependencies start before dependents.
    pub fn service_start_order(&self) -> Result<Vec<String>, ProcessManagerError> {
        let mut indegree: HashMap<String, usize> =
            self.services.keys().map(|name| (name.clone(), 0)).collect();
        let mut graph: HashMap<String, Vec<String>> = HashMap::new();

        for (service, cfg) in &self.services {
            if let Some(deps) = &cfg.depends_on {
                for dep in deps {
                    let dep_name = dep.service();
                    let Some(dep_cfg) = self.services.get(dep_name) else {
                        return Err(ProcessManagerError::UnknownDependency {
                            service: service.clone(),
                            dependency: dep_name.to_string(),
                        });
                    };

                    if dep.condition() == DependsOnCondition::Completed
                        && let Some(policy @ "always") = dep_cfg.restart_policy.as_deref()
                    {
                        return Err(ProcessManagerError::DependencyNeverCompletes {
                            service: service.clone(),
                            dependency: dep_name.to_string(),
                            policy: policy.to_string(),
                        });
                    }

                    *indegree.get_mut(service).expect("service must exist") += 1;
                    graph
                        .entry(dep_name.to_string())
                        .or_default()
                        .push(service.clone());
                }
            }
        }

        let mut ready: BTreeSet<String> = indegree
            .iter()
            .filter(|&(_, &deg)| deg == 0)
            .map(|(name, _)| name.clone())
            .collect();

        let mut order = Vec::with_capacity(self.services.len());

        while let Some(service) = ready.pop_first() {
            order.push(service.clone());

            if let Some(children) = graph.get(&service) {
                for child in children {
                    if let Some(deg) = indegree.get_mut(child) {
                        *deg -= 1;
                        if *deg == 0 {
                            ready.insert(child.clone());
                        }
                    }
                }
            }
        }

        if order.len() != self.services.len() {
            let remaining: Vec<String> = indegree
                .into_iter()
                .filter(|(_, deg)| *deg > 0)
                .map(|(name, _)| name)
                .collect();

            return Err(ProcessManagerError::DependencyCycle {
                cycle: remaining.join(" -> "),
            });
        }

        Ok(order)
    }

    /// Returns a map of each service to the services that depend on it.
    pub fn reverse_dependencies(&self) -> HashMap<String, Vec<String>> {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();

        for (service, cfg) in &self.services {
            if let Some(deps) = &cfg.depends_on {
                for dep in deps {
                    map.entry(dep.service().to_string())
                        .or_default()
                        .push(service.clone());
                }
            }
        }

        for dependents in map.values_mut() {
            dependents.sort();
        }

        map
    }
}

/// Expands environment variables within a string.
fn expand_env_vars(input: &str) -> Result<String, ProcessManagerError> {
    let re = Regex::new(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?").unwrap();
    let mut missing = None;
    let result = re.replace_all(input, |caps: &regex::Captures| {
        let var_name = &caps[1];
        match env::var(var_name) {
            Ok(value) => value,
            Err(_) => {
                if missing.is_none() {
                    missing = Some(var_name.to_string());
                }
                String::new()
            }
        }
    });

    if let Some(var_name) = missing {
        return Err(ProcessManagerError::MissingEnvVar(var_name));
    }

    Ok(result.to_string())
}

/// Loads an `.env` file and sets environment variables.
fn load_env_file(path: &str) -> Result<(), ProcessManagerError> {
    let content =
        fs::read_to_string(path).map_err(ProcessManagerError::ConfigReadError)?;
    for line in content.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let mut value = value.trim();

            if value.starts_with('"') && value.ends_with('"') {
                value = &value[1..value.len() - 1];
            }

            unsafe {
                env::set_var(key, value);
            }
        }
    }
    Ok(())
}

/// Parses a manifest using its declared schema version and migrates it to the
/// current runtime configuration shape.
pub fn parse_config_manifest(content: &str) -> Result<Config, serde_yaml::Error> {
    let header: ManifestHeader = serde_yaml::from_str(content)?;
    match header.version {
        Version::V1 => {
            let config: ConfigV1 = serde_yaml::from_str(content)?;
            config.try_into().map_err(serde_yaml::Error::custom)
        }
    }
}

/// Parses a manifest into one `Config` per declared project (plus one for any
/// loose top-level services). Single-project and legacy `project:` files yield a
/// one-element vec; a `projects:` map yields one per entry. This is the ingest
/// path the supervisor uses so one file can fan out into many project runtimes.
pub fn parse_config_projects(content: &str) -> Result<Vec<Config>, serde_yaml::Error> {
    let header: ManifestHeader = serde_yaml::from_str(content)?;
    match header.version {
        Version::V1 => {
            let config: ConfigV1 = serde_yaml::from_str(content)?;
            if config.projects.is_none() && uses_legacy_project_shape(&config) {
                warn!(
                    "'project:' is deprecated; run 'sysg migrate <file>' to convert to the 'projects:' format"
                );
            }
            config.into_configs().map_err(serde_yaml::Error::custom)
        }
    }
}

/// Whether a manifest uses the deprecated single-project shape (an explicit
/// `project:` block, or top-level services with no `projects:` map).
fn uses_legacy_project_shape(config: &ConfigV1) -> bool {
    config.project.is_some() || !config.services.is_empty()
}

/// Scope marker for the project-less (loose) service bundle in a multi-project
/// manifest, so loose services hash distinctly from any real project's.
const LOOSE_PROJECT_SCOPE: &str = "__loose__";

/// Stamps each service with its owning project so identical service configs in
/// different projects hash distinctly and never collide in shared state.
fn tag_project_scope(services: &mut HashMap<String, ServiceConfig>, scope: &str) {
    for service in services.values_mut() {
        service.project_scope = Some(scope.to_string());
    }
}

/// Rewrites a legacy `project:` + `services:` manifest into the canonical
/// `projects:` form, returning the converted YAML. A manifest that already uses
/// `projects:` (and has no legacy `project:`) is returned unchanged.
pub fn migrate_manifest(content: &str) -> Result<String, ProcessManagerError> {
    use serde_yaml::{Mapping, Value};

    let mut root: Value =
        serde_yaml::from_str(content).map_err(ProcessManagerError::ConfigParseError)?;
    let Value::Mapping(map) = &mut root else {
        return Err(ProcessManagerError::ConfigParseError(
            serde_yaml::Error::custom("manifest root must be a mapping"),
        ));
    };

    let project_key = Value::String("project".into());
    let projects_key = Value::String("projects".into());
    let services_key = Value::String("services".into());
    let name_key = Value::String("name".into());
    let id_key = Value::String("id".into());

    if map.contains_key(&projects_key) {
        return Ok(content.to_string());
    }

    let Some(project_val) = map.remove(&project_key) else {
        // No project block and no projects: nothing to migrate.
        return Ok(content.to_string());
    };

    // Derive id and optional name from the legacy `project:` block.
    let (id, name) = match project_val {
        Value::String(id) => (id, None),
        Value::Mapping(pm) => {
            let id = pm
                .get(&id_key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    ProcessManagerError::ConfigParseError(serde_yaml::Error::custom(
                        "legacy project block is missing an id",
                    ))
                })?;
            let name = pm.get(&name_key).and_then(Value::as_str).map(str::to_string);
            (id, name)
        }
        _ => {
            return Err(ProcessManagerError::ConfigParseError(
                serde_yaml::Error::custom("unrecognized project block shape"),
            ));
        }
    };

    let services = map.remove(&services_key).unwrap_or(Value::Mapping(Mapping::new()));

    let mut entry = Mapping::new();
    if let Some(name) = name {
        entry.insert(name_key, Value::String(name));
    }
    entry.insert(services_key, services);

    let mut projects = Mapping::new();
    projects.insert(Value::String(id), Value::Mapping(entry));
    map.insert(projects_key, Value::Mapping(projects));

    serde_yaml::to_string(&root).map_err(ProcessManagerError::ConfigParseError)
}

/// Loads and parses the configuration file, expanding environment variables.
pub fn load_config(config_path: Option<&str>) -> Result<Config, ProcessManagerError> {
    let config_path = config_path.map(Path::new).unwrap_or_else(|| {
        if Path::new("systemg.yaml").exists() {
            Path::new("systemg.yaml")
        } else {
            Path::new("sysg.yaml")
        }
    });

    let file = fs::File::open(config_path).map_err(|e| {
        ProcessManagerError::ConfigReadError(std::io::Error::new(
            e.kind(),
            format!("{} ({})", e, config_path.display()),
        ))
    })?;

    load_config_from_file(file, config_path)
}

/// Parses configuration from an already-open, trust-validated descriptor.
///
/// Reading from the same `File` that [`crate::runtime::open_trusted_config`]
/// validated closes the check-to-use race a stat-then-reopen sequence would
/// leave: the bytes parsed here are exactly the bytes that passed validation.
pub fn load_config_from_file(
    mut file: fs::File,
    config_path: &Path,
) -> Result<Config, ProcessManagerError> {
    use std::io::Read;

    let mut content = String::new();
    file.read_to_string(&mut content).map_err(|e| {
        ProcessManagerError::ConfigReadError(std::io::Error::new(
            e.kind(),
            format!("{} ({})", e, config_path.display()),
        ))
    })?;

    let mut config =
        parse_config_manifest(&content).map_err(ProcessManagerError::ConfigParseError)?;

    let base_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    config.project_dir = Some(base_path.to_string_lossy().to_string());
    config.project = resolve_project_config(config.project, &base_path)?;
    if let Some(env_config) = &config.env
        && let Some(resolved_path) = env_config.path(&base_path)
    {
        load_env_file(&resolved_path.to_string_lossy())?;
    }
    if let Some(env_config) = &config.env
        && let Some(vars) = &env_config.vars
    {
        for (key, value) in vars {
            unsafe {
                env::set_var(key, value);
            }
        }
    }
    for service in config.services.values_mut() {
        let merged_env = EnvConfig::merge(config.env.as_ref(), service.env.as_ref());

        if let Some(env_config) = &merged_env
            && let Some(resolved_path) = env_config.path(&base_path)
        {
            load_env_file(&resolved_path.to_string_lossy())?;
        }

        if let Some(env_config) = &merged_env
            && let Some(vars) = &env_config.vars
        {
            for (key, value) in vars {
                unsafe {
                    env::set_var(key, value);
                }
            }
        }

        service.env = merged_env;
    }

    let expanded_content = expand_env_vars(&content)?;

    let mut config = parse_config_manifest(&expanded_content)
        .map_err(ProcessManagerError::ConfigParseError)?;

    config.project_dir = Some(base_path.to_string_lossy().to_string());
    config.project = resolve_project_config(config.project, &base_path)?;
    for service in config.services.values_mut() {
        service.env = EnvConfig::merge(config.env.as_ref(), service.env.as_ref());
    }

    config.service_start_order()?;
    Ok(config)
}

/// Loads a manifest into one `Config` per project it declares, applying the same
/// env resolution and validation as [`load_config_from_file`] to each. This is
/// how one file fans out into the multiple project runtimes the supervisor holds.
pub fn load_projects_from_file(
    mut file: fs::File,
    config_path: &Path,
) -> Result<Vec<Config>, ProcessManagerError> {
    use std::io::Read;

    let mut content = String::new();
    file.read_to_string(&mut content).map_err(|e| {
        ProcessManagerError::ConfigReadError(std::io::Error::new(
            e.kind(),
            format!("{} ({})", e, config_path.display()),
        ))
    })?;

    let base_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    // First pass over the raw text applies env-file side effects so ${VAR}
    // expansion below can see them, mirroring load_config_from_file.
    for config in parse_config_projects(&content)
        .map_err(ProcessManagerError::ConfigParseError)?
    {
        apply_env_side_effects(&config, &base_path)?;
    }

    let expanded_content = expand_env_vars(&content)?;
    let configs = parse_config_projects(&expanded_content)
        .map_err(ProcessManagerError::ConfigParseError)?;

    let mut finalized = Vec::with_capacity(configs.len());
    for mut config in configs {
        config.project_dir = Some(base_path.to_string_lossy().to_string());
        config.project = resolve_project_config(config.project, &base_path)?;
        for service in config.services.values_mut() {
            service.env = EnvConfig::merge(config.env.as_ref(), service.env.as_ref());
        }
        config.service_start_order()?;
        finalized.push(config);
    }
    Ok(finalized)
}

/// Applies a config's env-file loads and inline var sets to the process
/// environment, so subsequent `${VAR}` expansion resolves against them.
fn apply_env_side_effects(
    config: &Config,
    base_path: &Path,
) -> Result<(), ProcessManagerError> {
    if let Some(env_config) = &config.env {
        if let Some(resolved_path) = env_config.path(base_path) {
            load_env_file(&resolved_path.to_string_lossy())?;
        }
        if let Some(vars) = &env_config.vars {
            for (key, value) in vars {
                unsafe {
                    env::set_var(key, value);
                }
            }
        }
    }

    for service in config.services.values() {
        let merged_env = EnvConfig::merge(config.env.as_ref(), service.env.as_ref());
        if let Some(env_config) = &merged_env {
            if let Some(resolved_path) = env_config.path(base_path) {
                load_env_file(&resolved_path.to_string_lossy())?;
            }
            if let Some(vars) = &env_config.vars {
                for (key, value) in vars {
                    unsafe {
                        env::set_var(key, value);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn projects_map_fans_out_into_one_config_per_entry() {
        let configs = parse_config_projects(
            r#"
version: "1"
projects:
  foo:
    name: "Foo"
    services:
      bar: { command: "sleep 1" }
  boo:
    services:
      shine: { command: "sleep 1" }
services:
  loose: { command: "sleep 1" }
"#,
        )
        .expect("multi-project manifest parses");

        assert_eq!(configs.len(), 3);
        let foo = configs.iter().find(|c| c.project.id == "foo").unwrap();
        assert_eq!(foo.project.name, "Foo");
        assert!(foo.services.contains_key("bar"));
        let boo = configs.iter().find(|c| c.project.id == "boo").unwrap();
        assert_eq!(boo.project.name, "boo");
        assert!(boo.services.contains_key("shine"));
        // The loose bundle has an empty (default) project id and holds `loose`.
        let loose = configs.iter().find(|c| c.project.id.is_empty()).unwrap();
        assert!(loose.services.contains_key("loose"));
    }

    #[test]
    fn legacy_single_project_still_parses_as_one_config() {
        let configs = parse_config_projects(
            r#"
version: "1"
project: { id: legacy }
services:
  worker: { command: "sleep 1" }
"#,
        )
        .expect("legacy manifest parses");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].project.id, "legacy");
        assert!(configs[0].services.contains_key("worker"));
    }

    #[test]
    fn migrate_converts_legacy_project_to_projects() {
        let converted = migrate_manifest(
            r#"
version: "1"
project:
  id: shop
  name: "Shop"
services:
  api: { command: "sleep 1" }
"#,
        )
        .expect("migration succeeds");

        // The converted text parses into exactly the shop project with api.
        let configs = parse_config_projects(&converted).expect("converted parses");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].project.id, "shop");
        assert_eq!(configs[0].project.name, "Shop");
        assert!(configs[0].services.contains_key("api"));
        assert!(converted.contains("projects:"));
        assert!(!converted.contains("\nproject:"));
    }

    #[test]
    fn migrate_leaves_projects_manifest_unchanged() {
        let src = "version: \"1\"\nprojects:\n  foo:\n    services:\n      s: { command: \"x\" }\n";
        assert_eq!(migrate_manifest(src).unwrap(), src);
    }

    #[test]
    fn project_and_projects_together_is_rejected() {
        let err = parse_config_projects(
            r#"
version: "1"
project: { id: a }
projects:
  b:
    services:
      s: { command: "sleep 1" }
"#,
        )
        .expect_err("both keys must be rejected");
        assert!(err.to_string().contains("project"));
    }

    #[test]
    fn expand_env_vars_errors_on_missing_variable() {
        let _guard = crate::test_utils::env_lock();
        unsafe {
            env::remove_var("SYSTEMG_DEFINITELY_UNSET_VAR");
        }
        let err = expand_env_vars("value: ${SYSTEMG_DEFINITELY_UNSET_VAR}")
            .expect_err("missing variable should error");
        assert!(
            matches!(err, ProcessManagerError::MissingEnvVar(name) if name == "SYSTEMG_DEFINITELY_UNSET_VAR")
        );
    }

    #[test]
    fn parse_manifest_accepts_string_version() {
        let config = parse_config_manifest(
            r#"
version: "1"
services:
  api:
    command: "echo ok"
"#,
        )
        .expect("parse manifest");

        assert_eq!(config.version, Version::V1);
        assert_eq!(config.services["api"].command, "echo ok");
    }

    #[test]
    fn parse_manifest_accepts_integer_version() {
        let config = parse_config_manifest(
            r#"
version: 1
services:
  api:
    command: "echo ok"
"#,
        )
        .expect("parse manifest");

        assert_eq!(config.version, Version::V1);
        assert_eq!(config.services["api"].command, "echo ok");
    }

    #[test]
    fn load_config_accepts_project_object() {
        let dir = tempdir().expect("tempdir");
        let yaml_path = dir.path().join("systemg.yaml");
        fs::write(
            &yaml_path,
            r#"
version: "1"
project:
  id: arbitration
  name: Arbitration
services:
  api:
    command: "echo ok"
"#,
        )
        .expect("write config");

        let config = load_config(Some(yaml_path.to_str().unwrap())).unwrap();

        assert_eq!(config.project.id, "arbitration");
        assert_eq!(config.project.name, "Arbitration");
    }

    #[test]
    fn load_config_accepts_project_shorthand() {
        let dir = tempdir().expect("tempdir");
        let yaml_path = dir.path().join("systemg.yaml");
        fs::write(
            &yaml_path,
            r#"
version: "1"
project: arbitration
services:
  api:
    command: "echo ok"
"#,
        )
        .expect("write config");

        let config = load_config(Some(yaml_path.to_str().unwrap())).unwrap();

        assert_eq!(config.project.id, "arbitration");
        assert_eq!(config.project.name, "arbitration");
    }

    #[test]
    fn load_config_migrates_missing_project_to_legacy_identity() {
        let dir = tempdir().expect("tempdir");
        let yaml_path = dir.path().join("systemg.yaml");
        fs::write(
            &yaml_path,
            r#"
version: "1"
services:
  api:
    command: "echo ok"
"#,
        )
        .expect("write config");

        let config = load_config(Some(yaml_path.to_str().unwrap())).unwrap();

        assert!(config.project.id.starts_with("legacy-"));
        assert_eq!(
            config.project.name,
            dir.path().file_name().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn parse_manifest_rejects_missing_version() {
        let err = parse_config_manifest(
            r#"
services:
  api:
    command: "echo ok"
"#,
        )
        .expect_err("missing version should fail");

        assert!(err.to_string().contains("missing field `version`"));
    }

    #[test]
    fn parse_manifest_rejects_unsupported_version() {
        let err = parse_config_manifest(
            r#"
version: "2"
services:
  api:
    command: "echo ok"
"#,
        )
        .expect_err("unsupported version should fail");

        assert!(
            err.to_string()
                .contains("unsupported manifest version '2'; supported versions: 1"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn migrates_v1_manifest_to_current_config() {
        let mut services = HashMap::new();
        services.insert("api".into(), minimal_service(None));

        let current = Config::try_from(ConfigV1 {
            version: Version::V1,
            project: Some(ProjectConfigInput::Fields {
                id: "systemg".into(),
                name: Some("Systemg".into()),
            }),
            projects: None,
            services,
            project_dir: Some("/tmp/systemg".into()),
            env: Some(EnvConfig {
                file: Some(".env".into()),
                vars: Some(HashMap::from([("RUST_LOG".into(), "debug".into())])),
                clear_session_vars: None,
                strip: None,
                inherit_env: None,
            }),
            metrics: MetricsConfig {
                retention_minutes: 30,
                ..MetricsConfig::default()
            },
            logs: LogsConfig {
                sink: Some(LogSink::None),
                ..LogsConfig::default()
            },
            status: StatusConfig {
                snapshot_mode: StatusSnapshotMode::Detailed,
                snapshot_interval_secs: 15,
            },
        })
        .expect("migrate v1 config");

        assert_eq!(current.version, CURRENT_MANIFEST_VERSION);
        assert_eq!(current.project.id, "systemg");
        assert_eq!(current.project.name, "Systemg");
        assert_eq!(current.project_dir.as_deref(), Some("/tmp/systemg"));
        assert_eq!(current.services["api"].command, "echo ok");
        assert_eq!(current.metrics.retention_minutes, 30);
        assert_eq!(current.logs.sink, Some(LogSink::None));
        assert_eq!(current.status.snapshot_mode, StatusSnapshotMode::Detailed);
    }

    #[test]
    fn test_load_env_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join(".env");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "TEST_KEY=TEST_VALUE").unwrap();
        writeln!(file, "ANOTHER_KEY=ANOTHER_VALUE").unwrap();

        load_env_file(file_path.to_str().unwrap()).unwrap();

        assert_eq!(env::var("TEST_KEY").unwrap(), "TEST_VALUE");
        assert_eq!(env::var("ANOTHER_KEY").unwrap(), "ANOTHER_VALUE");
    }

    #[test]
    fn test_load_config_with_absolute_env_path() {
        let dir = tempdir().unwrap();
        let env_path = dir.path().join("absolute.env");
        let mut env_file = File::create(&env_path).unwrap();
        writeln!(env_file, "MY_TEST_VAR=HelloWorld").unwrap();

        let yaml_path = dir.path().join("config.yaml");
        let mut yaml_file = File::create(&yaml_path).unwrap();
        writeln!(
            yaml_file,
            r#"
        version: "1"
        services:
          service1:
            command: "echo ${{MY_TEST_VAR}}"
            env:
              file: "{}"
              vars:
                TEST: "test"
        "#,
            env_path.to_str().unwrap()
        )
        .unwrap();

        let config = load_config(Some(yaml_path.to_str().unwrap())).unwrap();
        let base_path = Path::new(config.project_dir.as_ref().unwrap());
        let service = &config.services["service1"];

        let resolved = service.env.as_ref().unwrap().path(base_path).unwrap();
        assert_eq!(resolved, env_path);
        assert!(resolved.is_absolute());
    }

    #[test]
    fn test_load_config_with_relative_env_path() {
        let dir = tempdir().unwrap();
        let env_path = dir.path().join("relative.env");
        let mut env_file = File::create(&env_path).unwrap();
        writeln!(env_file, "REL_VAR=42").unwrap();

        let yaml_path = dir.path().join("systemg.yaml");
        let mut yaml_file = File::create(&yaml_path).unwrap();
        writeln!(
            yaml_file,
            r#"
version: "1"
services:
  rel_service:
    command: "echo ${{REL_VAR}}"
    env:
      file: "relative.env"
      vars:
        DB: "local"
"#
        )
        .unwrap();

        let config = load_config(Some(yaml_path.to_str().unwrap())).unwrap();
        let service = &config.services["rel_service"];
        let base_path = Path::new(config.project_dir.as_ref().unwrap());
        assert_eq!(
            service.env.as_ref().unwrap().path(base_path).unwrap(),
            env_path
        );
    }

    fn minimal_service(depends_on: Option<Vec<&str>>) -> ServiceConfig {
        ServiceConfig {
            command: "echo ok".into(),
            env: None,
            user: None,
            group: None,
            supplementary_groups: None,
            limits: None,
            capabilities: None,
            isolation: None,
            restart_policy: None,
            backoff: None,
            max_restarts: None,
            depends_on: depends_on
                .map(|deps| deps.into_iter().map(DependsOn::from).collect()),
            deployment: None,
            hooks: None,
            cron: None,
            skip: None,
            spawn: None,
            logs: None,
            project_scope: None,
        }
    }

    #[test]
    fn service_start_order_resolves_dependencies() {
        let mut services = HashMap::new();
        services.insert("a".into(), minimal_service(None));
        services.insert("b".into(), minimal_service(Some(vec!["a"])));
        services.insert("c".into(), minimal_service(Some(vec!["b"])));

        let config = Config {
            version: Version::V1,
            project: ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        let order = config.service_start_order().unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn service_start_order_unknown_dependency_error() {
        let mut services = HashMap::new();
        services.insert("a".into(), minimal_service(Some(vec!["missing"])));

        let config = Config {
            version: Version::V1,
            project: ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        match config.service_start_order() {
            Err(ProcessManagerError::UnknownDependency {
                service,
                dependency,
            }) => {
                assert_eq!(service, "a");
                assert_eq!(dependency, "missing");
            }
            other => panic!("expected unknown dependency error, got {other:?}"),
        }
    }

    #[test]
    fn service_start_order_cycle_error() {
        let mut services = HashMap::new();
        services.insert("a".into(), minimal_service(Some(vec!["b"])));
        services.insert("b".into(), minimal_service(Some(vec!["a"])));

        let config = Config {
            version: Version::V1,
            project: ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        match config.service_start_order() {
            Err(ProcessManagerError::DependencyCycle { cycle }) => {
                assert!(cycle.contains("a"));
                assert!(cycle.contains("b"));
            }
            other => panic!("expected dependency cycle error, got {other:?}"),
        }
    }

    #[test]
    fn logs_config_defaults_to_file_with_rotation() {
        let config: Config = serde_yaml::from_str(
            r#"
version: "1"
services:
  api:
    command: "echo ok"
"#,
        )
        .unwrap();

        let service = &config.services["api"];
        let logs = service.effective_logs(&config.logs);
        assert_eq!(logs.sink, LogSink::File);
        assert_eq!(logs.max_bytes, LOGS_DEFAULT_MAX_BYTES);
        assert_eq!(logs.max_files, LOGS_DEFAULT_MAX_FILES);
    }

    #[test]
    fn service_logs_override_global_logs_config() {
        let config: Config = serde_yaml::from_str(
            r#"
version: "1"
logs:
  sink: file
  max_bytes: 2048
  max_files: 4
services:
  api:
    command: "echo ok"
    logs:
      sink: none
      max_files: 0
"#,
        )
        .unwrap();

        let service = &config.services["api"];
        let logs = service.effective_logs(&config.logs);
        assert_eq!(logs.sink, LogSink::None);
        assert_eq!(logs.max_bytes, 2048);
        assert_eq!(logs.max_files, 0);
    }

    #[test]
    fn logs_config_rejects_unknown_sink() {
        let err = serde_yaml::from_str::<Config>(
            r#"
version: "1"
logs:
  sink: journald
services:
  api:
    command: "echo ok"
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("journald"));
    }

    #[test]
    fn status_config_defaults_to_summary_snapshots() {
        let config: Config = serde_yaml::from_str(
            r#"
version: "1"
services:
  api:
    command: "echo ok"
"#,
        )
        .unwrap();

        assert_eq!(config.status.snapshot_mode, StatusSnapshotMode::Summary);
        assert_eq!(config.status.snapshot_interval_secs, 5);
        assert_eq!(config.status.snapshot_interval(), Duration::from_secs(5));
    }

    #[test]
    fn status_config_parses_detailed_mode_and_clamps_interval() {
        let config: Config = serde_yaml::from_str(
            r#"
version: "1"
status:
  snapshot_mode: detailed
  snapshot_interval_secs: 0
services:
  api:
    command: "echo ok"
"#,
        )
        .unwrap();

        assert_eq!(config.status.snapshot_mode, StatusSnapshotMode::Detailed);
        assert_eq!(config.status.snapshot_interval(), Duration::from_secs(1));
    }

    #[test]
    fn status_config_rejects_unknown_snapshot_mode() {
        let err = serde_yaml::from_str::<Config>(
            r#"
version: "1"
status:
  snapshot_mode: deep
services:
  api:
    command: "echo ok"
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("deep"));
    }

    #[test]
    fn test_env_merge_both_none() {
        let result = EnvConfig::merge(None, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_env_merge_root_only() {
        let root = EnvConfig {
            file: Some("root.env".into()),
            vars: Some(HashMap::from([("ROOT_VAR".into(), "root_value".into())])),
            clear_session_vars: None,
            strip: None,
            inherit_env: None,
        };

        let result = EnvConfig::merge(Some(&root), None).unwrap();
        assert_eq!(result.file, Some("root.env".into()));
        assert_eq!(
            result.vars.as_ref().unwrap().get("ROOT_VAR"),
            Some(&"root_value".to_string())
        );
    }

    #[test]
    fn test_env_merge_service_only() {
        let service = EnvConfig {
            file: Some("service.env".into()),
            vars: Some(HashMap::from([(
                "SERVICE_VAR".into(),
                "service_value".into(),
            )])),
            clear_session_vars: None,
            strip: None,
            inherit_env: None,
        };

        let result = EnvConfig::merge(None, Some(&service)).unwrap();
        assert_eq!(result.file, Some("service.env".into()));
        assert_eq!(
            result.vars.as_ref().unwrap().get("SERVICE_VAR"),
            Some(&"service_value".to_string())
        );
    }

    #[test]
    fn test_env_merge_service_overrides_root() {
        let root = EnvConfig {
            file: Some("root.env".into()),
            vars: Some(HashMap::from([
                ("SHARED_VAR".into(), "root_value".into()),
                ("ROOT_ONLY".into(), "root_only_value".into()),
            ])),
            clear_session_vars: None,
            strip: None,
            inherit_env: None,
        };

        let service = EnvConfig {
            file: Some("service.env".into()),
            vars: Some(HashMap::from([
                ("SHARED_VAR".into(), "service_value".into()),
                ("SERVICE_ONLY".into(), "service_only_value".into()),
            ])),
            clear_session_vars: None,
            strip: None,
            inherit_env: None,
        };

        let result = EnvConfig::merge(Some(&root), Some(&service)).unwrap();
        assert_eq!(result.file, Some("service.env".into()));
        let vars = result.vars.unwrap();
        assert_eq!(vars.get("SHARED_VAR"), Some(&"service_value".to_string()));
        assert_eq!(vars.get("ROOT_ONLY"), Some(&"root_only_value".to_string()));
        assert_eq!(
            vars.get("SERVICE_ONLY"),
            Some(&"service_only_value".to_string())
        );
    }

    #[test]
    fn vars_to_strip_defaults_to_session_vars() {
        let env = EnvConfig {
            file: None,
            vars: None,
            clear_session_vars: None,
            strip: None,
            inherit_env: None,
        };
        let stripped = env.vars_to_strip();
        for var in crate::constants::SESSION_SCOPED_ENV_VARS {
            assert!(stripped.contains(&var.to_string()));
        }
    }

    #[test]
    fn vars_to_strip_preserves_explicit_vars() {
        let env = EnvConfig {
            file: None,
            vars: Some(HashMap::from([("SSH_TTY".into(), "/dev/pts/0".into())])),
            clear_session_vars: None,
            strip: None,
            inherit_env: None,
        };
        assert!(!env.vars_to_strip().contains(&"SSH_TTY".to_string()));
    }

    #[test]
    fn vars_to_strip_respects_clear_session_vars_false() {
        let env = EnvConfig {
            file: None,
            vars: None,
            clear_session_vars: Some(false),
            strip: Some(vec!["FOO".into()]),
            inherit_env: None,
        };
        let stripped = env.vars_to_strip();
        assert_eq!(stripped, vec!["FOO".to_string()]);
    }

    #[test]
    fn inherit_env_parses_and_defaults_to_none() {
        let env: EnvConfig = serde_yaml::from_str("inherit_env: true\n").unwrap();
        assert_eq!(env.inherit_env, Some(true));

        let default: EnvConfig = serde_yaml::from_str("FOO: bar\n").unwrap();
        assert_eq!(default.inherit_env, None);
    }

    #[test]
    fn merge_prefers_service_inherit_env_then_root() {
        let root = EnvConfig {
            inherit_env: Some(true),
            ..Default::default()
        };
        let service = EnvConfig {
            inherit_env: Some(false),
            ..Default::default()
        };
        let merged = EnvConfig::merge(Some(&root), Some(&service)).unwrap();
        assert_eq!(merged.inherit_env, Some(false));

        let service_unset = EnvConfig::default();
        let merged = EnvConfig::merge(Some(&root), Some(&service_unset)).unwrap();
        assert_eq!(merged.inherit_env, Some(true));
    }

    #[test]
    fn test_env_merge_service_file_only_overrides_root() {
        let root = EnvConfig {
            file: Some("root.env".into()),
            vars: Some(HashMap::from([("ROOT_VAR".into(), "root_value".into())])),
            clear_session_vars: None,
            strip: None,
            inherit_env: None,
        };

        let service = EnvConfig {
            file: Some("service.env".into()),
            vars: None,
            clear_session_vars: None,
            strip: None,
            inherit_env: None,
        };

        let result = EnvConfig::merge(Some(&root), Some(&service)).unwrap();
        assert_eq!(result.file, Some("service.env".into()));
        let vars = result.vars.unwrap();
        assert_eq!(vars.get("ROOT_VAR"), Some(&"root_value".to_string()));
    }

    #[test]
    fn test_env_config_deserializes_direct_inline_vars() {
        let env: EnvConfig = serde_yaml::from_str(
            r#"
file: ".env"
RUST_LOG: "debug"
ESPER_ENGINE_SERVICE_URL: "http://127.0.0.1:4100"
"#,
        )
        .unwrap();

        assert_eq!(env.file.as_deref(), Some(".env"));
        let vars = env.vars.unwrap();
        assert_eq!(vars.get("RUST_LOG"), Some(&"debug".to_string()));
        assert_eq!(
            vars.get("ESPER_ENGINE_SERVICE_URL"),
            Some(&"http://127.0.0.1:4100".to_string())
        );
    }

    #[test]
    fn test_env_config_deserializes_nested_and_direct_vars() {
        let env: EnvConfig = serde_yaml::from_str(
            r#"
file: ".env"
vars:
  POSTGRES_URI: "postgres://localhost/db"
RUST_LOG: "debug"
"#,
        )
        .unwrap();

        assert_eq!(env.file.as_deref(), Some(".env"));
        let vars = env.vars.unwrap();
        assert_eq!(
            vars.get("POSTGRES_URI"),
            Some(&"postgres://localhost/db".to_string())
        );
        assert_eq!(vars.get("RUST_LOG"), Some(&"debug".to_string()));
    }

    #[test]
    fn test_load_config_with_root_env() {
        let dir = tempdir().unwrap();
        let root_env_path = dir.path().join("root.env");
        let mut root_env_file = File::create(&root_env_path).unwrap();
        writeln!(root_env_file, "ROOT_VAR=from_root_file").unwrap();

        let yaml_path = dir.path().join("systemg.yaml");
        let mut yaml_file = File::create(&yaml_path).unwrap();
        writeln!(
            yaml_file,
            r#"
version: "1"
env:
  file: "root.env"
  vars:
    GLOBAL_VAR: "global_value"
services:
  service1:
    command: "echo ${{ROOT_VAR}} ${{GLOBAL_VAR}}"
  service2:
    command: "echo ${{ROOT_VAR}} ${{GLOBAL_VAR}}"
"#
        )
        .unwrap();

        let config = load_config(Some(yaml_path.to_str().unwrap())).unwrap();
        for service_name in ["service1", "service2"] {
            let service = &config.services[service_name];
            let env = service.env.as_ref().unwrap();
            let vars = env.vars.as_ref().unwrap();
            assert_eq!(vars.get("GLOBAL_VAR"), Some(&"global_value".to_string()));
        }
    }

    #[test]
    fn test_load_config_with_direct_service_env_vars() {
        let dir = tempdir().unwrap();
        let yaml_path = dir.path().join("systemg.yaml");
        let mut yaml_file = File::create(&yaml_path).unwrap();
        writeln!(
            yaml_file,
            r#"
version: "1"
services:
  service1:
    command: "echo ok"
    env:
      RUST_LOG: "debug"
      API_URL: "http://127.0.0.1:4100"
"#
        )
        .unwrap();

        let config = load_config(Some(yaml_path.to_str().unwrap())).unwrap();
        let service = &config.services["service1"];
        let env = service.env.as_ref().unwrap();
        let vars = env.vars.as_ref().unwrap();
        assert_eq!(vars.get("RUST_LOG"), Some(&"debug".to_string()));
        assert_eq!(
            vars.get("API_URL"),
            Some(&"http://127.0.0.1:4100".to_string())
        );
    }

    #[test]
    fn test_load_config_merges_root_and_service_direct_env_vars() {
        let dir = tempdir().unwrap();
        let yaml_path = dir.path().join("systemg.yaml");
        let mut yaml_file = File::create(&yaml_path).unwrap();
        writeln!(
            yaml_file,
            r#"
version: "1"
env:
  REDIS_URI: "redis://127.0.0.1:6379"
services:
  service1:
    command: "echo ok"
    env:
      RUST_LOG: "debug"
"#
        )
        .unwrap();

        let config = load_config(Some(yaml_path.to_str().unwrap())).unwrap();
        let service = &config.services["service1"];
        let env = service.env.as_ref().unwrap();
        let vars = env.vars.as_ref().unwrap();
        assert_eq!(
            vars.get("REDIS_URI"),
            Some(&"redis://127.0.0.1:6379".to_string())
        );
        assert_eq!(vars.get("RUST_LOG"), Some(&"debug".to_string()));
    }

    #[test]
    fn test_load_config_service_env_overrides_root() {
        let dir = tempdir().unwrap();
        let root_env_path = dir.path().join("root.env");
        let mut root_env_file = File::create(&root_env_path).unwrap();
        writeln!(root_env_file, "ROOT_FILE_VAR=root").unwrap();

        let service_env_path = dir.path().join("service.env");
        let mut service_env_file = File::create(&service_env_path).unwrap();
        writeln!(service_env_file, "SERVICE_FILE_VAR=service").unwrap();

        let yaml_path = dir.path().join("systemg.yaml");
        let mut yaml_file = File::create(&yaml_path).unwrap();
        writeln!(
            yaml_file,
            r#"
version: "1"
env:
  file: "root.env"
  vars:
    SHARED: "root_value"
    ROOT_ONLY: "root"
services:
  service1:
    command: "echo test"
    env:
      file: "service.env"
      vars:
        SHARED: "service_value"
        SERVICE_ONLY: "service"
  service2:
    command: "echo test"
"#
        )
        .unwrap();

        let config = load_config(Some(yaml_path.to_str().unwrap())).unwrap();
        let service1 = &config.services["service1"];
        let env1 = service1.env.as_ref().unwrap();
        assert_eq!(env1.file, Some("service.env".into()));
        let vars1 = env1.vars.as_ref().unwrap();
        assert_eq!(vars1.get("SHARED"), Some(&"service_value".to_string()));
        assert_eq!(vars1.get("ROOT_ONLY"), Some(&"root".to_string()));
        assert_eq!(vars1.get("SERVICE_ONLY"), Some(&"service".to_string()));
        let service2 = &config.services["service2"];
        let env2 = service2.env.as_ref().unwrap();
        assert_eq!(env2.file, Some("root.env".into()));
        let vars2 = env2.vars.as_ref().unwrap();
        assert_eq!(vars2.get("SHARED"), Some(&"root_value".to_string()));
        assert_eq!(vars2.get("ROOT_ONLY"), Some(&"root".to_string()));
        assert!(vars2.get("SERVICE_ONLY").is_none());
    }

    #[test]
    fn load_config_parses_blue_green_deployment_block() {
        let dir = tempdir().expect("tempdir");
        let yaml_path = dir.path().join("systemg.yaml");
        let mut yaml_file = File::create(&yaml_path).expect("create yaml");
        writeln!(
            yaml_file,
            r#"
version: "1"
services:
  web:
    command: "python app.py"
    deployment:
      strategy: "rolling"
      blue_green:
        env_var: "PORT"
        slots: ["8000", "8001"]
        switch_command: "echo switch"
        candidate_health_check:
          url: "http://127.0.0.1:{{slot}}/health"
          interval: "1s"
        switch_verify:
          command: "test -f /tmp/api-ready"
        state_path: ".state/web-slot.json"
"#
        )
        .expect("write yaml");

        let config = load_config(Some(yaml_path.to_str().expect("yaml path")))
            .expect("load config");
        let deployment = config
            .services
            .get("web")
            .expect("web service")
            .deployment
            .as_ref()
            .expect("deployment");
        let blue_green = deployment.blue_green.as_ref().expect("blue_green");

        assert_eq!(deployment.strategy.as_deref(), Some("rolling"));
        assert_eq!(blue_green.env_var.as_deref(), Some("PORT"));
        assert_eq!(blue_green.slots, vec!["8000", "8001"]);
        assert_eq!(
            blue_green
                .candidate_health_check
                .as_ref()
                .and_then(|check| check.url.as_deref()),
            Some("http://127.0.0.1:{slot}/health")
        );
        assert_eq!(
            blue_green
                .candidate_health_check
                .as_ref()
                .and_then(|check| check.interval.as_deref()),
            Some("1s")
        );
        assert_eq!(
            blue_green
                .switch_verify
                .as_ref()
                .and_then(|check| check.command.as_deref()),
            Some("test -f /tmp/api-ready")
        );
    }

    #[test]
    fn load_config_rejects_health_check_without_url_or_command() {
        let dir = tempdir().expect("tempdir");
        let yaml_path = dir.path().join("systemg.yaml");
        let mut yaml_file = File::create(&yaml_path).expect("create yaml");
        writeln!(
            yaml_file,
            r#"
version: "1"
services:
  web:
    command: "python app.py"
    deployment:
      strategy: "rolling"
      health_check:
        timeout: "30s"
"#
        )
        .expect("write yaml");

        let err = load_config(Some(yaml_path.to_str().expect("yaml path")))
            .expect_err("health check should fail validation");

        assert!(
            err.to_string()
                .contains("health check requires at least one of 'url' or 'command'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn hash_computation_is_stable() {
        let config1 = ServiceConfig {
            command: "test command".to_string(),
            env: None,
            user: None,
            group: None,
            supplementary_groups: None,
            limits: None,
            capabilities: None,
            isolation: None,
            restart_policy: Some("always".to_string()),
            backoff: Some("5s".to_string()),
            max_restarts: Some(3),
            depends_on: None,
            deployment: None,
            hooks: None,
            cron: Some(CronConfig {
                expression: "0 * * * * *".to_string(),
                timezone: Some("UTC".to_string()),
            }),
            skip: None,
            spawn: None,
            logs: None,
            project_scope: None,
        };

        let config2 = ServiceConfig {
            command: "test command".to_string(),
            env: None,
            user: None,
            group: None,
            supplementary_groups: None,
            limits: None,
            capabilities: None,
            isolation: None,
            restart_policy: Some("always".to_string()),
            backoff: Some("5s".to_string()),
            max_restarts: Some(3),
            depends_on: None,
            deployment: None,
            hooks: None,
            cron: Some(CronConfig {
                expression: "0 * * * * *".to_string(),
                timezone: Some("UTC".to_string()),
            }),
            skip: None,
            spawn: None,
            logs: None,
            project_scope: None,
        };

        let hash1 = config1.compute_hash();
        let hash2 = config2.compute_hash();

        assert_eq!(
            hash1, hash2,
            "Identical configs should produce identical hashes"
        );
        assert_eq!(hash1.len(), 16, "Hash should be 16 characters");
    }

    #[test]
    fn hash_changes_with_config_changes() {
        let base_config = ServiceConfig {
            command: "test command".to_string(),
            env: None,
            user: None,
            group: None,
            supplementary_groups: None,
            limits: None,
            capabilities: None,
            isolation: None,
            restart_policy: None,
            backoff: None,
            max_restarts: None,
            depends_on: None,
            deployment: None,
            hooks: None,
            cron: None,
            skip: None,
            spawn: None,
            logs: None,
            project_scope: None,
        };

        let modified_command = ServiceConfig {
            command: "different command".to_string(),
            ..base_config.clone()
        };

        let modified_cron = ServiceConfig {
            cron: Some(CronConfig {
                expression: "*/5 * * * * *".to_string(),
                timezone: None,
            }),
            ..base_config.clone()
        };
        let modified_logs = ServiceConfig {
            logs: Some(LogsConfig {
                sink: Some(LogSink::None),
                ..LogsConfig::default()
            }),
            ..base_config.clone()
        };

        let base_hash = base_config.compute_hash();
        let command_hash = modified_command.compute_hash();
        let cron_hash = modified_cron.compute_hash();
        let logs_hash = modified_logs.compute_hash();

        assert_ne!(
            base_hash, command_hash,
            "Changing command should change hash"
        );
        assert_ne!(base_hash, cron_hash, "Adding cron should change hash");
        assert_ne!(
            base_hash, logs_hash,
            "Changing service logs should change hash"
        );
        assert_ne!(
            command_hash, cron_hash,
            "Different changes should produce different hashes"
        );
    }

    #[test]
    fn service_rename_preserves_hash() {
        let config = ServiceConfig {
            command: "echo hello".to_string(),
            env: None,
            user: None,
            group: None,
            supplementary_groups: None,
            limits: None,
            capabilities: None,
            isolation: None,
            restart_policy: Some("always".to_string()),
            backoff: None,
            max_restarts: None,
            depends_on: None,
            deployment: None,
            hooks: None,
            cron: Some(CronConfig {
                expression: "0 * * * * *".to_string(),
                timezone: Some("UTC".to_string()),
            }),
            skip: None,
            spawn: None,
            logs: None,
            project_scope: None,
        };
        let hash = config.compute_hash();
        assert_eq!(hash.len(), 16);

        let renamed_config = config.clone();
        let renamed_hash = renamed_config.compute_hash();

        assert_eq!(
            hash, renamed_hash,
            "Hash should be the same after 'renaming' (using same config)"
        );
    }

    #[test]
    fn parse_limit_accepts_suffixes() {
        let kib = parse_limit("4K").expect("parse 4K");
        assert_eq!(kib, 4 * 1024);

        let mib = parse_limit("512M").expect("parse 512M");
        assert_eq!(mib, 512 * 1024 * 1024);

        let gib = parse_limit("1G").expect("parse 1G");
        assert_eq!(gib, 1024 * 1024 * 1024);

        let plain = parse_limit("1_000").expect("parse underscores");
        assert_eq!(plain, 1_000);
    }

    #[test]
    fn parse_limit_rejects_invalid_strings() {
        match parse_limit("ten") {
            Err(LimitParseError::Invalid(msg)) => assert_eq!(msg, "ten"),
            other => panic!("expected invalid error, got {other:?}"),
        }
    }
}
