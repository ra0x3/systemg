//! Cron scheduling for services.
use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{Local, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use fs2::FileExt;
use serde::{
    Deserialize, Serialize,
    de::{EnumAccess, IgnoredAny, MapAccess, VariantAccess, Visitor},
};
use tracing::{debug, info, warn};

use crate::{
    config::{Config, CronConfig},
    error::ProcessManagerError,
    state_store::StateStore,
};

/// Maximum number of execution history entries to keep per cron job.
const MAX_EXECUTION_HISTORY: usize = 10;
/// Serialized label for a successful cron execution.
const CRON_STATUS_SUCCESS: &str = "Success";
/// Serialized label for a failed cron execution.
const CRON_STATUS_FAILED: &str = "Failed";
/// Serialized prefix for a failed cron execution carrying its reason.
const CRON_STATUS_FAILED_PREFIX: &str = "Failed:";
/// Serialized label for a supervisor-interrupted cron execution.
const CRON_STATUS_INTERRUPTED: &str = "Interrupted";
/// Serialized prefix for an interrupted execution carrying its reason.
const CRON_STATUS_INTERRUPTED_PREFIX: &str = "Interrupted:";
/// Serialized label for a cron execution skipped because an earlier run overlapped.
const CRON_STATUS_OVERLAP: &str = "OverlapError";
/// Variants accepted by the cron execution status compatibility decoder.
const CRON_STATUS_VARIANTS: &[&str] = &[
    CRON_STATUS_SUCCESS,
    CRON_STATUS_FAILED,
    CRON_STATUS_INTERRUPTED,
    CRON_STATUS_OVERLAP,
];
/// Reason restored when an older state file discarded a failure detail.
const LEGACY_CRON_FAILURE_REASON: &str = "failure reason unavailable from legacy state";
/// Reason restored when an interrupted record contains no detail.
const UNKNOWN_CRON_INTERRUPTION_REASON: &str = "interruption reason was not recorded";
/// Reason assigned when recovery finds an execution whose process is gone.
const STALE_CRON_INTERRUPTION_REASON: &str =
    "supervisor stopped tracking the execution before it completed";

/// Acquires shared cron state and recovers its value after mutex poisoning.
fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Returns whether a process appears to still exist.
#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    use nix::{errno::Errno, sys::signal, unistd::Pid};

    match signal::kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// Returns whether a process appears to still exist.
#[cfg(not(unix))]
fn process_is_running(_pid: u32) -> bool {
    false
}

/// Returns whether an execution record has not been completed.
fn cron_record_is_incomplete(record: &CronExecutionRecord) -> bool {
    record.completed_at.is_none() && record.status.is_none() && record.exit_code.is_none()
}

/// Whether two execution timestamps identify the same persisted run.
fn same_run(left: SystemTime, right: SystemTime) -> bool {
    left.duration_since(UNIX_EPOCH)
        .ok()
        .map(|value| value.as_secs())
        == right
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|value| value.as_secs())
}

/// Returns whether a previously persisted in-progress execution is still live.
fn incomplete_execution_is_live(record: &CronExecutionRecord) -> bool {
    cron_record_is_incomplete(record)
        && record.pid.is_some_and(|pid| {
            process_is_running(pid)
                && record.process_start.is_none_or(|started| {
                    crate::daemon::process_start_time(pid) == Some(started)
                })
        })
}

/// Provides systemtime serde support.
mod systemtime_serde {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serializer};

    /// Serializes this item.
    pub fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let duration = time
            .duration_since(UNIX_EPOCH)
            .map_err(serde::ser::Error::custom)?;
        serializer.serialize_u64(duration.as_secs())
    }

    /// Handles deserialize.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

/// Provides systemtime serde opt support.
mod systemtime_serde_opt {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serializer};

    /// Serializes this item.
    pub fn serialize<S>(
        time: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match time {
            Some(t) => {
                let duration = t
                    .duration_since(UNIX_EPOCH)
                    .map_err(serde::ser::Error::custom)?;
                serializer.serialize_u64(duration.as_secs())
            }
            None => serializer.serialize_u64(0), // Use 0 to represent None for XML compatibility
        }
    }

    /// Handles deserialize.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<SystemTime>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        if secs == 0 {
            Ok(None)
        } else {
            Ok(Some(UNIX_EPOCH + Duration::from_secs(secs)))
        }
    }
}

/// Status of a cron job execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronExecutionStatus {
    /// Cron job completed successfully.
    Success,
    /// Cron job failed with an error message.
    Failed(String),
    /// Cron job lost supervision before a process result could be observed.
    Interrupted(String),
    /// Cron job was scheduled to run but previous execution was still running.
    OverlapError,
}

impl CronExecutionStatus {
    /// Encodes one outcome as an XML-safe scalar while retaining its reason.
    fn serialized_value(&self) -> String {
        match self {
            Self::Success => CRON_STATUS_SUCCESS.to_string(),
            Self::Failed(reason) => status_with_reason(CRON_STATUS_FAILED, reason),
            Self::Interrupted(reason) => {
                status_with_reason(CRON_STATUS_INTERRUPTED, reason)
            }
            Self::OverlapError => CRON_STATUS_OVERLAP.to_string(),
        }
    }

