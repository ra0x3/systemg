//! Configuration management for Systemg.
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::{BTreeSet, HashMap},
    env, fs,
    path::{Path, PathBuf},
};
use strum_macros::{AsRefStr, EnumString};

use crate::error::ProcessManagerError;

/// Represents the structure of the configuration file.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Configuration version.
    pub version: String,
    /// Map of service names to their respective configurations.
    pub services: HashMap<String, ServiceConfig>,
    /// Root directory from which relative paths are resolved.
    pub project_dir: Option<String>,
}

/// Configuration for an individual service.
#[derive(Debug, Deserialize, Clone)]
pub struct ServiceConfig {
    /// Command used to start the service.
    pub command: String,
    /// Optional environment variables for the service.
    pub env: Option<EnvConfig>,
    /// Restart policy (e.g., "always", "on-failure", "never").
    pub restart_policy: Option<String>,
    /// Backoff time before restarting a failed service.
    pub backoff: Option<String>,
    /// List of services that must start before this service.
    pub depends_on: Option<Vec<String>>,
    /// Deployment strategy configuration.
    pub deployment: Option<DeploymentConfig>,
    /// Hooks for lifecycle events (e.g., on_start, on_error).
    pub hooks: Option<Hooks>,
}

/// Deployment strategy configuration for a service.
#[derive(Debug, Deserialize, Clone)]
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
#[derive(Debug, Deserialize, Clone)]
pub struct HealthCheckConfig {
    /// Health check URL.
    pub url: String,
    /// Health check timeout duration (e.g., "30s").
    pub timeout: Option<String>,
    /// Number of retries before giving up.
    pub retries: Option<u32>,
}

/// Represents environment variables for a service.
#[derive(Debug, Deserialize, Clone)]
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
}

#[derive(Debug, EnumString, AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum HookType {
    OnStart,
    OnError,
}

/// Hooks that run on specific service lifecycle events.
#[derive(Debug, Deserialize, Clone)]
pub struct Hooks {
    pub on_start: Option<String>,
    pub on_error: Option<String>,
}

impl Config {
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

    for service in config.services.values_mut() {
        if let Some(env_config) = &service.env
            && let Some(resolved_path) = env_config.path(&base_path)
        {
            load_env_file(&resolved_path.to_string_lossy())?;
        }
    }

    let expanded_content = expand_env_vars(&content)?;

    let mut config: Config = serde_yaml::from_str(&expanded_content)
        .map_err(ProcessManagerError::ConfigParseError)?;

    config.project_dir = Some(base_path.to_string_lossy().to_string());
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
            restart_policy: None,
            backoff: None,
            depends_on: depends_on
                .map(|deps| deps.into_iter().map(String::from).collect()),
            deployment: None,
            hooks: None,
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
        };

        match config.service_start_order() {
            Err(ProcessManagerError::DependencyCycle { cycle }) => {
                assert!(cycle.contains("a"));
                assert!(cycle.contains("b"));
            }
            other => panic!("expected dependency cycle error, got {other:?}"),
        }
    }
}
