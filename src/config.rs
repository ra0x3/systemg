//! Configuration management for Systemg.
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};
use strum_macros::AsRefStr;

use crate::error::ProcessManagerError;
use crate::metrics::{MetricsSettings, SpilloverSettings};

/// Represents the structure of the configuration file.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// Configuration version.
    pub version: String,
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
}
const METRICS_DEFAULT_RETENTION_MINUTES: u64 = 720; // 12 hours
const METRICS_DEFAULT_SAMPLE_INTERVAL_SECS: u64 = 1;
const METRICS_DEFAULT_MAX_MEMORY_BYTES: usize = 10 * 1024 * 1024;
const METRICS_DEFAULT_SPILLOVER_SEGMENT_BYTES: u64 = 256 * 1024;

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
    pub depends_on: Option<Vec<String>>,
    /// Deployment strategy configuration.
    pub deployment: Option<DeploymentConfig>,
    /// Hooks for lifecycle events (e.g., on_start, on_error).
    pub hooks: Option<Hooks>,
    /// Cron configuration for scheduled service execution.
    pub cron: Option<CronConfig>,
    /// Optional skip configuration that determines if the service should be skipped.
    pub skip: Option<SkipConfig>,
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
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct LimitVisitor;

        impl<'de> serde::de::Visitor<'de> for LimitVisitor {
            type Value = LimitValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a non-negative integer, an optional size suffix (e.g. 512M), or 'unlimited'",
                )
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(LimitValue::Fixed(value))
            }

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
enum LimitParseError {
    Unlimited,
    Invalid(String),
}

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
    /// Computes a stable hash of this service configuration, excluding the service name.
    /// This hash is used to identify the service state across renames.
    ///
    /// # Returns
    /// A 16-character hexadecimal string representing the first 64 bits of the SHA256 hash.
    pub fn compute_hash(&self) -> String {
        // Serialize the config to JSON with sorted keys for deterministic output
        let json = serde_json::to_string(self)
            .expect("ServiceConfig should always be serializable");

        // Compute SHA256 hash
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        let result = hasher.finalize();

        // Take first 64 bits (8 bytes) and convert to hex
        // This gives us a 16-character hash which is sufficient for uniqueness
        // while being more readable than the full 64-character SHA256
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
}

/// Health check configuration used during rolling deployments.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct HealthCheckConfig {
    /// Health check URL.
    pub url: String,
    /// Health check timeout duration (e.g., "30s").
    pub timeout: Option<String>,
    /// Number of retries before giving up.
    pub retries: Option<u32>,
}