    /// Parses the scalar representation used by current and legacy state files.
    fn from_text<E>(value: &str) -> Result<Self, E>
    where
        E: serde::de::Error,
    {
        if let Some(reason) = value.strip_prefix(CRON_STATUS_FAILED_PREFIX) {
            return Ok(Self::Failed(reason.trim().to_string()));
        }
        if let Some(reason) = value.strip_prefix(CRON_STATUS_INTERRUPTED_PREFIX) {
            return Ok(Self::Interrupted(reason.trim().to_string()));
        }
        match value {
            CRON_STATUS_SUCCESS => Ok(Self::Success),
            CRON_STATUS_FAILED => {
                Ok(Self::Failed(LEGACY_CRON_FAILURE_REASON.to_string()))
            }
            CRON_STATUS_INTERRUPTED => Ok(Self::Interrupted(
                UNKNOWN_CRON_INTERRUPTION_REASON.to_string(),
            )),
            CRON_STATUS_OVERLAP => Ok(Self::OverlapError),
            other => Err(E::unknown_variant(other, CRON_STATUS_VARIANTS)),
        }
    }
}

/// Joins a persisted outcome label and optional human-readable reason.
fn status_with_reason(status: &str, reason: &str) -> String {
    if reason.trim().is_empty() {
        status.to_string()
    } else {
        format!("{status}: {reason}")
    }
}

impl Serialize for CronExecutionStatus {
    /// Serializes the outcome as a scalar accepted by JSON and XML state stores.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.serialized_value())
    }
}

/// Compatibility representation for reasons stored as scalars or XML text nodes.
#[derive(Deserialize)]
#[serde(untagged)]
enum StatusReasonValue {
    /// Reason stored directly as a string scalar.
    Plain(String),
    /// Reason stored inside quick-xml's text-node wrapper.
    Text {
        /// Text content of the serialized reason.
        #[serde(rename = "$text")]
        value: String,
    },
}

impl StatusReasonValue {
    /// Converts a compatibility wrapper into the concrete outcome reason.
    fn into_reason(self) -> String {
        match self {
            Self::Plain(reason) => reason,
            Self::Text { value } => value,
        }
    }
}

impl<'de> Deserialize<'de> for CronExecutionStatus {
    /// Deserializes current scalar outcomes and historical enum-shaped values.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Represents cron execution status visitor.
        struct CronExecutionStatusVisitor;

        impl<'de> Visitor<'de> for CronExecutionStatusVisitor {
            type Value = CronExecutionStatus;

            /// Describes the supported compatibility representations.
            fn expecting(
                &self,
                formatter: &mut std::fmt::Formatter<'_>,
            ) -> std::fmt::Result {
                formatter.write_str("a cron execution status in enum-tag or text form")
            }

            /// Parses a borrowed scalar status value.
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                CronExecutionStatus::from_text(value)
            }

            /// Parses an owned scalar status value.
            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(&value)
            }

            /// Parses serde's externally tagged enum compatibility shape.
            fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
            where
                A: EnumAccess<'de>,
            {
                let (variant, access) = data.variant::<String>()?;
                if let Some(reason) = variant.strip_prefix(CRON_STATUS_FAILED_PREFIX) {
                    access.unit_variant()?;
                    return Ok(CronExecutionStatus::Failed(reason.trim().to_string()));
                }
                if let Some(reason) = variant.strip_prefix(CRON_STATUS_INTERRUPTED_PREFIX)
                {
                    access.unit_variant()?;
                    return Ok(CronExecutionStatus::Interrupted(
                        reason.trim().to_string(),
                    ));
                }
                match variant.as_str() {
                    CRON_STATUS_SUCCESS => {
                        access.unit_variant()?;
                        Ok(CronExecutionStatus::Success)
                    }
                    CRON_STATUS_OVERLAP => {
                        access.unit_variant()?;
                        Ok(CronExecutionStatus::OverlapError)
                    }
                    CRON_STATUS_FAILED => {
                        let reason = access.newtype_variant::<StatusReasonValue>()?;
                        Ok(CronExecutionStatus::Failed(reason.into_reason()))
                    }
                    CRON_STATUS_INTERRUPTED => {
                        let reason = access.newtype_variant::<StatusReasonValue>()?;
                        Ok(CronExecutionStatus::Interrupted(reason.into_reason()))
                    }
                    other => Err(serde::de::Error::unknown_variant(
                        other,
                        CRON_STATUS_VARIANTS,
                    )),
                }
            }

            /// Parses quick-xml map and text-node compatibility shapes.
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut text_variant: Option<String> = None;
                let mut failed_reason: Option<String> = None;
                let mut tagged_variant: Option<CronExecutionStatus> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "$text" => text_variant = Some(map.next_value::<String>()?),
                        "$value" => failed_reason = Some(map.next_value::<String>()?),
                        CRON_STATUS_SUCCESS => {
                            let _: IgnoredAny = map.next_value()?;
                            tagged_variant = Some(CronExecutionStatus::Success);
                        }
                        CRON_STATUS_OVERLAP => {
                            let _: IgnoredAny = map.next_value()?;
                            tagged_variant = Some(CronExecutionStatus::OverlapError);
                        }
                        CRON_STATUS_FAILED => {
                            let value = map.next_value::<StatusReasonValue>()?;
                            let reason = value.into_reason();
                            failed_reason = Some(reason.clone());
                            tagged_variant = Some(CronExecutionStatus::Failed(reason));
                        }
                        CRON_STATUS_INTERRUPTED => {
                            let value = map.next_value::<StatusReasonValue>()?;
                            tagged_variant = Some(CronExecutionStatus::Interrupted(
                                value.into_reason(),
                            ));
                        }
                        _ => {
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }

                if let Some(status) = tagged_variant {
                    return Ok(status);
                }

                if let Some(text) = text_variant {
                    if text == CRON_STATUS_FAILED {
                        return Ok(CronExecutionStatus::Failed(
                            failed_reason.unwrap_or_else(|| {
                                LEGACY_CRON_FAILURE_REASON.to_string()
                            }),
                        ));
                    }
                    return CronExecutionStatus::from_text(&text);
                }

                if let Some(reason) = failed_reason {
                    return Ok(CronExecutionStatus::Failed(reason));
                }

                Err(serde::de::Error::custom(
                    "missing cron execution status value",
                ))
            }
        }

        deserializer.deserialize_any(CronExecutionStatusVisitor)
    }
}

