use regex::Regex;
use serde::Deserialize;
use std::{collections::HashMap, env, fs};

use crate::error::ProcessManagerError;

/// Represents the structure of the configuration file.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Configuration version.
    pub version: String,
    /// Map of service names to their respective configurations.
    pub services: HashMap<String, ServiceConfig>,
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
    /// Hooks for lifecycle events (e.g., on_start, on_error).
    pub hooks: Option<Hooks>,
}

/// Represents environment variables for a service.
#[derive(Debug, Deserialize, Clone)]
pub struct EnvConfig {
    /// Optional path to an environment file.
    pub file: Option<String>,
    /// Key-value pairs of environment variables.
    pub vars: Option<HashMap<String, String>>,
}

/// Hooks that run on specific service lifecycle events.
#[derive(Debug, Deserialize, Clone)]
pub struct Hooks {
    pub on_start: Option<String>,
    pub on_error: Option<String>,
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
pub fn load_config(path: &str) -> Result<Config, ProcessManagerError> {
    let content = fs::read_to_string(path).map_err(|e| {
        ProcessManagerError::ConfigReadError(std::io::Error::new(
            e.kind(),
            format!("{} ({})", e, path),
        ))
    })?;
    let mut config: Config =
        serde_yaml::from_str(&content).map_err(ProcessManagerError::ConfigParseError)?;

    // Load env files first before expanding variables
    for service in config.services.values_mut() {
        if let Some(env_config) = &service.env {
            if let Some(env_file) = &env_config.file {
                load_env_file(env_file)?;
            }
        }
    }

    // Now expand environment variables after loading .env
    let expanded_content = expand_env_vars(&content)?;

    // Parse the final expanded config
    let config: Config = serde_yaml::from_str(&expanded_content)
        .map_err(ProcessManagerError::ConfigParseError)?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    /// Test that an env file is properly loaded.
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

    /// Test that a YAML config with env file reference is properly loaded.
    #[test]
    fn test_load_config_with_env_file() {
        let dir = tempfile::tempdir().unwrap();

        // Create a fake .env file
        let env_path = dir.path().join(".env");
        let mut env_file = std::fs::File::create(&env_path).unwrap();
        writeln!(env_file, "MY_TEST_VAR=HelloWorld").unwrap();
        writeln!(env_file, "DB_USER=admin").unwrap();
        writeln!(env_file, "DB_PASS=secret").unwrap();

        // Create a mock YAML config referencing the .env file
        let yaml_path = dir.path().join("systemg.yaml");
        let mut yaml_file = std::fs::File::create(&yaml_path).unwrap();
        writeln!(
            yaml_file,
            r#"
        version: "1"
        services:
          test_service:
            command: "echo ${{MY_TEST_VAR}}"
            env:
              file: "{}"
              vars:
                DB_HOST: "localhost"
        "#,
            env_path.to_str().unwrap()
        )
        .unwrap();

        // Set MY_TEST_VAR in the test environment so it expands properly
        unsafe {
            std::env::set_var("MY_TEST_VAR", "HelloWorld");
        }

        // Load the config and verify values
        let config = load_config(yaml_path.to_str().unwrap()).unwrap();
        let service = config.services.get("test_service").unwrap();

        // Ensure command expansion works correctly
        assert_eq!(service.command, "echo HelloWorld");

        // Extract loaded env vars from the config instead of calling env::var()
        let env_vars = service.env.as_ref().unwrap().vars.as_ref().unwrap();
        assert_eq!(env_vars.get("DB_HOST").unwrap(), "localhost");

        // Verify .env file values were loaded correctly
        assert_eq!(std::env::var("MY_TEST_VAR").unwrap(), "HelloWorld");
        assert_eq!(std::env::var("DB_USER").unwrap(), "admin");
        assert_eq!(std::env::var("DB_PASS").unwrap(), "secret");
    }
}
