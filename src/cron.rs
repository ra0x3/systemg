//! Cron scheduling for services.
use crate::config::{Config, CronConfig};
use crate::error::ProcessManagerError;
use chrono::{Local, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tracing::{debug, info, warn};

/// Maximum number of execution history entries to keep per cron job.
const MAX_EXECUTION_HISTORY: usize = 10;

/// Status of a cron job execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CronExecutionStatus {
    /// Cron job completed successfully.
    Success,
    /// Cron job failed with an error message.
    Failed(String),
    /// Cron job was scheduled to run but previous execution was still running.
    OverlapError,
}

/// Record of a single cron job execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronExecutionRecord {
    /// When the cron job execution started.
    pub started_at: SystemTime,
    /// When the cron job execution completed (None if still running).
    pub completed_at: Option<SystemTime>,
    /// Final status of the execution (None if still running).
    pub status: Option<CronExecutionStatus>,
    /// Exit code of the process (None if no exit code available).
    pub exit_code: Option<i32>,
}

/// Tracks execution history and state for a single cron job.
#[derive(Debug, Clone)]
pub struct CronJobState {
    /// Name of the service this cron job manages.
    pub service_name: String,
    /// Parsed cron schedule expression.
    pub schedule: Schedule,
    /// Timestamp of the last execution start.
    pub last_execution: Option<SystemTime>,
    /// Timestamp when the job is next scheduled to run.
    pub next_execution: Option<SystemTime>,
    /// Whether an execution is currently in progress.
    pub currently_running: bool,
    /// Rolling history of recent executions (limited to MAX_EXECUTION_HISTORY).
    pub execution_history: VecDeque<CronExecutionRecord>,
    /// Timezone used for schedule calculations.
    pub timezone: EffectiveTimezone,
    /// Human-readable timezone label for display.
    pub timezone_label: String,
}

impl CronJobState {
    /// Creates a new cron job state, optionally restoring from persisted state.
    pub fn new(
        service_name: String,
        schedule: Schedule,
        timezone: EffectiveTimezone,
        timezone_label: String,
        persisted: Option<PersistedCronJobState>,
    ) -> Self {
        let next_execution = compute_next_execution(&schedule, timezone);

        let mut state = Self {
            service_name,
            schedule,
            last_execution: None,
            next_execution,
            currently_running: false,
            execution_history: VecDeque::with_capacity(MAX_EXECUTION_HISTORY),
            timezone,
            timezone_label,
        };

        if let Some(persisted) = persisted {
            state.last_execution = persisted.last_execution;
            state.execution_history = persisted.execution_history;
            while state.execution_history.len() > MAX_EXECUTION_HISTORY {
                state.execution_history.pop_front();
            }
        }

        state
    }

    /// Adds an execution record to the history, evicting the oldest if at capacity.
    pub fn add_execution_record(&mut self, record: CronExecutionRecord) {
        if self.execution_history.len() >= MAX_EXECUTION_HISTORY {
            self.execution_history.pop_front();
        }
        self.execution_history.push_back(record);
    }

    /// Recalculates the next execution time based on the cron schedule and timezone.
    pub fn update_next_execution(&mut self) {
        self.next_execution = compute_next_execution(&self.schedule, self.timezone);
    }
}

/// Timezone used for cron schedule calculations.
#[derive(Clone, Copy, Debug)]
pub enum EffectiveTimezone {
    /// Use the system's local timezone.
    Local,
    /// Use UTC timezone.
    Utc,
    /// Use a specific named timezone (e.g., America/New_York).
    Named(Tz),
}

fn compute_next_execution(
    schedule: &Schedule,
    tz: EffectiveTimezone,
) -> Option<SystemTime> {
    match tz {
        EffectiveTimezone::Local => schedule
            .upcoming(Local)
            .next()
            .map(|dt| dt.with_timezone(&Utc).into()),
        EffectiveTimezone::Utc => schedule.upcoming(Utc).next().map(|dt| dt.into()),
        EffectiveTimezone::Named(tz) => schedule
            .upcoming(tz)
            .next()
            .map(|dt| dt.with_timezone(&Utc).into()),
    }
}

