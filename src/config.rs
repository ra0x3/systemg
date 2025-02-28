use crate::error::ProcessManagerError;
use serde::Deserialize;
use std::collections::HashMap;

/// Represents the structure of the configuration file.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Configuration version.
    pub version: u8,
    /// Map of service names to their respective configurations.
    pub services: HashMap<String, ServiceConfig>,
}

/// Configuration for an individual service.
#[derive(Debug, Deserialize)]
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
    /// Hooks for lifecycle events (e.g., on_start, on_error).
    pub hooks: Option<Hooks>,
}

/// Represents environment variables for a service.
#[derive(Debug, Deserialize)]
pub struct EnvConfig {
    /// Optional path to an environment file.
    pub file: Option<String>,
    /// Key-value pairs of environment variables.
    pub vars: Option<HashMap<String, String>>,
}

/// Hooks that run on specific service lifecycle events.
#[derive(Debug, Deserialize)]
pub struct Hooks {
    /// Command to execute when the service starts.
    pub on_start: Option<String>,
    /// Command to execute when the service fails or crashes.
    pub on_error: Option<String>,
}

/// Loads and parses the configuration file.
///
/// # Arguments
///
/// * `path` - The path to the configuration YAML file.
///
/// # Returns
///
/// * `Ok(Config)` if the file is successfully parsed.
/// * `Err(ProcessManagerError::ConfigReadError)` if the file cannot be read.
/// * `Err(ProcessManagerError::ConfigParseError)` if the file format is invalid.
///
/// # Example
///
/// ```rust
/// use systemg::config::load_config;
/// let config = load_config("config.yaml").unwrap();
/// ```
pub fn load_config(path: &str) -> Result<Config, ProcessManagerError> {
    let content =
        std::fs::read_to_string(path).map_err(ProcessManagerError::ConfigReadError)?;
    let config: Config =
        serde_yaml::from_str(&content).map_err(ProcessManagerError::ConfigParseError)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper function to create a mock YAML config.
    fn mock_yaml_config() -> &'static str {
        r#"
        version: 1
        services:
          test_service:
            command: "echo Hello"
            env:
              vars:
                KEY1: "VALUE1"
                KEY2: "VALUE2"
            restart_policy: "on-failure"
            backoff: "5s"
            depends_on: ["db"]
            hooks:
              on_start: "echo Started"
              on_error: "echo Failed"
          db:
            command: "postgres -D /var/lib/postgres"
            restart_policy: "always"
        "#
    }

    /// Test that a valid config file parses correctly.
    #[test]
    fn test_load_valid_config() {
        let config: Config =
            serde_yaml::from_str(mock_yaml_config()).expect("Failed to parse YAML");

        assert_eq!(config.version, 1);
        assert!(config.services.contains_key("test_service"));
        assert!(config.services.contains_key("db"));

        let service = config.services.get("test_service").unwrap();
        assert_eq!(service.command, "echo Hello");
        assert_eq!(service.restart_policy.as_deref(), Some("on-failure"));
        assert_eq!(service.backoff.as_deref(), Some("5s"));
        assert_eq!(
            service.depends_on.as_ref().unwrap(),
            &vec!["db".to_string()]
        );

        let env_vars = service.env.as_ref().unwrap().vars.as_ref().unwrap();
        assert_eq!(env_vars.get("KEY1").unwrap(), "VALUE1");
        assert_eq!(env_vars.get("KEY2").unwrap(), "VALUE2");

        let hooks = service.hooks.as_ref().unwrap();
        assert_eq!(hooks.on_start.as_deref(), Some("echo Started"));
        assert_eq!(hooks.on_error.as_deref(), Some("echo Failed"));
    }

    /// Test that a missing YAML file returns a `ConfigReadError`.
    #[test]
    fn test_load_missing_file() {
        let result = load_config("nonexistent.yaml");
        assert!(matches!(
            result,
            Err(ProcessManagerError::ConfigReadError(_))
        ));
    }

    /// Test that an invalid YAML format returns a `ConfigParseError`.
    #[test]
    fn test_load_invalid_yaml() {
        let invalid_yaml = "invalid_yaml: [unterminated";
        let result: Result<Config, _> = serde_yaml::from_str(invalid_yaml);
        assert!(result.is_err());
    }
}
