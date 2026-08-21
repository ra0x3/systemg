#![allow(missing_docs)]
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{self, Write},
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
    constants::PROCESS_CHECK_INTERVAL,
    daemon::{PidFile, ServiceStateFile},
};

const DEFAULT_RETENTION_MINUTES: u64 = 720;
const DEFAULT_SAMPLE_INTERVAL_SECS: u64 = 1;
const DEFAULT_MAX_MEMORY_BYTES: usize = 10 * 1024 * 1024;

/// Raw samples kept at full resolution before aggregation begins.
const RAW_CAPACITY: usize = 120;
/// Bucket width of the middle tier.
const MINUTE_SPAN_SECS: i64 = 60;
/// Minute buckets kept before they fold into the coarse tier.
const MINUTE_CAPACITY: usize = 60;
/// Bucket width of the coarse tier.
const COARSE_SPAN_SECS: i64 = 900;
/// Hard ceiling on coarse buckets per unit; retention is honoured by widening
/// the bucket, never by keeping more of them.
const COARSE_CAPACITY_MAX: usize = 96;
/// Buckets a retention window is divided into when it exceeds what the default
/// span covers.
const COARSE_CAPACITY_MIN_SPAN_DIVISOR: i64 = 48;

/// Sample collected for a managed unit at a specific timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    /// Timestamp when this sample was collected.
    pub timestamp: DateTime<Utc>,
    /// CPU usage percentage (0-100+ for multi-core).
    pub cpu_percent: f32,
    /// Resident set size in bytes.
    pub rss_bytes: u64,
    /// Total bytes read from disk.
    pub io_read_bytes: u64,
    /// Total bytes written to disk.
    pub io_write_bytes: u64,
    /// Total bytes received from network.
    pub net_rx_bytes: u64,
    /// Total bytes transmitted to network.
    pub net_tx_bytes: u64,
    /// Raw observations this entry represents. Older entries are aggregated,
    /// so a single entry can stand for a whole minute or quarter hour.
    #[serde(default = "one_sample")]
    pub sample_count: u32,
    /// Seconds of wall clock the entry covers. Zero for an instantaneous read.
    #[serde(default)]
    pub span_secs: u32,
    /// Lowest CPU reading inside the entry; absent on an instantaneous read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_min: Option<f32>,
    /// Highest CPU reading inside the entry. Aggregating on the mean alone
    /// would erase every spike older than the raw window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_max: Option<f32>,
    /// Lowest resident size inside the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_min: Option<u64>,
    /// Highest resident size inside the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_max: Option<u64>,
    /// Mean resident size across the entry. `rss_bytes` stays the newest
    /// reading, which is what a status line wants; an average over a window
    /// needs this instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_mean: Option<u64>,
}

impl MetricSample {
    /// Lowest CPU the entry saw, falling back to its own reading.
    pub fn cpu_low(&self) -> f32 {
        self.cpu_min.unwrap_or(self.cpu_percent)
    }

    /// Highest CPU the entry saw, falling back to its own reading.
    pub fn cpu_high(&self) -> f32 {
        self.cpu_max.unwrap_or(self.cpu_percent)
    }

    /// Lowest resident size the entry saw, falling back to its own reading.
    pub fn rss_low(&self) -> u64 {
        self.rss_min.unwrap_or(self.rss_bytes)
    }

    /// Highest resident size the entry saw, falling back to its own reading.
    pub fn rss_high(&self) -> u64 {
        self.rss_max.unwrap_or(self.rss_bytes)
    }

    /// Mean resident size across the entry, falling back to its own reading.
    pub fn rss_average(&self) -> u64 {
        self.rss_mean.unwrap_or(self.rss_bytes)
    }
}

/// Serde default for [`MetricSample::sample_count`] on records written before
/// aggregation existed.
fn one_sample() -> u32 {
    1
}

/// Summary statistics derived from recent samples.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSummary {
    /// Most recent CPU usage percentage.
    pub latest_cpu_percent: f32,
    /// Average CPU usage across all samples.
    pub average_cpu_percent: f32,
    /// Maximum CPU usage observed.
    pub max_cpu_percent: f32,
    /// Most recent resident set size in bytes.
    pub latest_rss_bytes: u64,
    /// Total number of samples used for statistics.
    pub samples: usize,
}