/// Record of a single cron job execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronExecutionRecord {
    /// When the cron job execution started.
    #[serde(with = "systemtime_serde")]
    pub started_at: SystemTime,
    /// Observed completion time, absent while running or when supervision ended first.
    #[serde(with = "systemtime_serde_opt")]
    pub completed_at: Option<SystemTime>,
    /// Terminal outcome, absent only while the execution is actively tracked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CronExecutionStatus>,
    /// Exit code of the process (None if no exit code available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// PID of the spawned cron process when one was observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Kernel process start identity used to reject PID reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start: Option<u64>,
    /// User that executed the cron process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Command line used for the cron execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Metrics collected during this execution (for resource usage display).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<crate::metrics::MetricSample>,
}

/// Tracks execution history and state for a single cron job.
#[derive(Debug, Clone)]
pub struct CronJobState {
    /// Project this cron job belongs to; selects its persistence directory.
    pub project_id: String,
    /// Name of the service this cron job manages.
    pub service_name: String,
    /// Configuration hash of the service (used for persistence across renames).
    pub service_hash: String,
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

/// A cron job that is due to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronDueJob {
    /// Name of the service this cron job manages.
    pub service_name: String,
    /// Configuration hash of the service, used to resolve project ownership.
    pub service_hash: String,
    /// Start identity for the execution record created by the scheduler.
    pub started_at: SystemTime,
}

impl CronJobState {
    /// Creates a new cron job state, optionally restoring from persisted state.
    pub fn new(
        project_id: String,
        service_name: String,
        service_hash: String,
        schedule: Schedule,
        timezone: EffectiveTimezone,
        timezone_label: String,
        persisted: Option<PersistedCronJobState>,
    ) -> Self {
        let next_execution = compute_next_execution(&schedule, timezone);

        let mut state = Self {
            project_id,
            service_name,
            service_hash,
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
            let live_execution = state
                .active_record()
                .filter(|record| incomplete_execution_is_live(record))
                .map(|record| record.started_at);
            state.currently_running = live_execution.is_some();
            state.close_stale_running_executions(live_execution);
        }

        state
    }

    /// Adds an execution record to the history, evicting the oldest if at capacity.
    pub fn add_execution_record(&mut self, record: CronExecutionRecord) {
        if self.execution_history.len() >= MAX_EXECUTION_HISTORY {
            let remove = self
                .execution_history
                .iter()
                .position(|record| !cron_record_is_incomplete(record))
                .unwrap_or(0);
            self.execution_history.remove(remove);
        }
        self.execution_history.push_back(record);
    }

    /// Returns the newest execution that has no terminal outcome.
    fn active_record(&self) -> Option<&CronExecutionRecord> {
        self.execution_history
            .iter()
            .rev()
            .find(|record| cron_record_is_incomplete(record))
    }

    /// Returns mutable access to the newest execution without a terminal outcome.
    fn active_record_mut(&mut self) -> Option<&mut CronExecutionRecord> {
        self.execution_history
            .iter_mut()
            .rev()
            .find(|record| cron_record_is_incomplete(record))
    }

    /// Recalculates the next execution time based on the cron schedule and timezone.
    pub fn update_next_execution(&mut self) {
        self.next_execution = compute_next_execution(&self.schedule, self.timezone);
    }