/// Manager for all cron jobs in the system.
#[derive(Clone)]
pub struct CronManager {
    jobs: Arc<Mutex<Vec<CronJobState>>>,
    state_file: Arc<Mutex<CronStateFile>>,
}

impl Default for CronManager {
    fn default() -> Self {
        let state_file =
            CronStateFile::load().unwrap_or_else(|_| CronStateFile::default());
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            state_file: Arc::new(Mutex::new(state_file)),
        }
    }
}

impl CronManager {
    /// Creates a new cron manager, loading any persisted state from disk.
    pub fn new() -> Self {
        Self::default()
    }

    fn build_job_state(
        &self,
        service_name: &str,
        cron_config: &CronConfig,
    ) -> Result<(CronJobState, bool, String), ProcessManagerError> {
        let (effective_timezone, timezone_label) =
            resolve_timezone(cron_config, service_name)?;
        let (normalized_expression, normalized) =
            normalize_cron_expression(&cron_config.expression);
        let schedule = Schedule::from_str(&normalized_expression).map_err(|e| {
            let error_msg = format!(
                "Invalid cron expression '{}': {}",
                cron_config.expression, e
            );
            ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidInput, error_msg),
            }
        })?;

        let persisted_state = self
            .state_file
            .lock()
            .ok()
            .and_then(|state| state.jobs.get(service_name).cloned());

        let job_state = CronJobState::new(
            service_name.to_string(),
            schedule,
            effective_timezone,
            timezone_label.clone(),
            persisted_state,
        );

        Ok((job_state, normalized, normalized_expression))
    }

    /// Register a cron job from service configuration.
    pub fn register_job(
        &self,
        service_name: &str,
        cron_config: &CronConfig,
    ) -> Result<(), ProcessManagerError> {
        let (job_state, normalized, normalized_expression) =
            self.build_job_state(service_name, cron_config)?;
        let timezone_label = job_state.timezone_label.clone();
        let mut jobs = self.jobs.lock().unwrap();
        self.persist_job_state(&job_state);
        jobs.push(job_state.clone());

        if normalized {
            debug!(
                "Cron job '{}' expression normalized to '{}'",
                service_name, normalized_expression
            );
        }

        if let Some(next_exec) = job_state.next_execution {
            let now = SystemTime::now();
            let next_dt: chrono::DateTime<Utc> = next_exec.into();
            let now_dt: chrono::DateTime<Utc> = now.into();
            debug!(
                "Cron job '{}' scheduled with timezone {}. Next execution: {} (now: {})",
                service_name, timezone_label, next_dt, now_dt
            );
        } else {
            debug!(
                "Cron job '{}' scheduled with timezone {} but next_execution is None",
                service_name, timezone_label
            );
        }
        info!("Registered cron job for service '{}'", service_name);
        Ok(())
    }

    /// Replace all cron jobs using the provided configuration, pruning any that no longer exist.
    pub fn sync_from_config(&self, config: &Config) -> Result<(), ProcessManagerError> {
        let mut active_jobs = Vec::new();
        let mut active_names = HashSet::new();

        for (service_name, service_config) in &config.services {
            if let Some(cron_config) = &service_config.cron {
                let (job_state, normalized, normalized_expression) =
                    self.build_job_state(service_name, cron_config)?;
                let timezone_label = job_state.timezone_label.clone();

                self.persist_job_state(&job_state);
                if normalized {
                    debug!(
                        "Cron job '{}' expression normalized to '{}'",
                        service_name, normalized_expression
                    );
                }

                if let Some(next_exec) = job_state.next_execution {
                    let now = SystemTime::now();
                    let next_dt: chrono::DateTime<Utc> = next_exec.into();
                    let now_dt: chrono::DateTime<Utc> = now.into();
                    debug!(
                        "Cron job '{}' scheduled with timezone {}. Next execution: {} (now: {})",
                        service_name, timezone_label, next_dt, now_dt
                    );
                } else {
                    debug!(
                        "Cron job '{}' scheduled with timezone {} but next_execution is None",
                        service_name, timezone_label
                    );
                }

                active_names.insert(service_name.clone());
                info!("Registered cron job for service '{}'", service_name);
                active_jobs.push(job_state);
            }
        }

        {
            let mut jobs_guard = self.jobs.lock().unwrap();
            *jobs_guard = active_jobs;
        }

        self.prune_inactive_jobs(&active_names);

        Ok(())
    }

    /// Check if any cron jobs are due to run and return their names.
    pub fn get_due_jobs(&self) -> Vec<String> {
        let mut jobs = self.jobs.lock().unwrap();
        let now = SystemTime::now();
        let mut due_jobs = Vec::new();

        for job in jobs.iter_mut() {
            if let Some(next_exec) = job.next_execution
                && now >= next_exec
            {
                let next_dt: chrono::DateTime<Utc> = next_exec.into();
                let now_dt: chrono::DateTime<Utc> = now.into();
                debug!(
                    "Cron job '{}' is due (next_exec: {}, now: {})",
                    job.service_name, next_dt, now_dt
                );

                if job.currently_running {
                    warn!(
                        "Cron job '{}' is scheduled to run but previous execution is still running",
                        job.service_name
                    );
                    // Record overlap error
                    let record = CronExecutionRecord {
                        started_at: now,
                        completed_at: Some(now),
                        status: Some(CronExecutionStatus::OverlapError),
                        exit_code: None,
                    };
                    job.add_execution_record(record);
                    job.update_next_execution();
                    self.persist_job_state(job);
                } else {
                    due_jobs.push(job.service_name.clone());
                    job.currently_running = true;
                    job.last_execution = Some(now);

                    // Start execution record
                    let record = CronExecutionRecord {
                        started_at: now,
                        completed_at: None,
                        status: None,
                        exit_code: None,
                    };
                    job.add_execution_record(record);
                    job.update_next_execution();
                    self.persist_job_state(job);
                }
            }
        }

        due_jobs
    }

    /// Mark a cron job as completed.
    pub fn mark_job_completed(
        &self,
        service_name: &str,
        status: CronExecutionStatus,
        exit_code: Option<i32>,
    ) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.service_name == service_name) {
            job.currently_running = false;

            // Update the last execution record
            if let Some(record) = job.execution_history.back_mut() {
                record.completed_at = Some(SystemTime::now());
                record.status = Some(status);
                record.exit_code = exit_code;
            }

            debug!("Cron job '{}' completed", service_name);
            self.persist_job_state(job);
        }
    }

    /// Get the state of all cron jobs (for status display).
    pub fn get_all_jobs(&self) -> Vec<CronJobState> {
        let jobs = self.jobs.lock().unwrap();
        jobs.iter()
            .map(|job| {
                // Create a debug-compatible clone
                CronJobState {
                    service_name: job.service_name.clone(),
                    schedule: Schedule::from_str(&job.schedule.to_string()).unwrap(),
                    last_execution: job.last_execution,
                    next_execution: job.next_execution,
                    currently_running: job.currently_running,
                    execution_history: job.execution_history.clone(),
                    timezone: job.timezone,
                    timezone_label: job.timezone_label.clone(),
                }
            })
            .collect()
    }

    /// Clear all registered cron jobs.
    pub fn clear_all_jobs(&self) {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.clear();
    }

    fn prune_inactive_jobs(&self, active_names: &HashSet<String>) {
        if let Ok(mut state) = self.state_file.lock() {
            let original_len = state.jobs.len();
            state.jobs.retain(|name, _| active_names.contains(name));

            if state.jobs.len() != original_len
                && let Err(err) = state.save()
            {
                warn!("Failed to persist pruned cron state: {}", err);
            }
        }
    }

    fn persist_job_state(&self, job: &CronJobState) {
        if let Ok(mut state) = self.state_file.lock() {
            state.jobs.insert(
                job.service_name.clone(),
                PersistedCronJobState {
                    last_execution: job.last_execution,
                    execution_history: job.execution_history.clone(),
                    timezone_label: job.timezone_label.clone(),
                    timezone: match job.timezone {
                        EffectiveTimezone::Local => None,
                        EffectiveTimezone::Utc => Some("UTC".to_string()),
                        EffectiveTimezone::Named(tz) => Some(tz.name().to_string()),
                    },
                },
            );

            if let Err(err) = state.save() {
                warn!(
                    "Failed to persist cron state for '{}': {}",
                    job.service_name, err
                );
            }
        }
    }
}

