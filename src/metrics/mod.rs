#![allow(missing_docs)]
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::Write,
    mem,
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use thiserror::Error;
use tracing::error;

use crate::{
    config::Config,
    daemon::{PidFile, ServiceStateFile},
};

const DEFAULT_RETENTION_MINUTES: u64 = 720; // 12 hours
const DEFAULT_SAMPLE_INTERVAL_SECS: u64 = 1;
const DEFAULT_MAX_MEMORY_BYTES: usize = 10 * 1024 * 1024; // 10 MB

/// Sample collected for a managed unit at a specific timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    pub timestamp: DateTime<Utc>,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
}

/// Summary statistics derived from recent samples.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSummary {
    pub latest_cpu_percent: f32,
    pub average_cpu_percent: f32,
    pub max_cpu_percent: f32,
    pub latest_rss_bytes: u64,
    pub samples: usize,
}

/// Configuration for runtime metrics collection.
#[derive(Debug, Clone)]
pub struct MetricsSettings {
    pub retention: Duration,
    pub sample_interval: Duration,
    pub max_memory_bytes: usize,
    pub spillover: Option<SpilloverSettings>,
}

impl Default for MetricsSettings {
    fn default() -> Self {
        Self {
            retention: Duration::from_secs(DEFAULT_RETENTION_MINUTES * 60),
            sample_interval: Duration::from_secs(DEFAULT_SAMPLE_INTERVAL_SECS),
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            spillover: None,
        }
    }
}

/// Spillover configuration used to persist evicted samples to disk.
#[derive(Debug, Clone)]
pub struct SpilloverSettings {
    pub directory: PathBuf,
    pub max_bytes: u64,
    pub segment_bytes: u64,
}

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("failed to create spillover directory: {0}")]
    CreateDir(std::io::Error),
    #[error("failed to write spillover segment: {0}")]
    SpilloverWrite(std::io::Error),
    #[error("failed to serialise spillover record: {0}")]
    SpilloverSerialize(serde_json::Error),
}

#[derive(Debug, Clone, Default)]
struct UnitMetrics {
    samples: VecDeque<MetricSample>,
    estimated_bytes: usize,
}

/// Thread-safe handle for interacting with metrics storage.
pub type MetricsHandle = Arc<RwLock<MetricsStore>>;

/// In-memory storage for recently collected metrics with bounded memory usage.
#[derive(Debug)]
pub struct MetricsStore {
    settings: MetricsSettings,
    total_estimated_bytes: usize,
    units: HashMap<String, UnitMetrics>,
    spillover: Option<MetricsSpillover>,
}

impl MetricsStore {
    pub fn new(settings: MetricsSettings) -> Result<MetricsStore, MetricsError> {
        let spillover = match settings.spillover.clone() {
            Some(spill) => Some(MetricsSpillover::new(&spill)?),
            None => None,
        };

        Ok(Self {
            settings,
            total_estimated_bytes: 0,
            units: HashMap::new(),
            spillover,
        })
    }

    /// Ensures a unit hash is present in the metrics store.
    pub fn register_unit(&mut self, unit_hash: &str) {
        self.units.entry(unit_hash.to_string()).or_default();
    }

    /// Removes all metrics history for the given unit hash.
    pub fn remove_unit(&mut self, unit_hash: &str) {
        if let Some(buffer) = self.units.remove(unit_hash) {
            self.total_estimated_bytes = self
                .total_estimated_bytes
                .saturating_sub(buffer.estimated_bytes);
        }
    }