    /// Marks stale unfinished executions interrupted while retaining one live run.
    fn close_stale_running_executions(&mut self, live_execution: Option<SystemTime>) {
        for record in self
            .execution_history
            .iter_mut()
            .filter(|record| cron_record_is_incomplete(record))
        {
            if live_execution
                .is_some_and(|started_at| same_run(record.started_at, started_at))
            {
                continue;
            }
            record.completed_at = None;
            record.status = Some(CronExecutionStatus::Interrupted(
                STALE_CRON_INTERRUPTION_REASON.to_string(),
            ));
        }
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

/// Computes the next execution time for a cron schedule in the given timezone.
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
///
/// Jobs from every project share one scheduler loop, but each job persists to
/// and restores from **its own** project's state directory — `stores` maps a
/// project id to that directory so no project's cron history can leak into
/// another's file.
#[derive(Clone)]
pub struct CronManager {
    jobs: Arc<Mutex<Vec<CronJobState>>>,
    stores: Arc<Mutex<HashMap<String, StateStore>>>,
}

impl Default for CronManager {
    /// Returns the default this item, seeded with the loose project store.
    fn default() -> Self {
        Self::for_store(StateStore::loose())
    }
}

impl CronManager {
    /// Creates a cron manager seeded with a single project's state store.
    pub fn for_store(store: StateStore) -> Self {
        let mut stores = HashMap::new();
        stores.insert(String::new(), store);
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            stores: Arc::new(Mutex::new(stores)),
        }
    }

    /// Creates a new cron manager seeded with the loose project store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a project's state store so its cron jobs persist to their own
    /// directory. Idempotent.
    pub fn register_store(&self, project_id: &str, store: StateStore) {
        lock_recover(&self.stores).insert(project_id.to_string(), store);
    }

    /// The state store for a project, falling back to a project-derived store
    /// if one was never registered.
    fn store_for(&self, project_id: &str) -> StateStore {
        lock_recover(&self.stores)
            .get(project_id)
            .cloned()
            .unwrap_or_else(|| StateStore::for_project(project_id))
    }

    /// Builds a CronJobState from service configuration and optionally restores persisted state.
    fn build_job_state(
        &self,
        project_id: &str,
        service_name: &str,
        service_hash: &str,
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

        let persisted_state = CronStateFile::load(self.store_for(project_id))
            .ok()
            .and_then(|state| state.jobs().get(service_hash).cloned());

        let job_state = CronJobState::new(
            project_id.to_string(),
            service_name.to_string(),
            service_hash.to_string(),
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
        project_id: &str,
        service_name: &str,
        service_hash: &str,
        cron_config: &CronConfig,
    ) -> Result<(), ProcessManagerError> {
        let (job_state, normalized, normalized_expression) =
            self.build_job_state(project_id, service_name, service_hash, cron_config)?;
        let timezone_label = job_state.timezone_label.clone();
        let mut jobs = lock_recover(&self.jobs);
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
        self.sync_from_configs(std::iter::once(config))
    }

    /// Replace all cron jobs using the provided configurations.
    pub fn sync_from_configs<'a, I>(&self, configs: I) -> Result<(), ProcessManagerError>
    where
        I: IntoIterator<Item = &'a Config>,
    {
        let mut active_jobs = Vec::new();

        for config in configs {
            let project_id = config.project.id.clone();
            self.register_store(&project_id, StateStore::for_project(&project_id));
            for (service_name, service_config) in &config.services {
                if let Some(cron_config) = &service_config.cron {
                    let service_hash = config.state_key(service_name);
                    let (job_state, normalized, normalized_expression) = self
                        .build_job_state(
                            &project_id,
                            service_name,
                            &service_hash,
                            cron_config,
                        )?;
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

                    info!("Registered cron job for service '{}'", service_name);
                    active_jobs.push(job_state);
                }
            }
        }

        {
            let mut jobs_guard = lock_recover(&self.jobs);
            *jobs_guard = active_jobs;
        }

        Ok(())
    }

    /// Check if any cron jobs are due to run and return their service names.
    pub fn get_due_jobs(&self) -> Vec<String> {
        self.get_due_job_refs()
            .into_iter()
            .map(|job| job.service_name)
            .collect()
    }

    /// Check if any cron jobs are due to run and return their stable identities.
    pub fn get_due_job_refs(&self) -> Vec<CronDueJob> {
        let mut jobs = lock_recover(&self.jobs);
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
                    let record = CronExecutionRecord {
                        started_at: now,
                        completed_at: Some(now),
                        status: Some(CronExecutionStatus::OverlapError),
                        exit_code: None,
                        pid: None,
                        process_start: None,
                        user: None,
                        command: None,
                        metrics: vec![],
                    };
                    job.add_execution_record(record);
                    job.update_next_execution();
                    self.persist_job_state(job);
                    continue;
                }

                {
                    due_jobs.push(CronDueJob {
                        service_name: job.service_name.clone(),
                        service_hash: job.service_hash.clone(),
                        started_at: now,
                    });
                    job.currently_running = true;
                    job.last_execution = Some(now);

                    let record = CronExecutionRecord {
                        started_at: now,
                        completed_at: None,
                        status: None,
                        exit_code: None,
                        pid: None,
                        process_start: None,
                        user: None,
                        command: None,
                        metrics: vec![],
                    };
                    job.add_execution_record(record);
                    job.update_next_execution();
                    self.persist_job_state(job);
                }
            }
        }