/// Persistent storage for cron job state across supervisor restarts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronStateFile {
    jobs: std::collections::BTreeMap<String, PersistedCronJobState>,
}

impl CronStateFile {
    fn path() -> PathBuf {
        let home = std::env::var("HOME").expect("HOME not set");
        PathBuf::from(format!("{}/.local/share/systemg/cron_state.json", home))
    }

    fn save(&self) -> Result<(), std::io::Error> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;

        // Use explicit File operations with sync_all() to ensure data is flushed to disk
        // before returning. This prevents race conditions in tests on Linux where the OS
        // might buffer writes and the file could be read as empty before the buffer is flushed.
        use std::io::Write;
        let mut file = fs::File::create(&path)?;
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    /// Loads the cron state file from disk, creating an empty one if it doesn't exist.
    pub fn load() -> Result<Self, std::io::Error> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(path)?;
        let state = serde_json::from_str(&raw)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        Ok(state)
    }

    /// Returns a reference to the map of persisted cron job states.
    pub fn jobs(&self) -> &std::collections::BTreeMap<String, PersistedCronJobState> {
        &self.jobs
    }
}

/// Serializable cron job state that persists across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedCronJobState {
    /// Timestamp of the last execution start.
    pub last_execution: Option<SystemTime>,
    /// Rolling history of recent executions.
    pub execution_history: VecDeque<CronExecutionRecord>,
    /// Human-readable timezone label.
    pub timezone_label: String,
    /// Optional timezone string (e.g., "UTC", "America/New_York").
    pub timezone: Option<String>,
}