    /// Records a new sample for the provided unit, pruning data outside the retention
    /// window and enforcing the configured memory budget.
    pub fn record_sample(
        &mut self,
        unit_hash: &str,
        sample: MetricSample,
    ) -> Result<(), MetricsError> {
        let retention_duration = ChronoDuration::from_std(self.settings.retention)
            .unwrap_or_else(|_| {
                ChronoDuration::minutes(DEFAULT_RETENTION_MINUTES as i64)
            });
        let retention_cutoff = sample
            .timestamp
            .checked_sub_signed(retention_duration)
            .unwrap_or(DateTime::<Utc>::MIN_UTC);

        let buffer = self.units.entry(unit_hash.to_string()).or_default();

        // Estimate sample size (fixed struct + hashed key overhead negligible).
        let sample_estimated_bytes = mem::size_of::<MetricSample>();
        buffer.samples.push_back(sample.clone());
        buffer.estimated_bytes = buffer
            .estimated_bytes
            .saturating_add(sample_estimated_bytes);
        self.total_estimated_bytes = self
            .total_estimated_bytes
            .saturating_add(sample_estimated_bytes);

        // Retention pruning.
        while let Some(front) = buffer.samples.front() {
            if front.timestamp >= retention_cutoff {
                break;
            }

            if let Some(evicted) = buffer.samples.pop_front() {
                buffer.estimated_bytes = buffer
                    .estimated_bytes
                    .saturating_sub(sample_estimated_bytes);
                self.total_estimated_bytes = self
                    .total_estimated_bytes
                    .saturating_sub(sample_estimated_bytes);
                if let Some(spillover) = self.spillover.as_mut() {
                    spillover.persist(unit_hash, &evicted)?;
                }
            }
        }

        self.enforce_memory_budget()?;
        Ok(())
    }

    pub fn retention(&self) -> Duration {
        self.settings.retention
    }

    pub fn sample_interval(&self) -> Duration {
        self.settings.sample_interval
    }

    fn enforce_memory_budget(&mut self) -> Result<(), MetricsError> {
        if self.total_estimated_bytes <= self.settings.max_memory_bytes {
            return Ok(());
        }

        // Purge oldest samples across units until within budget.
        let mut unit_keys: Vec<String> = self.units.keys().cloned().collect();
        unit_keys.sort();
        while self.total_estimated_bytes > self.settings.max_memory_bytes {
            let mut removed_any = false;
            for key in unit_keys.iter() {
                if let Some(buffer) = self.units.get_mut(key)
                    && let Some(sample) = buffer.samples.pop_front()
                {
                    let sample_estimated_bytes = mem::size_of::<MetricSample>();
                    buffer.estimated_bytes = buffer
                        .estimated_bytes
                        .saturating_sub(sample_estimated_bytes);
                    self.total_estimated_bytes = self
                        .total_estimated_bytes
                        .saturating_sub(sample_estimated_bytes);
                    if let Some(spillover) = self.spillover.as_mut() {
                        spillover.persist(key, &sample)?;
                    }
                    removed_any = true;
                }
                if self.total_estimated_bytes <= self.settings.max_memory_bytes {
                    break;
                }
            }

            if !removed_any {
                break;
            }
        }

        Ok(())
    }

    /// Returns the recent samples for a unit without cloning the entire store.
    pub fn snapshot_unit(&self, unit_hash: &str) -> Option<Vec<MetricSample>> {
        self.units
            .get(unit_hash)
            .map(|buffer| buffer.samples.iter().cloned().collect())
    }