        due_jobs
    }

    /// Removes all scheduled jobs owned by one project.
    pub fn remove_project_jobs(&self, project_id: &str) {
        lock_recover(&self.jobs).retain(|job| job.project_id != project_id);
    }

    /// Returns whether a scheduled job still owns the supplied stable hash.
    pub fn contains_job_hash(&self, service_hash: &str) -> bool {
        lock_recover(&self.jobs)
            .iter()
            .any(|job| job.service_hash == service_hash)
    }

    /// Mark a cron job as completed.
    pub fn mark_job_completed(
        &self,
        service_name: &str,
        status: CronExecutionStatus,
        exit_code: Option<i32>,
        metrics: Vec<crate::metrics::MetricSample>,
    ) {
        self.mark_job_completed_by(
            |job| job.service_name == service_name,
            None,
            status,
            exit_code,
            metrics,
        );
    }

    /// Mark a cron job as completed by service hash.
    pub fn mark_job_completed_by_hash(
        &self,
        service_hash: &str,
        status: CronExecutionStatus,
        exit_code: Option<i32>,
        metrics: Vec<crate::metrics::MetricSample>,
    ) {
        self.mark_job_completed_by(
            |job| job.service_hash == service_hash,
            None,
            status,
            exit_code,
            metrics,
        );
    }

    /// Completes the execution created at `started_at` without mutating a newer run.
    pub fn complete_job_run(
        &self,
        service_hash: &str,
        started_at: SystemTime,
        status: CronExecutionStatus,
        exit_code: Option<i32>,
        metrics: Vec<crate::metrics::MetricSample>,
    ) {
        self.mark_job_completed_by(
            |job| job.service_hash == service_hash,
            Some(started_at),
            status,
            exit_code,
            metrics,
        );
    }

    /// Mark a cron job matching predicate as completed.
    fn mark_job_completed_by<F>(
        &self,
        matches_job: F,
        started_at: Option<SystemTime>,
        status: CronExecutionStatus,
        exit_code: Option<i32>,
        metrics: Vec<crate::metrics::MetricSample>,
    ) where
        F: Fn(&CronJobState) -> bool,
    {
        let mut jobs = lock_recover(&self.jobs);
        if let Some(job) = jobs.iter_mut().find(|job| matches_job(job)) {
            let active = job.active_record().map(|record| record.started_at);
            let target = started_at.map_or_else(
                || {
                    job.execution_history
                        .iter()
                        .rposition(cron_record_is_incomplete)
                },
                |started| {
                    job.execution_history
                        .iter()
                        .rposition(|record| same_run(record.started_at, started))
                },
            );
            if let Some(index) = target
                && let Some(mut record) = job.execution_history.remove(index)
            {
                let completed_active =
                    active.is_some_and(|active| same_run(active, record.started_at));
                record.completed_at = Some(SystemTime::now());
                record.status = Some(status);
                record.exit_code = exit_code;
                record.metrics = metrics;
                job.execution_history.push_back(record);
                if completed_active {
                    job.currently_running = false;
                }
            }

            debug!("Cron job '{}' completed", job.service_name);
            self.persist_job_state(job);
        }
    }

    /// Annotate the most recent execution record with runtime metadata captured after spawn.
    pub fn annotate_job_execution(
        &self,
        service_name: &str,
        pid: Option<u32>,
        user: Option<String>,
        command: Option<String>,
    ) {
        self.annotate_job_execution_by(
            |job| job.service_name == service_name,
            None,
            pid,
            user,
            command,
        );
    }

    /// Annotate the most recent execution record by service hash.
    pub fn annotate_job_execution_by_hash(
        &self,
        service_hash: &str,
        pid: Option<u32>,
        user: Option<String>,
        command: Option<String>,
    ) {
        self.annotate_job_execution_by(
            |job| job.service_hash == service_hash,
            None,
            pid,
            user,
            command,
        );
    }

    /// Annotates the execution created at `started_at` without mutating a newer run.
    pub fn annotate_job_run(
        &self,
        service_hash: &str,
        started_at: SystemTime,
        pid: Option<u32>,
        user: Option<String>,
        command: Option<String>,
    ) {
        self.annotate_job_execution_by(
            |job| job.service_hash == service_hash,
            Some(started_at),
            pid,
            user,
            command,
        );
    }

    /// Annotate the most recent execution record matching predicate.
    fn annotate_job_execution_by<F>(
        &self,
        matches_job: F,
        started_at: Option<SystemTime>,
        pid: Option<u32>,
        user: Option<String>,
        command: Option<String>,
    ) where
        F: Fn(&CronJobState) -> bool,
    {
        let mut jobs = lock_recover(&self.jobs);
        if let Some(job) = jobs.iter_mut().find(|job| matches_job(job)) {
            let record = match started_at {
                Some(started) => job
                    .execution_history
                    .iter_mut()
                    .rev()
                    .find(|record| same_run(record.started_at, started)),
                None => job.active_record_mut(),
            };
            let Some(record) = record else {
                return;
            };
            if pid.is_some() {
                record.pid = pid;
                record.process_start = pid.and_then(crate::daemon::process_start_time);
            }
            if user.is_some() {
                record.user = user;
            }
            if command.is_some() {
                record.command = command;
            }
            self.persist_job_state(job);
        }
    }

    /// Get the state of all cron jobs (for status display).
    pub fn get_all_jobs(&self) -> Vec<CronJobState> {
        lock_recover(&self.jobs).clone()
    }

    /// Clear all registered cron jobs.
    pub fn clear_all_jobs(&self) {
        let mut jobs = lock_recover(&self.jobs);
        jobs.clear();
    }

    /// Get the last execution status for a specific cron job (for testing).
    pub fn get_last_execution_status(
        &self,
        service_name: &str,
    ) -> Option<CronExecutionStatus> {
        let jobs = lock_recover(&self.jobs);
        if let Some(job) = jobs.iter().find(|j| j.service_name == service_name) {
            job.execution_history
                .back()
                .and_then(|record| record.status.clone())
        } else {
            None
        }
    }

    /// Persists the state of a cron job to its own project's state directory.
    ///
    /// Reads the project's current cron file, upserts just this job, and writes
    /// it back — so sibling jobs in the same project are never clobbered, and no
    /// other project's file is touched.
    fn persist_job_state(&self, job: &CronJobState) {
        let store = self.store_for(&job.project_id);
        let state = PersistedCronJobState {
            service_name: Some(job.service_name.clone()),
            last_execution: job.last_execution,
            execution_history: job.execution_history.clone(),
            timezone_label: job.timezone_label.clone(),
            timezone: match job.timezone {
                EffectiveTimezone::Local => None,
                EffectiveTimezone::Utc => Some("UTC".to_string()),
                EffectiveTimezone::Named(tz) => Some(tz.name().to_string()),
            },
        };
        if let Err(err) = CronStateFile::upsert(store, &job.service_hash, state) {
            warn!(
                "Failed to persist cron state for '{}': {}",
                job.service_name, err
            );
        }
    }
}