/// Represents environment variables for a service.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct EnvConfig {
    /// Optional path to an environment file.
    pub file: Option<String>,
    /// Key-value pairs of environment variables.
    pub vars: Option<HashMap<String, String>>,
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

                // Service-level vars override root-level vars
                if let Some(service_vars) = &service_cfg.vars {
                    merged_vars.extend(service_vars.clone());
                }

                // Service-level file takes precedence over root-level file
                let file = service_cfg.file.clone().or_else(|| root_cfg.file.clone());

                Some(EnvConfig {
                    file,
                    vars: if merged_vars.is_empty() {
                        None
                    } else {
                        Some(merged_vars)
                    },
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
                    if !self.services.contains_key(dep) {
                        return Err(ProcessManagerError::UnknownDependency {
                            service: service.clone(),
                            dependency: dep.clone(),
                        });
                    }
                    *indegree.get_mut(service).expect("service must exist") += 1;
                    graph.entry(dep.clone()).or_default().push(service.clone());
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
                    map.entry(dep.clone()).or_default().push(service.clone());
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
    let result = re.replace_all(input, |caps: &regex::Captures| {
        let var_name = &caps[1];
        match env::var(var_name) {
            Ok(value) => value,
            Err(_) => panic!("Missing environment variable: {var_name}"),
        }
    });
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

/// Loads and parses the configuration file, expanding environment variables.
pub fn load_config(config_path: Option<&str>) -> Result<Config, ProcessManagerError> {
    let config_path = config_path.map(Path::new).unwrap_or_else(|| {
        if Path::new("systemg.yaml").exists() {
            Path::new("systemg.yaml")
        } else {
            Path::new("sysg.yaml")
        }
    });

    let content = fs::read_to_string(config_path).map_err(|e| {
        ProcessManagerError::ConfigReadError(std::io::Error::new(
            e.kind(),
            format!("{} ({})", e, config_path.display()),
        ))
    })?;

    let mut config: Config =
        serde_yaml::from_str(&content).map_err(ProcessManagerError::ConfigParseError)?;

    let base_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    config.project_dir = Some(base_path.to_string_lossy().to_string());

    // Load root-level env file if present
    if let Some(env_config) = &config.env
        && let Some(resolved_path) = env_config.path(&base_path)
    {
        load_env_file(&resolved_path.to_string_lossy())?;
    }

    // Load root-level env vars if present
    if let Some(env_config) = &config.env
        && let Some(vars) = &env_config.vars
    {
        for (key, value) in vars {
            unsafe {
                env::set_var(key, value);
            }
        }
    }

    // Merge root-level env with service-level env and load service-specific env
    for service in config.services.values_mut() {
        // Merge root env with service env (service takes precedence)
        let merged_env = EnvConfig::merge(config.env.as_ref(), service.env.as_ref());

        if let Some(env_config) = &merged_env
            && let Some(resolved_path) = env_config.path(&base_path)
        {
            load_env_file(&resolved_path.to_string_lossy())?;
        }

        if let Some(env_config) = &merged_env
            && let Some(vars) = &env_config.vars
        {
            // Inline environment variables take precedence over values loaded from env files
            // when expanding the YAML template.
            for (key, value) in vars {
                unsafe {
                    env::set_var(key, value);
                }
            }
        }

        service.env = merged_env;
    }

    let expanded_content = expand_env_vars(&content)?;

    let mut config: Config = serde_yaml::from_str(&expanded_content)
        .map_err(ProcessManagerError::ConfigParseError)?;

    config.project_dir = Some(base_path.to_string_lossy().to_string());

    // Apply the env merge again after re-parsing
    for service in config.services.values_mut() {
        service.env = EnvConfig::merge(config.env.as_ref(), service.env.as_ref());
    }

    config.service_start_order()?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

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
                .map(|deps| deps.into_iter().map(String::from).collect()),
            deployment: None,
            hooks: None,
            cron: None,
            skip: None,
        }
    }

    #[test]
    fn service_start_order_resolves_dependencies() {
        let mut services = HashMap::new();
        services.insert("a".into(), minimal_service(None));
        services.insert("b".into(), minimal_service(Some(vec!["a"])));
        services.insert("c".into(), minimal_service(Some(vec!["b"])));

        let config = Config {
            version: "1".into(),
            services,
            project_dir: None,
            env: None,
            metrics: MetricsConfig::default(),
        };

        let order = config.service_start_order().unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn service_start_order_unknown_dependency_error() {
        let mut services = HashMap::new();
        services.insert("a".into(), minimal_service(Some(vec!["missing"])));

        let config = Config {
            version: "1".into(),
            services,
            project_dir: None,
            env: None,
            metrics: MetricsConfig::default(),
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
            version: "1".into(),
            services,
            project_dir: None,
            env: None,
            metrics: MetricsConfig::default(),
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
    fn test_env_merge_both_none() {
        let result = EnvConfig::merge(None, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_env_merge_root_only() {
        let root = EnvConfig {
            file: Some("root.env".into()),
            vars: Some(HashMap::from([("ROOT_VAR".into(), "root_value".into())])),
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
        };

        let service = EnvConfig {
            file: Some("service.env".into()),
            vars: Some(HashMap::from([
                ("SHARED_VAR".into(), "service_value".into()),
                ("SERVICE_ONLY".into(), "service_only_value".into()),
            ])),
        };

        let result = EnvConfig::merge(Some(&root), Some(&service)).unwrap();

        // Service file should take precedence
        assert_eq!(result.file, Some("service.env".into()));

        // Service vars should override root vars
        let vars = result.vars.unwrap();
        assert_eq!(vars.get("SHARED_VAR"), Some(&"service_value".to_string()));
        assert_eq!(vars.get("ROOT_ONLY"), Some(&"root_only_value".to_string()));
        assert_eq!(
            vars.get("SERVICE_ONLY"),
            Some(&"service_only_value".to_string())
        );
    }

    #[test]
    fn test_env_merge_service_file_only_overrides_root() {
        let root = EnvConfig {
            file: Some("root.env".into()),
            vars: Some(HashMap::from([("ROOT_VAR".into(), "root_value".into())])),
        };

        let service = EnvConfig {
            file: Some("service.env".into()),
            vars: None,
        };

        let result = EnvConfig::merge(Some(&root), Some(&service)).unwrap();

        // Service file should take precedence
        assert_eq!(result.file, Some("service.env".into()));

        // Root vars should be preserved
        let vars = result.vars.unwrap();
        assert_eq!(vars.get("ROOT_VAR"), Some(&"root_value".to_string()));
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

        // Both services should have the root env
        for service_name in ["service1", "service2"] {
            let service = &config.services[service_name];
            let env = service.env.as_ref().unwrap();
            let vars = env.vars.as_ref().unwrap();
            assert_eq!(vars.get("GLOBAL_VAR"), Some(&"global_value".to_string()));
        }
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

        // Service1 should have merged env with service overrides
        let service1 = &config.services["service1"];
        let env1 = service1.env.as_ref().unwrap();
        assert_eq!(env1.file, Some("service.env".into()));
        let vars1 = env1.vars.as_ref().unwrap();
        assert_eq!(vars1.get("SHARED"), Some(&"service_value".to_string()));
        assert_eq!(vars1.get("ROOT_ONLY"), Some(&"root".to_string()));
        assert_eq!(vars1.get("SERVICE_ONLY"), Some(&"service".to_string()));

        // Service2 should have only root env
        let service2 = &config.services["service2"];
        let env2 = service2.env.as_ref().unwrap();
        assert_eq!(env2.file, Some("root.env".into()));
        let vars2 = env2.vars.as_ref().unwrap();
        assert_eq!(vars2.get("SHARED"), Some(&"root_value".to_string()));
        assert_eq!(vars2.get("ROOT_ONLY"), Some(&"root".to_string()));
        assert!(vars2.get("SERVICE_ONLY").is_none());
    }

    #[test]
    fn hash_computation_is_stable() {
        // Same config should always produce the same hash
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

        let base_hash = base_config.compute_hash();
        let command_hash = modified_command.compute_hash();
        let cron_hash = modified_cron.compute_hash();

        assert_ne!(
            base_hash, command_hash,
            "Changing command should change hash"
        );
        assert_ne!(base_hash, cron_hash, "Adding cron should change hash");
        assert_ne!(
            command_hash, cron_hash,
            "Different changes should produce different hashes"
        );
    }

    #[test]
    fn service_rename_preserves_hash() {
        // The hash should NOT depend on the service name
        // This test verifies that renaming a service doesn't change its hash

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
        };

        // The same config used with different service names should have the same hash
        let hash = config.compute_hash();

        // Verify that the hash is based on config content, not name
        // (Since ServiceConfig doesn't contain the name field, this is guaranteed)
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