/// Configuration for runtime metrics collection.
#[derive(Debug, Clone)]
pub struct MetricsSettings {
    /// How long to retain metrics in memory.
    pub retention: Duration,
    /// Interval between metric samples.
    pub sample_interval: Duration,
    /// Maximum memory used for metrics storage.
    pub max_memory_bytes: usize,
    /// Optional spillover configuration for disk persistence.
    pub spillover: Option<SpilloverSettings>,
}

impl Default for MetricsSettings {
    /// Returns the default this item.
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
    /// Directory where spillover segments are written.
    pub directory: PathBuf,
    /// Maximum total bytes allowed for spillover storage.
    pub max_bytes: u64,
    /// Target size for individual spillover segment files.
    pub segment_bytes: u64,
}

/// Errors that can occur during metrics operations.
#[derive(Debug, Error)]
pub enum MetricsError {
    /// Failed to create spillover directory.
    #[error("failed to create spillover directory: {0}")]
    CreateDir(std::io::Error),
    /// Failed to write spillover segment to disk.
    #[error("failed to write spillover segment: {0}")]
    SpilloverWrite(std::io::Error),
    /// Failed to serialize spillover record.
    #[error("failed to serialise spillover record: {0}")]
    SpilloverSerialize(serde_json::Error),
}

/// One time bucket. A freshly recorded sample is a bucket of one observation
/// spanning nothing; folding older buckets together is what keeps a unit's
/// history bounded no matter how long it runs.
#[derive(Debug, Clone)]
struct Bucket {
    timestamp: DateTime<Utc>,
    /// When the newest reading inside this bucket was actually taken. The
    /// bucket's own timestamp is the window it was aligned to, which says
    /// nothing about which reading is the latest once entries merge.
    last_observed: DateTime<Utc>,
    cpu_mean: f32,
    cpu_min: f32,
    cpu_max: f32,
    rss_bytes: u64,
    rss_mean: f64,
    rss_min: u64,
    rss_max: u64,
    io_read_bytes: u64,
    io_write_bytes: u64,
    net_rx_bytes: u64,
    net_tx_bytes: u64,
    count: u32,
    span_secs: u32,
}

impl Bucket {
    /// Wraps a freshly collected sample.
    fn raw(sample: &MetricSample) -> Self {
        Self {
            timestamp: sample.timestamp,
            last_observed: sample.timestamp,
            cpu_mean: sample.cpu_percent,
            cpu_min: sample.cpu_low(),
            cpu_max: sample.cpu_high(),
            rss_bytes: sample.rss_bytes,
            rss_mean: sample.rss_mean.unwrap_or(sample.rss_bytes) as f64,
            rss_min: sample.rss_low(),
            rss_max: sample.rss_high(),
            io_read_bytes: sample.io_read_bytes,
            io_write_bytes: sample.io_write_bytes,
            net_rx_bytes: sample.net_rx_bytes,
            net_tx_bytes: sample.net_tx_bytes,
            count: sample.sample_count.max(1),
            span_secs: sample.span_secs,
        }
    }

    /// Renders the bucket back into the wire sample shape. Aggregates carry
    /// their extrema so a consumer can still see the peak inside the window.
    fn to_sample(&self) -> MetricSample {
        let aggregated = self.count > 1;
        MetricSample {
            timestamp: self.timestamp,
            cpu_percent: self.cpu_mean,
            rss_bytes: self.rss_bytes,
            io_read_bytes: self.io_read_bytes,
            io_write_bytes: self.io_write_bytes,
            net_rx_bytes: self.net_rx_bytes,
            net_tx_bytes: self.net_tx_bytes,
            sample_count: self.count,
            span_secs: self.span_secs,
            cpu_min: aggregated.then_some(self.cpu_min),
            cpu_max: aggregated.then_some(self.cpu_max),
            rss_min: aggregated.then_some(self.rss_min),
            rss_max: aggregated.then_some(self.rss_max),
            rss_mean: aggregated.then_some(self.rss_mean.round() as u64),
        }
    }