/// Wrapper for cron job entries to make them XML-safe
#[derive(Debug, Serialize, Deserialize, Clone)]
struct CronJobEntry {
    hash: String,
    state: PersistedCronJobState,
}

/// Persistent storage for cron job state across supervisor restarts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronStateFile {
    #[serde(
        serialize_with = "serialize_cron_jobs",
        deserialize_with = "deserialize_cron_jobs"
    )]
    jobs: std::collections::BTreeMap<String, PersistedCronJobState>,
    /// The project state directory this file is bound to. Never serialized;
    /// re-attached after every load.
    #[serde(skip)]
    store: StateStore,
}

/// Serializes cron jobs.
fn serialize_cron_jobs<S>(
    map: &std::collections::BTreeMap<String, PersistedCronJobState>,
    s: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(map.len()))?;
    for (k, v) in map {
        seq.serialize_element(&CronJobEntry {
            hash: k.clone(),
            state: v.clone(),
        })?;
    }
    seq.end()
}

/// Handles deserialize cron jobs.
fn deserialize_cron_jobs<'de, D>(
    d: D,
) -> Result<std::collections::BTreeMap<String, PersistedCronJobState>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries: Vec<CronJobEntry> = Vec::deserialize(d)?;
    Ok(entries.into_iter().map(|e| (e.hash, e.state)).collect())
}

impl CronStateFile {
    /// Returns the path to the cron state file.
    fn path(&self) -> PathBuf {
        self.store.cron_path()
    }

    fn lock(store: &StateStore) -> Result<fs::File, std::io::Error> {
        let path = store.cron_lock_path();
        if let Some(parent) = path.parent() {
            crate::runtime::create_private_dir(parent)?;
        }
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
    }

    fn write(&self) -> Result<(), std::io::Error> {
        let path = self.path();
        if let Some(parent) = path.parent() {
            crate::runtime::create_private_dir(parent)?;
        }
        let data = quick_xml::se::to_string(self).map_err(std::io::Error::other)?;
        crate::runtime::write_private_file(&path, data)
    }

    /// The project store this file is bound to.
    pub fn store(&self) -> StateStore {
        self.store.clone()
    }

    /// Loads the cron state file from disk, creating an empty one if it doesn't exist.
    pub fn load(store: StateStore) -> Result<Self, std::io::Error> {
        let lock = Self::lock(&store)?;
        FileExt::lock_shared(&lock)?;
        Self::read(store)
    }

    fn read(store: StateStore) -> Result<Self, std::io::Error> {
        let empty = || Self {
            store: store.clone(),
            ..Self::default()
        };
        let path = store.cron_path();
        if !path.exists() {
            return Ok(empty());
        }

        let raw = fs::read_to_string(&path)?;

        if raw.trim().is_empty() || raw.trim() == "<CronStateFile/>" {
            return Ok(empty());
        }

        let mut state = quick_xml::de::from_str::<Self>(&raw).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to deserialize {}: {err}", path.display()),
            )
        })?;
        state.store = store;
        Ok(state)
    }

    fn upsert(
        store: StateStore,
        hash: &str,
        job: PersistedCronJobState,
    ) -> Result<(), std::io::Error> {
        let lock = Self::lock(&store)?;
        FileExt::lock_exclusive(&lock)?;
        let mut state = Self::read(store)?;
        state.jobs.insert(hash.to_string(), job);
        state.write()
    }

    /// Returns a reference to the map of persisted cron job states.
    /// Keys are service configuration hashes (not service names).
    pub fn jobs(&self) -> &std::collections::BTreeMap<String, PersistedCronJobState> {
        &self.jobs
    }
}

/// Serializable cron job state that persists across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedCronJobState {
    /// Name of the service this cron job manages.
    #[serde(default)]
    pub service_name: Option<String>,
    /// Timestamp of the last execution start.
    #[serde(with = "systemtime_serde_opt", default)]
    pub last_execution: Option<SystemTime>,
    /// Rolling history of recent executions.
    #[serde(default)]
    pub execution_history: VecDeque<CronExecutionRecord>,
    /// Human-readable timezone label.
    #[serde(default)]
    pub timezone_label: String,
    /// Optional timezone string (e.g., "UTC", "America/New_York").
    #[serde(default)]
    pub timezone: Option<String>,
}

impl Default for PersistedCronJobState {
    /// Returns the default this item.
    fn default() -> Self {
        Self {
            service_name: None,
            last_execution: None,
            execution_history: VecDeque::with_capacity(MAX_EXECUTION_HISTORY),
            timezone_label: "".to_string(),
            timezone: None,
        }
    }
}

/// Normalizes a cron expression to 6 fields if needed.
/// Returns (normalized_expression, was_five_field).
fn normalize_cron_expression(expr: &str) -> (String, bool) {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    match parts.len() {
        5 => (format!("0 {}", parts.join(" ")), true),
        _ => (parts.join(" "), false),
    }
}