    /// Returns a copy of the most recent samples limited to `limit` entries.
    pub fn latest_samples(&self, unit_hash: &str, limit: usize) -> Vec<MetricSample> {
        self.units
            .get(unit_hash)
            .map(|buffer| {
                buffer
                    .samples
                    .iter()
                    .rev()
                    .take(limit)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Produces summary statistics for the requested unit.
    pub fn summarize_unit(&self, unit_hash: &str) -> Option<MetricsSummary> {
        let buffer = self.units.get(unit_hash)?;
        if buffer.samples.is_empty() {
            return None;
        }

        let samples = buffer.samples.len();
        let latest = buffer.samples.back()?;
        let sum_cpu: f32 = buffer.samples.iter().map(|sample| sample.cpu_percent).sum();
        let max_cpu = buffer
            .samples
            .iter()
            .fold(0.0_f32, |acc, sample| acc.max(sample.cpu_percent));

        Some(MetricsSummary {
            latest_cpu_percent: latest.cpu_percent,
            average_cpu_percent: sum_cpu / samples as f32,
            max_cpu_percent: max_cpu,
            latest_rss_bytes: latest.rss_bytes,
            samples,
        })
    }
}

/// Persists evicted metrics samples to disk for later inspection.
#[derive(Debug)]
struct MetricsSpillover {
    base: PathBuf,
    max_bytes: u64,
    segment_bytes: u64,
    total_bytes: u64,
    segments: VecDeque<SegmentMeta>,
    current: Option<SegmentWriter>,
}

#[derive(Debug)]
struct SegmentMeta {
    path: PathBuf,
    bytes: u64,
}

#[derive(Debug)]
struct SegmentWriter {
    file: fs::File,
    path: PathBuf,
    bytes_written: u64,
}

impl MetricsSpillover {
    fn new(settings: &SpilloverSettings) -> Result<Self, MetricsError> {
        if !settings.directory.exists() {
            fs::create_dir_all(&settings.directory).map_err(MetricsError::CreateDir)?;
        }

        let mut segments = VecDeque::new();
        let mut total_bytes: u64 = 0;

        if let Ok(entries) = fs::read_dir(&settings.directory) {
            let mut existing: Vec<_> = entries
                .flatten()
                .filter(|entry| entry.file_type().map(|ft| ft.is_file()).unwrap_or(false))
                .collect();
            existing.sort_by_key(|entry| entry.path());
            for entry in existing {
                let path = entry.path();
                if let Ok(metadata) = entry.metadata() {
                    let bytes = metadata.len();
                    segments.push_back(SegmentMeta { path, bytes });
                    total_bytes = total_bytes.saturating_add(bytes);
                }
            }
        }

        Ok(Self {
            base: settings.directory.clone(),
            max_bytes: settings.max_bytes,
            segment_bytes: settings.segment_bytes,
            total_bytes,
            segments,
            current: None,
        })
    }

    fn persist(
        &mut self,
        unit_hash: &str,
        sample: &MetricSample,
    ) -> Result<(), MetricsError> {
        let record = serde_json::to_vec(&SpilloverRecord { unit_hash, sample })
            .map_err(MetricsError::SpilloverSerialize)?;
        let bytes_written = (record.len() + 1) as u64;
        let mut should_rotate = false;

        {
            let writer = self.ensure_writer()?;
            writer
                .file
                .write_all(&record)
                .map_err(MetricsError::SpilloverWrite)?;
            writer
                .file
                .write_all(b"\n")
                .map_err(MetricsError::SpilloverWrite)?;
            writer.bytes_written += bytes_written;
            if writer.bytes_written >= self.segment_bytes {
                should_rotate = true;
            }
        }

        self.total_bytes = self.total_bytes.saturating_add(bytes_written);

        if should_rotate {
            self.rotate_segment()?;
        }

        self.enforce_budget()?;
        Ok(())
    }

    fn ensure_writer(&mut self) -> Result<&mut SegmentWriter, MetricsError> {
        if self.current.is_none() {
            let timestamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let path = self.base.join(format!("metrics-{timestamp}.jsonl"));
            let file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(MetricsError::SpilloverWrite)?;
            self.current = Some(SegmentWriter {
                file,
                path: path.clone(),
                bytes_written: 0,
            });
            self.segments.push_back(SegmentMeta { path, bytes: 0 });
        }

        Ok(self.current.as_mut().unwrap())
    }

    fn rotate_segment(&mut self) -> Result<(), MetricsError> {
        if let Some(current) = self.current.take()
            && let Some(meta) = self.segments.back_mut()
        {
            meta.bytes = meta.bytes.saturating_add(current.bytes_written);
        }
        Ok(())
    }

    fn enforce_budget(&mut self) -> Result<(), MetricsError> {
        while self.total_bytes > self.max_bytes {
            if let Some(meta) = self.segments.pop_front() {
                if self.current.as_ref().map(|w| w.path.clone())
                    == Some(meta.path.clone())
                {
                    // Can't remove active segment; rotate first.
                    self.rotate_segment()?;
                    if let Some(writer) = self.current.take()
                        && let Some(back) = self.segments.back_mut()
                    {
                        back.bytes = back.bytes.saturating_add(writer.bytes_written);
                    }
                }
                if let Ok(metadata) = fs::metadata(&meta.path) {
                    self.total_bytes = self.total_bytes.saturating_sub(metadata.len());
                }
                let _ = fs::remove_file(&meta.path);
            } else {
                break;
            }
        }

        Ok(())
    }
}

#[derive(Serialize)]
struct SpilloverRecord<'a> {
    unit_hash: &'a str,
    sample: &'a MetricSample,
}

pub fn shared_store(settings: MetricsSettings) -> Result<MetricsHandle, MetricsError> {
    Ok(Arc::new(RwLock::new(MetricsStore::new(settings)?)))
}

/// Unit metadata used by the collector to emit samples.
#[derive(Debug)]
pub struct UnitTarget {
    pub hash: String,
    pub pid: Option<u32>,
}

/// Result of sampling a unit in the collector.
#[derive(Debug)]
pub struct CollectedSample {
    pub hash: String,
    pub sample: MetricSample,
}

/// Background worker that periodically collects metrics for running units.
pub struct MetricsCollector {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MetricsCollector {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        store: MetricsHandle,
        config: Arc<Config>,
        pid_file: Arc<Mutex<PidFile>>,
        service_state: Arc<Mutex<ServiceStateFile>>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let store_clone = Arc::clone(&store);

        let interval = {
            store
                .read()
                .map(|guard| guard.sample_interval())
                .unwrap_or_else(|_| Duration::from_secs(DEFAULT_SAMPLE_INTERVAL_SECS))
        };

        let handle = thread::spawn(move || {
            let mut system = System::new();

            while !stop_clone.load(Ordering::SeqCst) {
                let targets =
                    gather_unit_targets(config.as_ref(), &pid_file, &service_state);

                let mut collected = Vec::with_capacity(targets.len());
                for target in targets {
                    let sample = if let Some(pid) = target.pid {
                        sample_process(&mut system, pid)
                    } else {
                        missing_process_sample()
                    };
                    collected.push(CollectedSample {
                        hash: target.hash,
                        sample,
                    });
                }

                if let Ok(mut guard) = store_clone.write() {
                    for entry in collected {
                        guard.register_unit(&entry.hash);
                        if let Err(err) = guard.record_sample(&entry.hash, entry.sample) {
                            error!("failed to record metrics sample: {err}");
                        }
                    }
                }

                let mut slept = Duration::ZERO;
                while slept < interval {
                    if stop_clone.load(Ordering::SeqCst) {
                        return;
                    }
                    let remaining = interval.saturating_sub(slept);
                    let step = if remaining > Duration::from_millis(100) {
                        Duration::from_millis(100)
                    } else {
                        remaining
                    };
                    thread::sleep(step);
                    slept += step;
                }
            }
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for MetricsCollector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn gather_unit_targets(
    config: &Config,
    pid_file: &Arc<Mutex<PidFile>>,
    service_state: &Arc<Mutex<ServiceStateFile>>,
) -> Vec<UnitTarget> {
    let pid_guard = pid_file.lock().unwrap();
    let state_guard = service_state.lock().unwrap();

    let mut targets = Vec::new();
    let mut seen_hashes = Vec::new();

    for (service_name, service_config) in &config.services {
        let hash = service_config.compute_hash();
        let pid = state_guard
            .get(&hash)
            .and_then(|entry| entry.pid)
            .or_else(|| pid_guard.pid_for(service_name));
        targets.push(UnitTarget {
            hash: hash.clone(),
            pid,
        });
        seen_hashes.push(hash);
    }

    for (hash, entry) in state_guard.services() {
        if seen_hashes.contains(hash) {
            continue;
        }
        targets.push(UnitTarget {
            hash: hash.clone(),
            pid: entry.pid,
        });
    }

    targets
}

fn sample_process(system: &mut System, pid: u32) -> MetricSample {
    let pid_sys = Pid::from_u32(pid);
    let refresh_kind = ProcessRefreshKind::everything();
    let processes = [pid_sys];
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&processes),
        true,
        refresh_kind,
    );

    if let Some(process) = system.process(pid_sys) {
        MetricSample {
            timestamp: Utc::now(),
            cpu_percent: process.cpu_usage(),
            rss_bytes: process.memory() * 1024,
            io_read_bytes: 0,
            io_write_bytes: 0,
            net_rx_bytes: 0,
            net_tx_bytes: 0,
        }
    } else {
        missing_process_sample()
    }
}

fn missing_process_sample() -> MetricSample {
    MetricSample {
        timestamp: Utc::now(),
        cpu_percent: 0.0,
        rss_bytes: 0,
        io_read_bytes: 0,
        io_write_bytes: 0,
        net_rx_bytes: 0,
        net_tx_bytes: 0,
    }
}