    /// Returns the wall clock the bucket's window ends at.
    fn end(&self) -> DateTime<Utc> {
        self.timestamp + ChronoDuration::seconds(self.span_secs as i64)
    }

    /// Absorbs a later bucket: means combine by observation count, extrema
    /// survive, and the monotonic counters take the newer reading.
    fn merge(&mut self, other: &Bucket) {
        let total = self.count.saturating_add(other.count).max(1) as f64;
        self.cpu_mean = ((self.cpu_mean as f64 * self.count as f64
            + other.cpu_mean as f64 * other.count as f64)
            / total) as f32;
        self.rss_mean = (self.rss_mean * self.count as f64
            + other.rss_mean * other.count as f64)
            / total;
        self.cpu_min = self.cpu_min.min(other.cpu_min);
        self.cpu_max = self.cpu_max.max(other.cpu_max);
        self.rss_min = self.rss_min.min(other.rss_min);
        self.rss_max = self.rss_max.max(other.rss_max);
        // "Latest" follows observation time, not arrival order: a reading that
        // turns up late, or after a clock step, must not overwrite a newer one.
        if other.last_observed >= self.last_observed {
            self.last_observed = other.last_observed;
            self.rss_bytes = other.rss_bytes;
            self.io_read_bytes = other.io_read_bytes;
            self.io_write_bytes = other.io_write_bytes;
            self.net_rx_bytes = other.net_rx_bytes;
            self.net_tx_bytes = other.net_tx_bytes;
        }
        self.count = self.count.saturating_add(other.count);
    }
}

/// Returns the start of the `span`-aligned bucket containing `timestamp`.
fn align_to_span(timestamp: DateTime<Utc>, span: i64) -> DateTime<Utc> {
    let secs = timestamp.timestamp();
    DateTime::from_timestamp(secs - secs.rem_euclid(span), 0).unwrap_or(timestamp)
}

/// Folds `bucket` into `tier` at its aligned window, keeping the tier ordered.
///
/// The common case appends or merges at the tail. A clock that steps backwards
/// would otherwise push an older entry after a newer one, which corrupts both
/// the ordering charts rely on and whatever "latest" means, so an out-of-order
/// arrival is placed where it belongs instead.
fn fold_into(tier: &mut VecDeque<Bucket>, mut bucket: Bucket, span: i64) {
    let start = align_to_span(bucket.timestamp, span);
    // Aligning moves the bucket's label, never its observation time.
    bucket.timestamp = start;
    bucket.span_secs = span as u32;

    match tier.back_mut() {
        Some(last) if last.timestamp == start => {
            last.merge(&bucket);
        }
        Some(last) if last.timestamp < start => tier.push_back(bucket),
        Some(_) => match tier.iter().rposition(|entry| entry.timestamp <= start) {
            Some(index) if tier[index].timestamp == start => {
                let existing = &mut tier[index];
                existing.merge(&bucket);
            }
            Some(index) => tier.insert(index + 1, bucket),
            None => tier.push_front(bucket),
        },
        None => tier.push_back(bucket),
    }
}

/// A unit's history at three resolutions: recent seconds, the last hour by the
/// minute, and older history in quarter hours.
#[derive(Debug, Clone, Default)]
struct UnitMetrics {
    raw: VecDeque<Bucket>,
    minute: VecDeque<Bucket>,
    coarse: VecDeque<Bucket>,
}

impl UnitMetrics {
    /// Returns every retained bucket oldest first.
    fn buckets(&self) -> impl Iterator<Item = &Bucket> {
        self.coarse
            .iter()
            .chain(self.minute.iter())
            .chain(self.raw.iter())
    }

    /// Returns the number of retained buckets across all tiers.
    fn len(&self) -> usize {
        self.coarse.len() + self.minute.len() + self.raw.len()
    }