/// Resolves the timezone for a cron job from configuration.
/// Defaults to local timezone if not specified or invalid.
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
    use std::{
        collections::{HashMap, VecDeque},
        fs,
        time::{Duration, SystemTime},
    };

    use super::*;
    use crate::config::ServiceConfig;

    /// Computes a test hash for a cron configuration.
    fn compute_test_hash(cron_config: &CronConfig) -> String {
        let service_config = ServiceConfig {
            command: "test_command".to_string(),
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
            cron: Some(cron_config.clone()),
            skip: None,
            spawn: None,
            logs: None,
            project_scope: None,
        };
        service_config.compute_hash()
    }

    #[test]
    fn test_cron_manager_registration() {
        let manager = CronManager::new();
        let cron_config = CronConfig {
            expression: "0 * * * * *".to_string(),
            timezone: Some("UTC".into()),
        };
        let service_hash = compute_test_hash(&cron_config);

        assert!(
            manager
                .register_job("", "test_service", &service_hash, &cron_config)
                .is_ok()
        );

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
        let service_hash = compute_test_hash(&cron_config);

        assert!(
            manager
                .register_job("", "test_service", &service_hash, &cron_config)
                .is_err()
        );
    }

    #[test]
    fn test_five_field_expression_normalizes() {
        let manager = CronManager::new();
        let cron_config = CronConfig {
            expression: "* * * * *".to_string(),
            timezone: None,
        };
        let service_hash = compute_test_hash(&cron_config);

        assert!(
            manager
                .register_job("", "test_service", &service_hash, &cron_config)
                .is_ok()
        );
        let jobs = manager.get_all_jobs();
        assert!(jobs[0].next_execution.is_some());
    }

    #[test]
    fn restores_running_state_for_live_persisted_execution() {
        let schedule = Schedule::from_str("* * * * * *").expect("valid schedule");
        let current_pid = std::process::id();
        let mut history = VecDeque::new();
        history.push_back(CronExecutionRecord {
            started_at: SystemTime::now() - Duration::from_secs(30),
            completed_at: None,
            status: None,
            exit_code: None,
            pid: Some(current_pid),
            process_start: crate::daemon::process_start_time(current_pid),
            user: Some("rashad".to_string()),
            command: Some("/bin/true".to_string()),
            metrics: vec![],
        });

        let state = CronJobState::new(
            String::new(),
            "live_service".to_string(),
            "live-hash".to_string(),
            schedule,
            EffectiveTimezone::Utc,
            "UTC".to_string(),
            Some(PersistedCronJobState {
                service_name: Some("live_service".to_string()),
                last_execution: Some(SystemTime::now() - Duration::from_secs(30)),
                execution_history: history,
                timezone_label: "UTC".to_string(),
                timezone: Some("UTC".to_string()),
            }),
        );

        assert!(
            state.currently_running,
            "a live unfinished persisted run should remain marked as running"
        );
    }

    #[test]
    fn due_job_keeps_inflight_record_until_worker_completion() {
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
        crate::runtime::init_with_test_home(home);
        crate::runtime::set_drop_privileges(false);

        let manager = CronManager::new();
        let schedule = Schedule::from_str("* * * * * *").expect("valid schedule");
        let mut history = VecDeque::new();
        history.push_back(CronExecutionRecord {
            started_at: SystemTime::now() - Duration::from_secs(30),
            completed_at: None,
            status: None,
            exit_code: None,
            pid: Some(i32::MAX as u32),
            process_start: None,
            user: Some("rashad".to_string()),
            command: Some("/bin/true".to_string()),
            metrics: vec![],
        });
        let mut job = CronJobState::new(
            String::new(),
            "stale_service".to_string(),
            "stale-hash".to_string(),
            schedule,
            EffectiveTimezone::Utc,
            "UTC".to_string(),
            None,
        );
        job.currently_running = true;
        job.next_execution = Some(SystemTime::now() - Duration::from_secs(1));
        job.execution_history = history;

        {
            let mut jobs = manager.jobs.lock().unwrap();
            jobs.push(job);
        }

        let due = manager.get_due_job_refs();

        assert!(due.is_empty());

        let jobs = manager.jobs.lock().unwrap();
        let job = jobs.first().expect("job present");
        assert!(job.currently_running);
        assert_eq!(job.execution_history.len(), 2);
        let inflight = job.execution_history.front().expect("inflight record");
        assert!(cron_record_is_incomplete(inflight));
        let overlap = job.execution_history.back().expect("overlap record");
        assert!(matches!(
            overlap.status,
            Some(CronExecutionStatus::OverlapError)
        ));

        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    /// Verifies completed cron records persist exit and process metadata.
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
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let manager = CronManager::new();
        let cron_config = CronConfig {
            expression: "* * * * * *".to_string(),
            timezone: Some("UTC".into()),
        };
        let service_hash = compute_test_hash(&cron_config);

        manager
            .register_job("", "persisted_service", &service_hash, &cron_config)
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

        manager.annotate_job_execution(
            "persisted_service",
            Some(4242),
            Some("postgres".to_string()),
            Some("/bin/true".to_string()),
        );
        manager.mark_job_completed(
            "persisted_service",
            CronExecutionStatus::Success,
            Some(0),
            vec![],
        );

        let service_hash = compute_test_hash(&cron_config);
        let state = CronStateFile::load(StateStore::loose()).expect("load cron state");
        let persisted = state.jobs().get(&service_hash).expect("persisted cron job");

        assert_eq!(persisted.execution_history.len(), 1);
        let record = persisted.execution_history.back().unwrap();
        assert!(matches!(record.status, Some(CronExecutionStatus::Success)));
        assert_eq!(record.exit_code, Some(0));
        assert_eq!(record.pid, Some(4242));
        assert_eq!(record.user.as_deref(), Some("postgres"));
        assert_eq!(record.command.as_deref(), Some("/bin/true"));

        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    /// Creates a test service with a cron configuration.
    fn service_with_cron(expr: &str) -> ServiceConfig {
        ServiceConfig {
            command: "/bin/true".into(),
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
            cron: Some(CronConfig {
                expression: expr.to_string(),
                timezone: None,
            }),
            skip: None,
            spawn: None,
            logs: None,
            project_scope: None,
        }
    }

    #[test]
    fn sync_from_config_removes_inactive_jobs_without_deleting_history() {
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
        crate::runtime::init_with_test_home(home);
        crate::runtime::set_drop_privileges(false);

        let manager = CronManager::new();

        let mut services_v1 = HashMap::new();
        services_v1.insert("job_one".to_string(), service_with_cron("* * * * * *"));
        services_v1.insert("job_two".to_string(), service_with_cron("*/2 * * * * *"));
        let config_v1 = Config {
            version: crate::config::Version::V2,
            project: crate::config::ProjectConfig::default(),
            services: services_v1,
            project_dir: None,
            env: None,
            metrics: crate::config::MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        manager.sync_from_config(&config_v1).unwrap();

        let mut services_v2 = HashMap::new();
        services_v2.insert("job_two".to_string(), service_with_cron("*/2 * * * * *"));
        services_v2.insert("job_three".to_string(), service_with_cron("0 */5 * * * *"));
        let config_v2 = Config {
            version: crate::config::Version::V2,
            project: crate::config::ProjectConfig::default(),
            services: services_v2,
            project_dir: None,
            env: None,
            metrics: crate::config::MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        let job_two_hash = config_v2.state_key("job_two");
        let job_three_hash = config_v2.state_key("job_three");
        let job_one_hash = config_v1.state_key("job_one");

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

        let state = CronStateFile::load(StateStore::loose()).expect("load cron state");
        assert!(state.jobs().contains_key(&job_two_hash));
        assert!(state.jobs().contains_key(&job_three_hash));
        assert!(
            state.jobs().contains_key(&job_one_hash),
            "inactive cron state should remain persisted for history restoration"
        );

        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn cron_execution_status_accepts_text_compat_shape() {
        let status: CronExecutionStatus = serde_json::from_str(r#"{"$text":"Success"}"#)
            .expect("deserialize compat text status");
        assert!(matches!(status, CronExecutionStatus::Success));
    }

    #[test]
    fn cron_state_deserializes_legacy_text_status_entries() {
        let mut state = CronStateFile::default();
        let mut history = VecDeque::new();
        history.push_back(CronExecutionRecord {
            started_at: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            completed_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(12)),
            status: Some(CronExecutionStatus::Success),
            exit_code: Some(0),
            pid: None,
            process_start: None,
            user: None,
            command: None,
            metrics: vec![],
        });

        state.jobs.insert(
            "legacy-hash".to_string(),
            PersistedCronJobState {
                service_name: Some("legacy_service".to_string()),
                last_execution: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(10)),
                execution_history: history,
                timezone_label: "UTC".to_string(),
                timezone: Some("UTC".to_string()),
            },
        );

        let xml = quick_xml::se::to_string(&state).expect("serialize cron state");
        let parsed: CronStateFile =
            quick_xml::de::from_str(&xml).expect("deserialize legacy state");
        let record = parsed
            .jobs()
            .get("legacy-hash")
            .and_then(|job| job.execution_history.back())
            .expect("legacy record present");
        assert!(matches!(record.status, Some(CronExecutionStatus::Success)));
    }

    #[test]
    fn cron_state_round_trips_running_execution_without_status() {
        let mut state = CronStateFile::default();
        let mut history = VecDeque::new();
        history.push_back(CronExecutionRecord {
            started_at: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            completed_at: None,
            status: None,
            exit_code: None,
            pid: Some(1234),
            process_start: None,
            user: Some("ubuntu".to_string()),
            command: Some("/bin/true".to_string()),
            metrics: vec![],
        });

        state.jobs.insert(
            "running-hash".to_string(),
            PersistedCronJobState {
                service_name: Some("running_service".to_string()),
                last_execution: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(20)),
                execution_history: history,
                timezone_label: "UTC".to_string(),
                timezone: Some("UTC".to_string()),
            },
        );

        let xml = quick_xml::se::to_string(&state).expect("serialize cron state");
        assert!(
            !xml.contains("<status"),
            "in-progress records should omit status instead of writing an empty status element"
        );

        let parsed: CronStateFile =
            quick_xml::de::from_str(&xml).expect("deserialize running state");
        let record = parsed
            .jobs()
            .get("running-hash")
            .and_then(|job| job.execution_history.back())
            .expect("running record present");
        assert!(record.status.is_none());
        assert_eq!(record.exit_code, None);
        assert_eq!(record.pid, Some(1234));
    }
}