impl Default for PersistedCronJobState {
    fn default() -> Self {
        Self {
            last_execution: None,
            execution_history: VecDeque::with_capacity(MAX_EXECUTION_HISTORY),
            timezone_label: "".to_string(),
            timezone: None,
        }
    }
}

fn normalize_cron_expression(expr: &str) -> (String, bool) {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    match parts.len() {
        5 => (format!("0 {}", parts.join(" ")), true),
        _ => (parts.join(" "), false),
    }
}

fn resolve_timezone(
    cron_config: &CronConfig,
    service_name: &str,
) -> Result<(EffectiveTimezone, String), ProcessManagerError> {
    if let Some(tz_raw) = cron_config
        .timezone
        .as_ref()
        .map(|tz| tz.trim())
        .filter(|tz| !tz.is_empty())
    {
        if tz_raw.eq_ignore_ascii_case("utc") {
            return Ok((EffectiveTimezone::Utc, "UTC".to_string()));
        }

        if tz_raw.eq_ignore_ascii_case("local") {
            let label = format!("local ({})", Local::now().format("%Z%:z"));
            return Ok((EffectiveTimezone::Local, label));
        }

        match tz_raw.parse::<Tz>() {
            Ok(tz) => {
                let label = tz.name().to_string();
                Ok((EffectiveTimezone::Named(tz), label))
            }
            Err(e) => Err(ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Invalid timezone '{}': {}", tz_raw, e),
                ),
            }),
        }
    } else {
        let label = format!("local ({})", Local::now().format("%Z%:z"));
        Ok((EffectiveTimezone::Local, label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};
    use std::{collections::HashMap, fs};

    use crate::config::ServiceConfig;

    #[test]
    fn test_cron_manager_registration() {
        let manager = CronManager::new();
        let cron_config = CronConfig {
            expression: "0 * * * * *".to_string(),
            timezone: Some("UTC".into()),
        };

        assert!(manager.register_job("test_service", &cron_config).is_ok());

        let jobs = manager.get_all_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].service_name, "test_service");
        assert!(matches!(jobs[0].timezone, EffectiveTimezone::Utc));
    }

    #[test]
    fn test_invalid_cron_expression() {
        let manager = CronManager::new();
        let cron_config = CronConfig {
            expression: "invalid cron".to_string(),
            timezone: None,
        };

        assert!(manager.register_job("test_service", &cron_config).is_err());
    }

    #[test]
    fn test_five_field_expression_normalizes() {
        let manager = CronManager::new();
        let cron_config = CronConfig {
            expression: "* * * * *".to_string(),
            timezone: None,
        };

        assert!(manager.register_job("test_service", &cron_config).is_ok());
        let jobs = manager.get_all_jobs();
        assert!(jobs[0].next_execution.is_some());
    }

    #[test]
    fn persists_execution_history_with_exit_codes() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempfile::tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }

        let manager = CronManager::new();
        let cron_config = CronConfig {
            expression: "* * * * * *".to_string(),
            timezone: Some("UTC".into()),
        };

        manager
            .register_job("persisted_service", &cron_config)
            .unwrap();

        {
            let mut jobs = manager.jobs.lock().unwrap();
            let job = jobs
                .iter_mut()
                .find(|j| j.service_name == "persisted_service")
                .expect("job registered");
            job.next_execution = Some(SystemTime::now() - Duration::from_secs(1));
        }

        let due = manager.get_due_jobs();
        assert_eq!(due, vec!["persisted_service".to_string()]);

        manager.mark_job_completed(
            "persisted_service",
            CronExecutionStatus::Success,
            Some(0),
        );

        let state = CronStateFile::load().expect("load cron state");
        let persisted = state
            .jobs()
            .get("persisted_service")
            .expect("persisted cron job");

        assert_eq!(persisted.execution_history.len(), 1);
        let record = persisted.execution_history.back().unwrap();
        assert!(matches!(record.status, Some(CronExecutionStatus::Success)));
        assert_eq!(record.exit_code, Some(0));

        if let Some(original) = original_home {
            unsafe {
                std::env::set_var("HOME", original);
            }
        }
    }

    fn service_with_cron(expr: &str) -> ServiceConfig {
        ServiceConfig {
            command: "/bin/true".into(),
            env: None,
            restart_policy: None,
            backoff: None,
            max_restarts: None,
            depends_on: None,
            deployment: None,
            hooks: None,
            cron: Some(CronConfig {
                expression: expr.to_string(),
                timezone: None,
            }),
            skip: None,
        }
    }

    #[test]
    fn sync_from_config_prunes_removed_jobs() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempfile::tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }

        let manager = CronManager::new();

        let mut services_v1 = HashMap::new();
        services_v1.insert("job_one".to_string(), service_with_cron("* * * * * *"));
        services_v1.insert("job_two".to_string(), service_with_cron("*/2 * * * * *"));
        let config_v1 = Config {
            version: "1".to_string(),
            services: services_v1,
            project_dir: None,
            env: None,
        };

        manager.sync_from_config(&config_v1).unwrap();

        let mut services_v2 = HashMap::new();
        services_v2.insert("job_two".to_string(), service_with_cron("*/2 * * * * *"));
        services_v2.insert("job_three".to_string(), service_with_cron("0 */5 * * * *"));
        let config_v2 = Config {
            version: "1".to_string(),
            services: services_v2,
            project_dir: None,
            env: None,
        };

        manager.sync_from_config(&config_v2).unwrap();

        let job_names: Vec<String> = manager
            .get_all_jobs()
            .into_iter()
            .map(|job| job.service_name)
            .collect();
        assert_eq!(job_names.len(), 2);
        assert!(job_names.contains(&"job_two".to_string()));
        assert!(job_names.contains(&"job_three".to_string()));
        assert!(!job_names.contains(&"job_one".to_string()));

        let state = CronStateFile::load().expect("load cron state");
        assert!(state.jobs().contains_key("job_two"));
        assert!(state.jobs().contains_key("job_three"));
        assert!(!state.jobs().contains_key("job_one"));

        if let Some(original) = original_home {
            unsafe {
                std::env::set_var("HOME", original);
            }
        }
    }
}