    /// Drops the oldest retained bucket, coarsest tier first.
    fn pop_oldest(&mut self) -> Option<Bucket> {
        self.coarse
            .pop_front()
            .or_else(|| self.minute.pop_front())
            .or_else(|| self.raw.pop_front())
    }
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
    /// Handles new.
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
                .saturating_sub(buffer.len() * mem::size_of::<Bucket>());
        }
    }

    /// Records a new sample for the provided unit.
    ///
    /// History is bounded by resolution, not by count: recent samples stay raw,
    /// anything older folds into minute buckets and then quarter-hour buckets.
    /// A unit therefore costs the same whether it has run for a minute or a
    /// week, and retention no longer shrinks as more units are supervised.
    pub fn record_sample(
        &mut self,
        unit_hash: &str,
        sample: MetricSample,
    ) -> Result<(), MetricsError> {
        let raw_capacity = self.raw_capacity();
        let coarse_span = self.coarse_span();
        let retention_cutoff = sample.timestamp
            - ChronoDuration::from_std(self.settings.retention).unwrap_or_else(|_| {
                ChronoDuration::minutes(DEFAULT_RETENTION_MINUTES as i64)
            });
        let buffer = self.units.entry(unit_hash.to_string()).or_default();
        let before = buffer.len();

        let arriving = Bucket::raw(&sample);
        match buffer.raw.back() {
            Some(last) if last.timestamp > arriving.timestamp => {
                // A backwards clock step: place the reading in time order
                // rather than letting it masquerade as the newest one.
                let index = buffer
                    .raw
                    .iter()
                    .rposition(|entry| entry.timestamp <= arriving.timestamp);
                match index {
                    Some(index) => buffer.raw.insert(index + 1, arriving),
                    None => buffer.raw.push_front(arriving),
                }
            }
            _ => buffer.raw.push_back(arriving),
        }

        let mut evicted = Vec::new();
        while buffer.raw.len() > raw_capacity {
            if let Some(bucket) = buffer.raw.pop_front() {
                fold_into(&mut buffer.minute, bucket, MINUTE_SPAN_SECS);
            }
        }
        while buffer.minute.len() > MINUTE_CAPACITY {
            if let Some(bucket) = buffer.minute.pop_front() {
                fold_into(&mut buffer.coarse, bucket, coarse_span);
            }
        }
        // Retention is a wall-clock window, not a bucket count, and it applies
        // to every tier: a retention shorter than the raw window would
        // otherwise keep readings the operator asked to forget.
        for tier in [&mut buffer.coarse, &mut buffer.minute, &mut buffer.raw] {
            while tier
                .front()
                .is_some_and(|bucket| bucket.end() <= retention_cutoff)
            {
                if let Some(bucket) = tier.pop_front() {
                    evicted.push(bucket);
                }
            }
        }
        while buffer.coarse.len() > COARSE_CAPACITY_MAX {
            if let Some(bucket) = buffer.coarse.pop_front() {
                evicted.push(bucket);
            }
        }

        let after = buffer.len();
        self.total_estimated_bytes = self
            .total_estimated_bytes
            .saturating_add(after.saturating_sub(before) * mem::size_of::<Bucket>())
            .saturating_sub(before.saturating_sub(after) * mem::size_of::<Bucket>());

        if let Some(spillover) = self.spillover.as_mut() {
            for bucket in &evicted {
                spillover.persist(unit_hash, &bucket.to_sample())?;
            }
        }

        self.enforce_memory_budget()?;
        Ok(())
    }

    /// Returns how many raw samples to keep before aggregation starts. Slower
    /// sampling keeps the same wall-clock window at full resolution.
    fn raw_capacity(&self) -> usize {
        let interval = self.settings.sample_interval.as_secs().max(1);
        (RAW_CAPACITY / interval as usize).max(30)
    }

    /// Returns the coarse bucket width that covers the configured retention
    /// within a fixed number of buckets. A long retention widens the bucket
    /// rather than allocating more of them, so memory stays flat while the
    /// window the operator asked for is honoured.
    fn coarse_span(&self) -> i64 {
        let retention = self.settings.retention.as_secs() as i64;
        let needed = retention.div_euclid(COARSE_CAPACITY_MIN_SPAN_DIVISOR);
        needed.max(COARSE_SPAN_SECS)
    }

    /// Handles retention.
    pub fn retention(&self) -> Duration {
        self.settings.retention
    }

    /// Samples interval.
    pub fn sample_interval(&self) -> Duration {
        self.settings.sample_interval
    }

    /// Backstop for the tiered budget: with bounded tiers this should never
    /// bind, but a very large unit count can still exceed the ceiling.
    fn enforce_memory_budget(&mut self) -> Result<(), MetricsError> {
        if self.total_estimated_bytes <= self.settings.max_memory_bytes {
            return Ok(());
        }

        let mut unit_keys: Vec<String> = self.units.keys().cloned().collect();
        unit_keys.sort();
        while self.total_estimated_bytes > self.settings.max_memory_bytes {
            let mut removed_any = false;
            for key in unit_keys.iter() {
                if let Some(buffer) = self.units.get_mut(key)
                    && let Some(bucket) = buffer.pop_oldest()
                {
                    self.total_estimated_bytes = self
                        .total_estimated_bytes
                        .saturating_sub(mem::size_of::<Bucket>());
                    if let Some(spillover) = self.spillover.as_mut() {
                        spillover.persist(key, &bucket.to_sample())?;
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

    /// Returns the retained history for a unit, oldest first. Entries older
    /// than the raw window carry `sample_count` and `span_secs` describing the
    /// window they summarise.
    pub fn snapshot_unit(&self, unit_hash: &str) -> Option<Vec<MetricSample>> {
        self.units
            .get(unit_hash)
            .map(|buffer| buffer.buckets().map(Bucket::to_sample).collect())
    }

    /// Returns a copy of the most recent entries limited to `limit`.
    pub fn latest_samples(&self, unit_hash: &str, limit: usize) -> Vec<MetricSample> {
        self.units
            .get(unit_hash)
            .map(|buffer| {
                let total = buffer.len();
                buffer
                    .buckets()
                    .skip(total.saturating_sub(limit))
                    .map(Bucket::to_sample)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Produces summary statistics for the requested unit. The average weighs
    /// each entry by the observations it stands for, so aggregation does not
    /// let one quarter-hour bucket outvote a hundred raw samples.
    pub fn summarize_unit(&self, unit_hash: &str) -> Option<MetricsSummary> {
        let buffer = self.units.get(unit_hash)?;
        let latest = buffer.buckets().max_by_key(|bucket| bucket.last_observed)?;

        let mut observations = 0_u64;
        let mut weighted_cpu = 0.0_f64;
        let mut max_cpu = 0.0_f32;
        for bucket in buffer.buckets() {
            observations += bucket.count as u64;
            weighted_cpu += bucket.cpu_mean as f64 * bucket.count as f64;
            max_cpu = max_cpu.max(bucket.cpu_max);
        }
        if observations == 0 {
            return None;
        }

        Some(MetricsSummary {
            latest_cpu_percent: latest.cpu_mean,
            average_cpu_percent: (weighted_cpu / observations as f64) as f32,
            max_cpu_percent: max_cpu,
            latest_rss_bytes: latest.rss_bytes,
            samples: observations as usize,
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
/// Represents segment meta.
struct SegmentMeta {
    path: PathBuf,
    bytes: u64,
}

#[derive(Debug)]
/// Represents segment writer.
struct SegmentWriter {
    file: fs::File,
    path: PathBuf,
    bytes_written: u64,
}

impl MetricsSpillover {
    /// Handles new.
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

    /// Handles persist.
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

    /// Ensures writer.
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

    /// Handles rotate segment.
    fn rotate_segment(&mut self) -> Result<(), MetricsError> {
        if let Some(current) = self.current.take()
            && let Some(meta) = self.segments.back_mut()
        {
            meta.bytes = meta.bytes.saturating_add(current.bytes_written);
        }
        Ok(())
    }

    /// Handles enforce budget.
    fn enforce_budget(&mut self) -> Result<(), MetricsError> {
        while self.total_bytes > self.max_bytes {
            if let Some(meta) = self.segments.pop_front() {
                if self.current.as_ref().map(|w| w.path.clone())
                    == Some(meta.path.clone())
                {
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
/// Represents spillover record.
struct SpilloverRecord<'a> {
    unit_hash: &'a str,
    sample: &'a MetricSample,
}

/// Creates a new shared, thread-safe metrics store with the given settings.
pub fn shared_store(settings: MetricsSettings) -> Result<MetricsHandle, MetricsError> {
    Ok(Arc::new(RwLock::new(MetricsStore::new(settings)?)))
}

/// Unit metadata used by the collector to emit samples.
#[derive(Debug)]
pub struct UnitTarget {
    /// Unique hash identifying the unit.
    pub hash: String,
    /// Process ID if the unit has a running process.
    pub pid: Option<u32>,
}

/// Result of sampling a unit in the collector.
#[derive(Debug)]
pub struct CollectedSample {
    /// Hash of the unit that was sampled.
    pub hash: String,
    /// Collected metric sample data.
    pub sample: MetricSample,
}

/// Background worker that periodically collects metrics for running units.
pub struct MetricsCollector {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MetricsCollector {
    #[allow(clippy::too_many_arguments)]
    /// Starts the metrics collector and returns its shutdown handle.
    ///
    /// Thread creation errors are returned to the supervisor so it can fail the
    /// worker startup transaction instead of running without metric collection.
    pub fn spawn(
        store: MetricsHandle,
        config: Arc<Config>,
        pid_file: Arc<Mutex<PidFile>>,
        service_state: Arc<Mutex<ServiceStateFile>>,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let store_clone = Arc::clone(&store);

        let interval = {
            store
                .read()
                .map(|guard| guard.sample_interval())
                .unwrap_or_else(|_| Duration::from_secs(DEFAULT_SAMPLE_INTERVAL_SECS))
        };

        let handle = thread::Builder::new()
            .name("sysg-metrics".to_string())
            .spawn(move || {
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
                            if let Err(err) =
                                guard.record_sample(&entry.hash, entry.sample)
                            {
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
                        let step = if remaining > PROCESS_CHECK_INTERVAL {
                            PROCESS_CHECK_INTERVAL
                        } else {
                            remaining
                        };
                        thread::sleep(step);
                        slept += step;
                    }
                }
            })?;

        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }

    /// Stops this item.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for MetricsCollector {
    /// Handles drop.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Gathers unit targets.
fn gather_unit_targets(
    config: &Config,
    pid_file: &Arc<Mutex<PidFile>>,
    service_state: &Arc<Mutex<ServiceStateFile>>,
) -> Vec<UnitTarget> {
    let pid_guard = pid_file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state_guard = service_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut targets = Vec::new();
    let mut seen_hashes = Vec::new();

    for service_name in config.services.keys() {
        let hash = config.state_key(service_name);
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

/// Samples process.
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
            // sysinfo's `Process::memory()` returns bytes (since v0.30); do NOT
            // scale it. Multiplying by 1024 inflated RSS 1024x — a 66MB API read
            // as 63GB.
            rss_bytes: process.memory(),
            io_read_bytes: 0,
            io_write_bytes: 0,
            net_rx_bytes: 0,
            net_tx_bytes: 0,
            sample_count: 1,
            span_secs: 0,
            cpu_min: None,
            cpu_max: None,
            rss_min: None,
            rss_max: None,
            rss_mean: None,
        }
    } else {
        missing_process_sample()
    }
}

/// Builds the placeholder process sample.
fn missing_process_sample() -> MetricSample {
    MetricSample {
        timestamp: Utc::now(),
        cpu_percent: 0.0,
        rss_bytes: 0,
        io_read_bytes: 0,
        io_write_bytes: 0,
        net_rx_bytes: 0,
        net_tx_bytes: 0,
        sample_count: 1,
        span_secs: 0,
        cpu_min: None,
        cpu_max: None,
        rss_min: None,
        rss_max: None,
        rss_mean: None,
    }
}
