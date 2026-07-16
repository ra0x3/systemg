//! Module for managing and displaying logs of system services.
//!
//! Service output is captured into one canonical per-service log, with each line
//! tagged by capture timestamp and source stream.
use std::{
    collections::BTreeSet,
    env,
    fs::{self, File, OpenOptions},
    io::{BufWriter, IsTerminal, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, mpsc, mpsc::RecvTimeoutError},
    thread,
    time::Duration,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    os::unix::{
        io::{AsRawFd, FromRawFd, IntoRawFd},
        net::UnixStream,
    },
    process::{Command, Stdio},
};

use terminal_size::Width;
use tracing::debug;

use crate::{
    config::EffectiveLogsConfig, cron::CronStateFile, daemon::PidFile,
    error::LogsManagerError, runtime,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// High-level bucket for all-services log rendering.
pub enum LogSection {
    /// Services that are currently running.
    Running,
    /// Services that are currently offline.
    Offline,
}

impl LogSection {
    /// Returns the stable display label for this section.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running Services",
            Self::Offline => "Offline Services",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Captured service output stream.
enum LogStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
    /// Canonical merged stdout/stderr stream.
    Combined,
}

impl LogStream {
    /// Returns the stable persisted label.
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Combined => "combined",
        }
    }

    /// Parses a stream filter.
    fn from_filter(kind: &str) -> Option<Self> {
        match kind {
            "stdout" => Some(Self::Stdout),
            "stderr" => Some(Self::Stderr),
            _ => None,
        }
    }
}

impl From<LogStream> for &'static str {
    fn from(stream: LogStream) -> Self {
        stream.as_str()
    }
}

/// Returns the path to the canonical service log file.
pub fn get_service_log_path(service: &str) -> PathBuf {
    resolve_combined_log_path(service)
}

/// Returns the path to the supervisor's own log file.
pub fn supervisor_log_path() -> PathBuf {
    runtime::log_dir().join("supervisor.log")
}

/// Returns the last `n` content lines of a service's log, ANSI-stripped and
/// with the `<timestamp> <stream>` prefix removed, for use as diagnostic
/// evidence. Returns an empty vec when the log is missing or unreadable.
pub fn tail_service_log(service: &str, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};

    let path = get_service_log_path(service);
    let Ok(mut file) = fs::File::open(&path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let window = 16 * 1024;
    let start = len.saturating_sub(window);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::with_capacity(window as usize);
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&strip_ansi(&buf)).into_owned();

    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(strip_log_line_prefix)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(n)
        .rev()
        .collect()
}

/// Drops the leading `<rfc3339-timestamp> <stream>` tokens a service log line
/// carries, leaving the process's own output.
fn strip_log_line_prefix(line: &str) -> String {
    let mut parts = line.splitn(3, ' ');
    let (Some(first), Some(second), Some(rest)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return line.to_string();
    };
    let looks_like_timestamp = first.len() >= 20
        && first.ends_with('Z')
        && first.contains('T')
        && first.starts_with(|c: char| c.is_ascii_digit());
    if looks_like_timestamp && matches!(second, "stdout" | "stderr") {
        rest.to_string()
    } else {
        line.to_string()
    }
}

/// Summary of the files removed by a prune run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PruneSummary {
    /// Number of log files removed.
    pub removed_files: usize,
    /// Total bytes reclaimed by the prune.
    pub reclaimed_bytes: u64,
}

/// Parses a human-friendly byte size such as `500MB`, `2g`, or `1048576`.
pub fn parse_byte_size(value: &str) -> Result<u64, LogsManagerError> {
    let trimmed = value.trim();
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| LogsManagerError::InvalidPruneArg(value.to_string()))?;

    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(LogsManagerError::InvalidPruneArg(value.to_string())),
    };

    Ok((number * multiplier) as u64)
}

/// Parses a human-friendly duration such as `7d`, `12h`, or `30m` into seconds.
pub fn parse_age_seconds(value: &str) -> Result<u64, LogsManagerError> {
    let trimmed = value.trim();
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| LogsManagerError::InvalidPruneArg(value.to_string()))?;

    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "w" => 7 * 24 * 60 * 60,
        _ => return Err(LogsManagerError::InvalidPruneArg(value.to_string())),
    };

    Ok(number * multiplier)
}

/// Returns whether a file is a rotated backup (e.g. `supervisor.log.2`).
fn is_rotated_backup(file_name: &str) -> bool {
    file_name.rsplit_once('.').is_some_and(|(stem, suffix)| {
        stem.ends_with(".log") && suffix.parse::<usize>().is_ok()
    })
}

/// Prunes rotated log files by age and total size, keeping active `.log` files intact.
pub fn prune_logs(
    max_size: Option<&str>,
    max_age: Option<&str>,
) -> Result<PruneSummary, LogsManagerError> {
    let max_bytes = max_size.map(parse_byte_size).transpose()?;
    let max_age_secs = max_age.map(parse_age_seconds).transpose()?;

    let log_dir = runtime::log_dir();
    if !log_dir.exists() {
        return Ok(PruneSummary::default());
    }

    let mut backups: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    for entry in fs::read_dir(&log_dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_rotated_backup(file_name) {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        backups.push((path, metadata.len(), modified));
    }

    backups.sort_by_key(|(_, _, modified)| *modified);

    let mut summary = PruneSummary::default();

    if let Some(max_age_secs) = max_age_secs {
        let now = std::time::SystemTime::now();
        backups.retain(|(path, len, modified)| {
            let age = now
                .duration_since(*modified)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if age > max_age_secs {
                if fs::remove_file(path).is_ok() {
                    summary.removed_files += 1;
                    summary.reclaimed_bytes += len;
                }
                false
            } else {
                true
            }
        });
    }

    if let Some(max_bytes) = max_bytes {
        let mut total: u64 = backups.iter().map(|(_, len, _)| *len).sum();
        for (path, len, _) in &backups {
            if total <= max_bytes {
                break;
            }
            if fs::remove_file(path).is_ok() {
                summary.removed_files += 1;
                summary.reclaimed_bytes += len;
                total = total.saturating_sub(*len);
            }
        }
    }

    Ok(summary)
}

/// Parses a `--since` / `--until` bound into an absolute UTC instant.
///
/// Accepts an RFC3339 timestamp (`2026-07-07T14:00:00Z`), a bare UTC date
/// (`2026-07-07`, taken as midnight), or a relative duration in the past
/// (`30m`, `2h`, `7d`) resolved against `now`.
pub fn parse_time_bound(
    value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<chrono::DateTime<chrono::Utc>, LogsManagerError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(LogsManagerError::InvalidTimeBound(value.to_string()));
    }

    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(parsed.with_timezone(&chrono::Utc));
    }

    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        && let Some(midnight) = date.and_hms_opt(0, 0, 0)
    {
        return Ok(chrono::DateTime::from_naive_utc_and_offset(
            midnight,
            chrono::Utc,
        ));
    }

    let seconds = parse_age_seconds(trimmed)
        .map_err(|_| LogsManagerError::InvalidTimeBound(value.to_string()))?;
    Ok(now - chrono::Duration::seconds(seconds as i64))
}

/// Post-capture filter applied to persisted log lines before display.
#[derive(Clone, Default)]
pub struct LogFilter {
    /// Lower time bound (inclusive) on the systemg capture timestamp.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Upper time bound (inclusive) on the systemg capture timestamp.
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    /// Compiled substring/regex pattern a line must match to be kept.
    pub grep: Option<regex::Regex>,
    /// Read the full active-plus-rotated history instead of just the tail.
    pub all: bool,
}

impl LogFilter {
    /// Builds a filter from raw CLI/IPC parts, resolving time bounds against
    /// `now` and compiling the grep pattern.
    pub fn from_parts(
        since: Option<&str>,
        until: Option<&str>,
        grep: Option<&str>,
        all: bool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Self, LogsManagerError> {
        let since = since
            .map(|value| parse_time_bound(value, now))
            .transpose()?;
        let until = until
            .map(|value| parse_time_bound(value, now))
            .transpose()?;
        let grep = grep
            .map(|pattern| {
                regex::Regex::new(pattern)
                    .map_err(|err| LogsManagerError::InvalidGrep(err.to_string()))
            })
            .transpose()?;
        Ok(Self {
            since,
            until,
            grep,
            all,
        })
    }

    /// Returns whether any content filter (time bound or pattern) is active.
    pub fn has_content_filter(&self) -> bool {
        self.since.is_some() || self.until.is_some() || self.grep.is_some()
    }

    /// Returns whether the filter would keep any line at all.
    pub fn is_noop(&self) -> bool {
        !self.has_content_filter() && !self.all
    }

    /// Returns whether a single captured log line passes the filter.
    fn matches(&self, line: &[u8]) -> bool {
        if let Some(ts) = captured_line_timestamp(line) {
            if let Some(since) = self.since
                && ts < since
            {
                return false;
            }
            if let Some(until) = self.until
                && ts > until
            {
                return false;
            }
        } else if self.since.is_some() || self.until.is_some() {
            return false;
        }

        if let Some(pattern) = &self.grep {
            let text = String::from_utf8_lossy(line);
            if !pattern.is_match(&text) {
                return false;
            }
        }

        true
    }

    /// Retains only the newline-delimited lines that pass the content filter.
    pub fn apply(&self, bytes: &[u8]) -> Vec<u8> {
        if !self.has_content_filter() {
            return bytes.to_vec();
        }
        bytes
            .split_inclusive(|byte| *byte == b'\n')
            .filter(|line| self.matches(line.trim_ascii_end()))
            .flat_map(|line| line.iter().copied())
            .collect()
    }
}

/// Parses the leading systemg capture timestamp from a persisted log line.
fn captured_line_timestamp(line: &[u8]) -> Option<chrono::DateTime<chrono::Utc>> {
    let text = std::str::from_utf8(line).ok()?;
    let first = text.split(' ').next()?;
    chrono::DateTime::parse_from_rfc3339(first)
        .ok()
        .map(|parsed| parsed.with_timezone(&chrono::Utc))
}

/// Returns a service's active log path followed by its rotated backups,
/// ordered oldest to newest, for full-history reads.
pub fn rotated_history_paths(active: &Path) -> Vec<PathBuf> {
    let Some(parent) = active.parent() else {
        return vec![active.to_path_buf()];
    };
    let Some(base_name) = active.file_name().and_then(|name| name.to_str()) else {
        return vec![active.to_path_buf()];
    };
    let prefix = format!("{base_name}.");

    let mut backups: Vec<(usize, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(suffix) = file_name.strip_prefix(&prefix)
                && let Ok(index) = suffix.parse::<usize>()
            {
                backups.push((index, path));
            }
        }
    }

    backups.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
    let mut paths: Vec<PathBuf> = backups.into_iter().map(|(_, path)| path).collect();
    if active.exists() {
        paths.push(active.to_path_buf());
    }
    if paths.is_empty() {
        paths.push(active.to_path_buf());
    }
    paths
}

/// Reads a service's full active-plus-rotated history as one byte buffer.
fn read_full_history(active: &Path) -> Result<Vec<u8>, LogsManagerError> {
    let mut bytes = Vec::new();
    for path in rotated_history_paths(active) {
        match fs::read(&path) {
            Ok(mut chunk) => bytes.append(&mut chunk),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(bytes)
}

/// Output rendering mode for displayed log lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// Human-readable text with systemg's `ts stream` prefix.
    #[default]
    Text,
    /// The application's original line only (systemg prefix stripped).
    Raw,
    /// One JSON object per line: `{ts, stream, service, line}`.
    Json,
}

/// Prefix of the control line the supervisor emits before a service's bytes so
/// downstream readers can attribute lines to the right unit. Begins with an
/// ASCII record separator (`0x1e`) so it never collides with captured output.
pub const SERVICE_MARKER_PREFIX: &str = "\u{1e}sysg-service ";

/// Builds the per-service marker line the supervisor writes before streaming a
/// unit's captured bytes.
pub fn service_marker_line(service: &str) -> Vec<u8> {
    format!("{SERVICE_MARKER_PREFIX}{service}\n").into_bytes()
}

/// Returns the service name carried by a marker line, if `line` is one.
fn parse_service_marker(line: &str) -> Option<&str> {
    line.strip_prefix(SERVICE_MARKER_PREFIX)
}

/// Removes ANSI escape sequences (CSI and simple two-byte escapes) from bytes.
pub fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().copied().peekable();
    while let Some(byte) = iter.next() {
        if byte != 0x1b {
            out.push(byte);
            continue;
        }
        match iter.peek().copied() {
            Some(b'[') => {
                iter.next();
                for follow in iter.by_ref() {
                    if (0x40..=0x7e).contains(&follow) {
                        break;
                    }
                }
            }
            Some(b']') => {
                iter.next();
                while let Some(follow) = iter.next() {
                    if follow == 0x07 {
                        break;
                    }
                    if follow == 0x1b && iter.peek() == Some(&b'\\') {
                        iter.next();
                        break;
                    }
                }
            }
            Some(_) => {
                iter.next();
            }
            None => {}
        }
    }
    out
}

/// A captured log line split into its systemg metadata and payload.
struct CapturedLine<'a> {
    timestamp: &'a str,
    stream: &'a str,
    message: &'a str,
}

/// Parses a persisted `<rfc3339> <stream> <message>` captured log line.
///
/// Returns `None` for chrome such as banners and section headers, which do not
/// carry a leading capture timestamp.
fn parse_captured_line(line: &str) -> Option<CapturedLine<'_>> {
    let mut parts = line.splitn(3, ' ');
    let timestamp = parts.next()?;
    chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    let stream = parts.next()?;
    if !matches!(stream, "stdout" | "stderr" | "combined") {
        return None;
    }
    let message = parts.next().unwrap_or("");
    Some(CapturedLine {
        timestamp,
        stream,
        message,
    })
}

/// Escapes a string as a JSON string value (without surrounding quotes).
fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                escaped.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => escaped.push(c),
        }
    }
    escaped
}

/// A `Write` adapter that reformats systemg log bytes into the selected output
/// mode line by line, optionally stripping ANSI escapes and dropping chrome.
pub struct LogWriter<W: Write> {
    inner: W,
    format: LogFormat,
    strip_ansi: bool,
    service: Option<String>,
    pending: Vec<u8>,
}

impl<W: Write> LogWriter<W> {
    /// Creates a new adapter around `inner`.
    pub fn new(
        inner: W,
        format: LogFormat,
        strip_ansi: bool,
        service: Option<String>,
    ) -> Self {
        Self {
            inner,
            format,
            strip_ansi,
            service,
            pending: Vec::new(),
        }
    }

    /// Renders and writes a single complete line (newline already stripped).
    fn render_line(&mut self, raw: &[u8]) -> std::io::Result<()> {
        let bytes = if self.strip_ansi {
            strip_ansi(raw)
        } else {
            raw.to_vec()
        };

        if let Ok(text) = std::str::from_utf8(&bytes)
            && let Some(service) = parse_service_marker(text)
        {
            self.service = Some(service.to_string());
            return Ok(());
        }

        if matches!(self.format, LogFormat::Text) {
            self.inner.write_all(&bytes)?;
            self.inner.write_all(b"\n")?;
            return Ok(());
        }

        let text = String::from_utf8_lossy(&bytes);
        let parsed = parse_captured_line(&text);

        match self.format {
            LogFormat::Text => unreachable!(),
            LogFormat::Raw => {
                if let Some(parsed) = parsed {
                    self.inner.write_all(parsed.message.as_bytes())?;
                    self.inner.write_all(b"\n")?;
                }
            }
            LogFormat::Json => {
                if let Some(parsed) = parsed {
                    let service = self.service.as_deref().unwrap_or("");
                    let json = format!(
                        "{{\"ts\":\"{}\",\"stream\":\"{}\",\"service\":\"{}\",\"line\":\"{}\"}}\n",
                        json_escape(parsed.timestamp),
                        json_escape(parsed.stream),
                        json_escape(service),
                        json_escape(parsed.message),
                    );
                    self.inner.write_all(json.as_bytes())?;
                }
            }
        }
        Ok(())
    }
}

impl<W: Write> Write for LogWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if matches!(self.format, LogFormat::Text) && !self.strip_ansi {
            return self.inner.write(buf);
        }
        self.pending.extend_from_slice(buf);
        while let Some(pos) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            let line = &line[..line.len() - 1];
            self.render_line(line)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.render_line(&line)?;
        }
        self.inner.flush()
    }
}

/// Returns the legacy path to the log file for a given service and kind.
pub fn get_log_path(service: &str, kind: &str) -> PathBuf {
    resolve_log_path(service, kind)
}

/// Rejects a service name that could escape the log directory.
///
/// Log file paths are built by interpolating the service name into a filename
/// under `runtime::log_dir()`. The name arrives from the control socket, so a
/// value containing a path separator, NUL, or a `.`/`..` component could
/// traverse out of the log directory and cause the supervisor to read or create
/// files elsewhere ([CWE-22]/[CWE-73]). Only names that resolve to a single
/// in-directory filename are accepted.
///
/// [CWE-22]: https://cwe.mitre.org/data/definitions/22.html
/// [CWE-73]: https://cwe.mitre.org/data/definitions/73.html
pub fn validate_service_name(service: &str) -> Result<(), LogsManagerError> {
    let invalid = service.is_empty()
        || service.len() > 255
        || service == "."
        || service == ".."
        || service
            .chars()
            .any(|c| c == '/' || c == '\\' || c == '\0' || std::path::is_separator(c));
    if invalid {
        return Err(LogsManagerError::InvalidServiceName(service.to_string()));
    }
    Ok(())
}

/// Confirms a resolved log path stays within the log directory before it is
/// opened or created, guarding against traversal even if an unvalidated name
/// reaches a path builder.
fn assert_within_log_dir(path: &Path) -> Result<(), LogsManagerError> {
    let log_dir = runtime::log_dir();
    let parent = path.parent().unwrap_or(&log_dir);
    let base = log_dir.canonicalize().unwrap_or(log_dir.clone());
    let resolved = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    if resolved == base {
        Ok(())
    } else {
        Err(LogsManagerError::InvalidServiceName(
            path.display().to_string(),
        ))
    }
}

/// Returns the canonical path for a service log without performing any existence checks.
fn canonical_log_path(service: &str, kind: &str) -> PathBuf {
    let mut path = runtime::log_dir();
    path.push(format!("{service}_{kind}.log"));
    path
}

/// Returns the canonical stdout/stderr log path for a service.
fn canonical_combined_log_path(service: &str) -> PathBuf {
    let mut path = runtime::log_dir();
    path.push(format!("{service}.log"));
    path
}

const LIVE_LOG_BUFFER_LIMIT: usize = 256 * 1024;

/// Holds recent log bytes and active subscribers for a single service stream.
struct LiveLogEntry {
    buffer: Vec<u8>,
    subscribers: Vec<mpsc::Sender<Vec<u8>>>,
}

impl LiveLogEntry {
    /// Creates an empty live log entry.
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            subscribers: Vec::new(),
        }
    }

    /// Appends bytes and trims the in-memory buffer to the configured cap.
    fn append(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > LIVE_LOG_BUFFER_LIMIT {
            let overflow = self.buffer.len() - LIVE_LOG_BUFFER_LIMIT;
            self.buffer.drain(..overflow);
        }
        self.subscribers
            .retain(|subscriber| subscriber.send(chunk.to_vec()).is_ok());
    }
}

type LiveLogKey = (String, String);

/// Returns the global live log registry shared by supervisor-side log readers.
fn live_log_registry()
-> &'static Mutex<std::collections::HashMap<LiveLogKey, LiveLogEntry>> {
    static REGISTRY: OnceLock<
        Mutex<std::collections::HashMap<LiveLogKey, LiveLogEntry>>,
    > = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Appends new live log bytes for a service stream and notifies subscribers.
fn append_live_log_chunk(service: &str, stream: LogStream, chunk: &[u8]) {
    let key = (service.to_string(), stream.as_str().to_string());
    if let Ok(mut registry) = live_log_registry().lock() {
        let entry = registry.entry(key).or_insert_with(LiveLogEntry::new);
        entry.append(chunk);
    }
}

/// Drops the in-memory live-log buffer for every stream of a service.
///
/// The supervisor serves `sysg logs` from this registry, not from disk, so
/// truncating the files alone leaves the reader showing "purged" content. A
/// purge that runs inside the supervisor must clear this too.
pub fn clear_live_log(service: &str) {
    if let Ok(mut registry) = live_log_registry().lock() {
        registry.retain(|(name, _), _| name != service);
    }
}

/// Returns the buffered live log bytes for a service stream, if any.
fn snapshot_live_log(service: &str, stream: LogStream) -> Option<Vec<u8>> {
    let key = (service.to_string(), stream.as_str().to_string());
    live_log_registry()
        .lock()
        .ok()
        .and_then(|registry| registry.get(&key).map(|entry| entry.buffer.clone()))
}

/// Registers a subscriber for future live log chunks on a service stream.
fn subscribe_live_log(service: &str, stream: LogStream) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    let key = (service.to_string(), stream.as_str().to_string());
    if let Ok(mut registry) = live_log_registry().lock() {
        let entry = registry.entry(key).or_insert_with(LiveLogEntry::new);
        entry.subscribers.push(tx);
    }
    rx
}

/// Returns whether the client side of a Unix socket has disconnected.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn socket_peer_disconnected(stream: &UnixStream) -> bool {
    let fd = stream.as_raw_fd();
    let mut byte = 0_u8;
    let result = unsafe {
        libc::recv(
            fd,
            &mut byte as *mut u8 as *mut libc::c_void,
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };

    if result == 0 {
        return true;
    }

    if result < 0 {
        let err = std::io::Error::last_os_error();
        return !matches!(
            err.raw_os_error(),
            Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK
        );
    }

    false
}

/// Normalizes this item.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Locates existing log.
fn locate_existing_log(service: &str, kind: &str) -> Option<PathBuf> {
    let canonical = canonical_log_path(service, kind);
    let directory = canonical.parent()?;
    let needle = normalize(service);
    let suffix = format!("_{kind}.log");

    let entries = fs::read_dir(directory).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = path.file_name()?.to_str()?;
        if !file_name.ends_with(&suffix) {
            continue;
        }

        if let Some(service_name) = file_name.strip_suffix(&suffix)
            && normalize(service_name) == needle
        {
            return Some(path);
        }
    }

    None
}

/// Locates an existing merged service log.
fn locate_existing_combined_log(service: &str) -> Option<PathBuf> {
    let canonical = canonical_combined_log_path(service);
    let directory = canonical.parent()?;
    let needle = normalize(service);

    let entries = fs::read_dir(directory).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = path.file_name()?.to_str()?;
        if file_name == "supervisor.log"
            || file_name.ends_with("_stdout.log")
            || file_name.ends_with("_stderr.log")
            || !file_name.ends_with(".log")
        {
            continue;
        }

        if let Some(service_name) = file_name.strip_suffix(".log")
            && normalize(service_name) == needle
        {
            return Some(path);
        }
    }

    None
}

/// Attempts to resolve an on-disk log path for the given service and kind, falling back to the
/// canonical location when no existing file can be found.
pub fn resolve_log_path(service: &str, kind: &str) -> PathBuf {
    let canonical = canonical_log_path(service, kind);
    if canonical.exists() {
        return canonical;
    }

    locate_existing_log(service, kind).unwrap_or(canonical)
}

/// Attempts to resolve an on-disk merged log path for the given service.
fn resolve_combined_log_path(service: &str) -> PathBuf {
    let canonical = canonical_combined_log_path(service);
    if canonical.exists() {
        return canonical;
    }

    locate_existing_combined_log(service).unwrap_or(canonical)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Defines tail mode values.
enum TailMode {
    Follow,
    OneShot,
}

/// Forces one-shot mode when a content filter or full-history read is active,
/// since following cannot apply time bounds and full history is bounded.
fn resolve_tail_mode(mode: TailMode, filter: &LogFilter) -> TailMode {
    if filter.all || filter.since.is_some() || filter.until.is_some() {
        TailMode::OneShot
    } else {
        mode
    }
}

impl TailMode {
    /// Handles current.
    fn current() -> Self {
        match env::var("SYSTEMG_TAIL_MODE") {
            Ok(value) if value.eq_ignore_ascii_case("oneshot") => TailMode::OneShot,
            _ => TailMode::Follow,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    /// Handles configure command.
    fn configure_command(
        self,
        cmd: &mut Command,
        lines: usize,
        stdout_path: &Path,
        stderr_path: &Path,
        combined_path: &Path,
        kind: Option<&str>,
    ) {
        cmd.arg("-n").arg(lines.to_string());
        if matches!(self, TailMode::Follow) {
            cmd.arg("-F");
        }

        match kind {
            Some("stdout") => {
                if combined_path.exists() {
                    cmd.arg(combined_path);
                } else {
                    cmd.arg(stdout_path);
                }
            }
            Some("stderr") => {
                if combined_path.exists() {
                    cmd.arg(combined_path);
                } else {
                    cmd.arg(stderr_path);
                }
            }
            _ => {
                if combined_path.exists() {
                    cmd.arg(combined_path);
                } else {
                    cmd.arg(stdout_path).arg(stderr_path);
                }
            }
        }
    }
}

/// Touches log file.
fn touch_log_file(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = OpenOptions::new().create(true).append(true).open(path);
}

/// Truncates log file.
fn truncate_log_file(path: &Path) -> Result<(), LogsManagerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;

    Ok(())
}

/// Removes numbered rotated files that belong to an active log file.
fn remove_rotated_log_files(path: &Path) -> Result<(), LogsManagerError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let Some(base_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let prefix = format!("{base_name}.");

    if !parent.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(parent)? {
        let entry_path = entry?.path();
        if !entry_path.is_file() {
            continue;
        }
        let Some(file_name) = entry_path.file_name().and_then(|name| name.to_str())
        else {
            continue;
        };
        if file_name
            .strip_prefix(&prefix)
            .is_some_and(|suffix| suffix.parse::<usize>().is_ok())
        {
            fs::remove_file(entry_path)?;
        }
    }

    Ok(())
}

/// Returns the numbered rotation path for an active log file.
fn rotated_log_path(path: &Path, index: usize) -> PathBuf {
    let mut rotated = path.as_os_str().to_os_string();
    rotated.push(format!(".{index}"));
    PathBuf::from(rotated)
}

/// Rotates an active log file and keeps at most `max_files` numbered backups.
fn rotate_log_file(path: &Path, max_files: usize) -> std::io::Result<()> {
    if max_files == 0 {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        return Ok(());
    }

    let oldest = rotated_log_path(path, max_files);
    match fs::remove_file(&oldest) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }

    for index in (1..max_files).rev() {
        let from = rotated_log_path(path, index);
        let to = rotated_log_path(path, index + 1);
        if from.exists() {
            fs::rename(from, to)?;
        }
    }

    if path.exists() {
        fs::rename(path, rotated_log_path(path, 1))?;
    }

    Ok(())
}

/// Append-only log file that applies systemg rotation limits.
struct ActiveLogFile {
    path: PathBuf,
    file: BufWriter<File>,
    active_len: u64,
    settings: EffectiveLogsConfig,
}

impl ActiveLogFile {
    /// Opens an active log file.
    fn open(path: PathBuf, settings: EffectiveLogsConfig) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw_file = OpenOptions::new().create(true).append(true).open(&path)?;
        let active_len = raw_file.metadata().map(|meta| meta.len()).unwrap_or(0);
        Ok(Self {
            path,
            file: BufWriter::new(raw_file),
            active_len,
            settings,
        })
    }

    /// Writes one already-formatted log line.
    fn write_line(&mut self, line: &[u8]) -> std::io::Result<()> {
        if self.settings.max_bytes > 0
            && self.active_len > 0
            && self.active_len.saturating_add(line.len() as u64) > self.settings.max_bytes
        {
            self.file.flush()?;
            rotate_log_file(&self.path, self.settings.max_files)?;
            let raw_file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            self.file = BufWriter::new(raw_file);
            self.active_len = 0;
        }

        self.file.write_all(line)?;
        self.active_len = self.active_len.saturating_add(line.len() as u64);
        Ok(())
    }

    /// Flushes the active file.
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Shared, rotation-aware writer for the supervisor's own tracing output.
#[derive(Clone)]
pub struct RotatingLogWriter {
    inner: Arc<Mutex<ActiveLogFile>>,
}

impl RotatingLogWriter {
    /// Opens a rotating writer for the supervisor log at `path`.
    pub fn open(path: PathBuf, settings: EffectiveLogsConfig) -> std::io::Result<Self> {
        Ok(Self {
            inner: Arc::new(Mutex::new(ActiveLogFile::open(path, settings)?)),
        })
    }
}

impl Write for RotatingLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let payload = truncate_log_payload(buf);
        let mut file = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("supervisor log writer poisoned"))?;
        file.write_line(&payload)?;
        file.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut file = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("supervisor log writer poisoned"))?;
        file.flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatingLogWriter {
    type Writer = RotatingLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Maximum size, in bytes, of a single persisted log event before it is truncated.
const MAX_LOG_LINE_BYTES: usize = 16 * 1024;

/// Returns the current capture timestamp for persisted service output.
fn capture_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// Truncates an oversized log payload, appending a marker noting the dropped byte count.
fn truncate_log_payload(line: &[u8]) -> Vec<u8> {
    if line.len() <= MAX_LOG_LINE_BYTES {
        return line.to_vec();
    }

    let dropped = line.len() - MAX_LOG_LINE_BYTES;
    let mut boundary = MAX_LOG_LINE_BYTES;
    while boundary > 0 && (line[boundary] & 0b1100_0000) == 0b1000_0000 {
        boundary -= 1;
    }

    let mut truncated = line[..boundary].to_vec();
    truncated.extend_from_slice(format!("…[truncated {dropped} bytes]").as_bytes());
    truncated
}

/// Formats a captured stdout/stderr line.
fn format_captured_log_line(kind: &str, line: &[u8]) -> Vec<u8> {
    let line = truncate_log_payload(line);
    let line = String::from_utf8_lossy(&line);
    format!("{} {} {}\n", capture_timestamp(), kind, line).into_bytes()
}

#[cfg(target_os = "linux")]
/// Handles process fds present.
fn process_fds_present(pid: u32) -> bool {
    let stdout_fd_path = format!("/proc/{pid}/fd/1");
    let stderr_fd_path = format!("/proc/{pid}/fd/2");
    let stdout_fd = Path::new(&stdout_fd_path);
    let stderr_fd = Path::new(&stderr_fd_path);
    stdout_fd.exists() || stderr_fd.exists()
}

/// Resolves tail targets.
fn resolve_tail_targets(
    service_name: &str,
    pid: Option<u32>,
) -> Result<(PathBuf, PathBuf, PathBuf), LogsManagerError> {
    validate_service_name(service_name)?;
    let stdout_path = resolve_log_path(service_name, "stdout");
    let stderr_path = resolve_log_path(service_name, "stderr");
    let combined_path = resolve_combined_log_path(service_name);

    let stdout_exists = stdout_path.exists();
    let stderr_exists = stderr_path.exists();

    if !combined_path.exists() && !stdout_exists {
        assert_within_log_dir(&stdout_path)?;
        touch_log_file(&stdout_path);
    }
    if !combined_path.exists() && !stderr_exists {
        assert_within_log_dir(&stderr_path)?;
        touch_log_file(&stderr_path);
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(pid_value) = pid
            && !(combined_path.exists()
                || stdout_exists
                || stderr_exists
                || process_fds_present(pid_value))
        {
            return Err(LogsManagerError::LogUnavailable(pid_value));
        }
    }

    #[cfg(not(target_os = "linux"))]
    let _ = pid;

    Ok((stdout_path, stderr_path, combined_path))
}

/// Writes the standard service log header into the provided writer.
fn write_log_header(
    mut writer: impl Write,
    service_name: &str,
    pid: Option<u32>,
) -> Result<(), LogsManagerError> {
    write_boxed_log_title(
        &mut writer,
        &LogManager::format_log_title(service_name, pid),
    )
}

/// Writes a section header used by the all-services log view.
pub fn write_log_section_header(
    mut writer: impl Write,
    section: LogSection,
) -> Result<(), LogsManagerError> {
    write_boxed_log_title(&mut writer, section.label())?;
    writer.flush()?;
    Ok(())
}

/// Returns the current terminal width or a stable fallback when no TTY size is
/// available.
fn detect_log_terminal_width(default_width: usize) -> usize {
    terminal_size::terminal_size()
        .map(|(Width(width), _)| width as usize)
        .unwrap_or(default_width)
        .max(24)
}

/// Truncates a title to fit inside a full-width bordered log banner.
fn truncate_log_title(title: &str, max_width: usize) -> String {
    let title_width = title.chars().count();
    if title_width <= max_width {
        return title.to_string();
    }

    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let visible_width = max_width.saturating_sub(3);
    let mut truncated = title.chars().take(visible_width).collect::<String>();
    truncated.push_str("...");
    truncated
}

/// Writes a centered boxed title spanning the current terminal width.
fn write_boxed_log_title(
    mut writer: impl Write,
    title: &str,
) -> Result<(), LogsManagerError> {
    let terminal_width = detect_log_terminal_width(100);
    write!(writer, "{}", format_boxed_log_title(title, terminal_width))?;
    writer.flush()?;
    Ok(())
}

/// Formats a centered boxed title spanning the provided width.
fn format_boxed_log_title(title: &str, terminal_width: usize) -> String {
    let inner_width = terminal_width.saturating_sub(2).max(1);
    let title = truncate_log_title(title, inner_width);
    let title_width = title.chars().count();
    let left_padding = inner_width.saturating_sub(title_width) / 2;
    let right_padding = inner_width.saturating_sub(title_width + left_padding);

    format!(
        "\n┌{}┐\n│{}{}{}│\n└{}┘\n\n",
        "─".repeat(inner_width),
        " ".repeat(left_padding),
        title,
        " ".repeat(right_padding),
        "─".repeat(inner_width)
    )
}

const LOG_TAIL_CHUNK_SIZE: u64 = 8192;

/// Returns the last `lines` newline-delimited slices from a raw byte buffer.
fn tail_log_bytes(bytes: &[u8], lines: usize) -> Vec<u8> {
    if lines == 0 || bytes.is_empty() {
        return Vec::new();
    }

    let mut index = bytes.len();
    if bytes.last() == Some(&b'\n') {
        index = index.saturating_sub(1);
    }

    let mut newlines_seen = 0usize;
    while index > 0 {
        index -= 1;
        if bytes[index] == b'\n' {
            newlines_seen += 1;
            if newlines_seen == lines {
                return bytes[index + 1..].to_vec();
            }
        }
    }

    bytes.to_vec()
}

/// Reads the last `lines` log lines without scanning the whole file when the
/// requested tail fits near the end.
fn tail_log_file(path: &Path, lines: usize) -> Result<Vec<u8>, LogsManagerError> {
    if lines == 0 {
        return Ok(Vec::new());
    }

    let mut file = File::open(path)?;
    let mut remaining = file.metadata()?.len();
    let mut bytes = Vec::new();

    while remaining > 0 {
        let chunk_len = remaining.min(LOG_TAIL_CHUNK_SIZE);
        remaining -= chunk_len;

        file.seek(SeekFrom::Start(remaining))?;
        let mut chunk = vec![0_u8; chunk_len as usize];
        file.read_exact(&mut chunk)?;

        chunk.extend_from_slice(&bytes);
        bytes = chunk;

        if tail_log_bytes(&bytes, lines).len() < bytes.len() {
            break;
        }
    }

    Ok(tail_log_bytes(&bytes, lines))
}

/// Returns whether a captured canonical service log line belongs to `kind`.
fn captured_log_line_matches_kind(line: &[u8], kind: &str) -> bool {
    let Some(stream) = LogStream::from_filter(kind) else {
        return false;
    };
    let Some(first_space) = line.iter().position(|byte| *byte == b' ') else {
        return false;
    };
    let rest = &line[first_space + 1..];
    rest.strip_prefix(stream.as_str().as_bytes())
        .is_some_and(|remaining| remaining.first() == Some(&b' '))
}

/// Returns the last `lines` canonical log lines matching a stream kind.
fn tail_log_file_filtered(
    path: &Path,
    lines: usize,
    kind: &str,
) -> Result<Vec<u8>, LogsManagerError> {
    if lines == 0 {
        return Ok(Vec::new());
    }

    let mut file = File::open(path)?;
    let mut remaining = file.metadata()?.len();
    let mut bytes = Vec::new();

    while remaining > 0 {
        let chunk_len = remaining.min(LOG_TAIL_CHUNK_SIZE);
        remaining -= chunk_len;

        file.seek(SeekFrom::Start(remaining))?;
        let mut chunk = vec![0_u8; chunk_len as usize];
        file.read_exact(&mut chunk)?;

        chunk.extend_from_slice(&bytes);
        bytes = chunk;

        let matching_count = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| captured_log_line_matches_kind(line, kind))
            .count();
        if matching_count > lines {
            break;
        }
    }

    let mut matching = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| captured_log_line_matches_kind(line.trim_ascii_end(), kind))
        .map(Vec::from)
        .collect::<Vec<_>>();

    if matching.len() > lines {
        matching.drain(..matching.len() - lines);
    }

    Ok(matching.concat())
}

/// Filters canonical captured log bytes by stream kind.
fn filter_captured_log_bytes(bytes: &[u8], kind: &str) -> Vec<u8> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| captured_log_line_matches_kind(line.trim_ascii_end(), kind))
        .flat_map(|line| line.iter().copied())
        .collect()
}

/// Returns whether a captured line passes an optional stream kind filter.
fn line_matches_stream(line: &[u8], stream: Option<LogStream>) -> bool {
    match stream {
        Some(stream) => captured_log_line_matches_kind(line, stream.as_str()),
        None => true,
    }
}

/// Follows a canonical service log while emitting only lines that pass the
/// optional stream kind filter and the content filter (e.g. `--grep`).
fn follow_filtered_log_file(
    mut writer: impl Write,
    path: &Path,
    lines: usize,
    stream: Option<LogStream>,
    filter: &LogFilter,
) -> Result<(), LogsManagerError> {
    let initial = match stream {
        Some(stream) => tail_log_file_filtered(path, lines, stream.as_str())?,
        None => tail_log_file(path, lines)?,
    };
    writer.write_all(&filter.apply(&initial))?;
    writer.flush()?;

    let mut offset = fs::metadata(path)?.len();
    let mut pending = Vec::new();

    loop {
        thread::sleep(Duration::from_millis(250));

        let current_len = match fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                offset = 0;
                pending.clear();
                continue;
            }
            Err(err) => return Err(err.into()),
        };

        if current_len < offset {
            offset = 0;
            pending.clear();
        }

        if current_len == offset {
            continue;
        }

        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = Vec::with_capacity((current_len - offset) as usize);
        file.read_to_end(&mut chunk)?;
        offset = current_len;
        pending.extend_from_slice(&chunk);

        while let Some(newline_pos) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline_pos).collect::<Vec<_>>();
            let trimmed = line.trim_ascii_end();
            if line_matches_stream(trimmed, stream) && filter.matches(trimmed) {
                writer.write_all(&line)?;
                writer.flush()?;
            }
        }
    }
}

/// Writes the selected one-shot log tails to a writer.
fn write_log_file_tail(
    mut writer: impl Write,
    stdout_path: &Path,
    stderr_path: &Path,
    combined_path: &Path,
    lines: usize,
    kind: Option<&str>,
    filter: &LogFilter,
) -> Result<(), LogsManagerError> {
    for bytes in
        collect_log_tail(stdout_path, stderr_path, combined_path, lines, kind, filter)?
    {
        writer.write_all(&bytes)?;
    }
    writer.flush()?;
    Ok(())
}

/// Collects the selected log bytes for a one-shot view, honoring stream kind,
/// full-history reads, and post-capture time/pattern filtering.
fn collect_log_tail(
    stdout_path: &Path,
    stderr_path: &Path,
    combined_path: &Path,
    lines: usize,
    kind: Option<&str>,
    filter: &LogFilter,
) -> Result<Vec<Vec<u8>>, LogsManagerError> {
    let stream_kind = kind.and_then(LogStream::from_filter);

    if filter.all {
        let mut chunks = Vec::new();
        if combined_path.exists() {
            let raw = read_full_history(combined_path)?;
            let selected = match stream_kind {
                Some(stream) => filter_captured_log_bytes(&raw, stream.as_str()),
                None => raw,
            };
            chunks.push(filter.apply(&selected));
        } else {
            match stream_kind {
                Some(LogStream::Stdout) => {
                    chunks.push(filter.apply(&read_full_history(stdout_path)?))
                }
                Some(LogStream::Stderr) => {
                    chunks.push(filter.apply(&read_full_history(stderr_path)?))
                }
                _ => {
                    chunks.push(filter.apply(&read_full_history(stdout_path)?));
                    chunks.push(filter.apply(&read_full_history(stderr_path)?));
                }
            }
        }
        return Ok(chunks);
    }

    let read_lines = if filter.has_content_filter() {
        usize::MAX
    } else {
        lines
    };

    let mut chunks: Vec<Vec<u8>> = Vec::new();
    match stream_kind {
        Some(stream) => {
            if combined_path.exists() {
                chunks.push(tail_log_file_filtered(
                    combined_path,
                    read_lines,
                    stream.as_str(),
                )?);
            } else {
                let single = match stream {
                    LogStream::Stdout => stdout_path,
                    _ => stderr_path,
                };
                chunks.push(tail_log_file(single, read_lines)?);
            }
        }
        None => {
            if combined_path.exists() {
                chunks.push(tail_log_file(combined_path, read_lines)?);
            } else {
                chunks.push(tail_log_file(stdout_path, read_lines)?);
                chunks.push(tail_log_file(stderr_path, read_lines)?);
            }
        }
    }

    if filter.has_content_filter() {
        for chunk in &mut chunks {
            let filtered = filter.apply(chunk);
            *chunk = tail_log_bytes(&filtered, lines);
        }
    }

    Ok(chunks)
}

/// Writes forwarded console line.
fn write_forwarded_console_line(
    mut writer: impl Write,
    prefix: &str,
    line: &str,
) -> std::io::Result<()> {
    writeln!(writer, "{prefix}{line}")
}

/// Forwards a completed byte line to stderr or the debug logger.
fn forward_prefixed_line(service_label: &str, line: &[u8], echo_to_terminal: bool) {
    let line = String::from_utf8_lossy(line);
    if echo_to_terminal {
        if let Err(err) = write_forwarded_console_line(
            std::io::stderr(),
            &format!("[{service_label}] "),
            &line,
        ) {
            eprintln!(
                "Warning: Failed to write forwarded log for [{}]: {}",
                service_label, err
            );
        }
    } else {
        debug!("[{service_label}] {line}");
    }
}

/// Flushes all complete lines from a buffered byte stream to the configured console/debug sink.
fn flush_forwarded_lines(
    pending: &mut Vec<u8>,
    service_label: &str,
    echo_to_terminal: bool,
) {
    while let Some(newline_pos) = pending.iter().position(|byte| *byte == b'\n') {
        let mut line = pending.drain(..=newline_pos).collect::<Vec<_>>();
        if matches!(line.last(), Some(b'\n')) {
            line.pop();
        }
        if matches!(line.last(), Some(b'\r')) {
            line.pop();
        }
        forward_prefixed_line(service_label, &line, echo_to_terminal);
    }
}

/// Flushes any trailing unterminated line to the configured console/debug sink.
fn flush_remaining_forwarded_line(
    pending: &mut Vec<u8>,
    service_label: &str,
    echo_to_terminal: bool,
) {
    if pending.is_empty() {
        return;
    }

    let line = std::mem::take(pending);
    forward_prefixed_line(service_label, &line, echo_to_terminal);
}

/// A completed service output line received by the canonical log writer.
struct ServiceLogLine {
    stream: LogStream,
    line: Vec<u8>,
}

/// Reads one service output stream and sends completed lines to the canonical writer.
fn read_service_log_stream(
    service_label: &str,
    stream: LogStream,
    mut reader: impl Read,
    sender: mpsc::Sender<ServiceLogLine>,
) -> std::io::Result<()> {
    let mut buffer = [0_u8; 8192];
    let mut pending = Vec::new();
    let mut forward_pending = Vec::new();
    let echo_to_terminal = std::io::stderr().is_terminal();

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        let chunk = &buffer[..bytes_read];
        pending.extend_from_slice(chunk);
        forward_pending.extend_from_slice(chunk);
        while let Some(newline_pos) = pending.iter().position(|byte| *byte == b'\n') {
            let mut line = pending.drain(..=newline_pos).collect::<Vec<_>>();
            if matches!(line.last(), Some(b'\n')) {
                line.pop();
            }
            if matches!(line.last(), Some(b'\r')) {
                line.pop();
            }

            let _ = sender.send(ServiceLogLine { stream, line });
        }
        flush_forwarded_lines(&mut forward_pending, service_label, echo_to_terminal);
    }

    if !pending.is_empty() {
        let _ = sender.send(ServiceLogLine {
            stream,
            line: pending.clone(),
        });
    }

    flush_remaining_forwarded_line(&mut forward_pending, service_label, echo_to_terminal);
    Ok(())
}

/// Writes all service output streams into one canonical append-only service log.
fn write_service_log(
    service_label: &str,
    path: PathBuf,
    receiver: mpsc::Receiver<ServiceLogLine>,
    settings: EffectiveLogsConfig,
) -> std::io::Result<()> {
    let mut file = ActiveLogFile::open(path, settings)?;

    for message in receiver {
        let formatted = format_captured_log_line(message.stream.as_str(), &message.line);
        file.write_line(&formatted)?;
        append_live_log_chunk(service_label, LogStream::Combined, &formatted);
    }

    file.flush()
}

/// Copies a spawned-child output stream into its log file while optionally echoing completed lines.
fn stream_dynamic_child_log(
    path: &Path,
    owner_label: Option<&str>,
    child_label: &str,
    mut reader: impl Read,
    echo_to_console: bool,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut buffer = [0_u8; 8192];
    let mut pending = Vec::new();

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        let chunk = &buffer[..bytes_read];
        file.write_all(chunk)?;

        if echo_to_console {
            pending.extend_from_slice(chunk);
            while let Some(newline_pos) = pending.iter().position(|byte| *byte == b'\n') {
                let mut line = pending.drain(..=newline_pos).collect::<Vec<_>>();
                if matches!(line.last(), Some(b'\n')) {
                    line.pop();
                }
                if matches!(line.last(), Some(b'\r')) {
                    line.pop();
                }
                let owner = owner_label.unwrap_or("spawn");
                println!(
                    "[{}:{}] {}",
                    owner,
                    child_label,
                    String::from_utf8_lossy(&line)
                );
            }
        }
    }

    if echo_to_console && !pending.is_empty() {
        let owner = owner_label.unwrap_or("spawn");
        println!(
            "[{}:{}] {}",
            owner,
            child_label,
            String::from_utf8_lossy(&pending)
        );
    }

    file.flush()
}

/// Creates the log directory if it doesn't exist and spawns a thread to write logs to file.
pub fn spawn_log_writer(service: &str, reader: impl Read + Send + 'static, kind: &str) {
    spawn_log_writer_with_config(service, reader, kind, EffectiveLogsConfig::default());
}

/// Spawns a legacy single-stream writer through the canonical service log.
pub fn spawn_log_writer_with_config(
    service: &str,
    reader: impl Read + Send + 'static,
    kind: &str,
    settings: EffectiveLogsConfig,
) {
    let reader = Box::new(reader) as Box<dyn Read + Send>;
    match LogStream::from_filter(kind) {
        Some(LogStream::Stdout) => {
            spawn_service_log_writers(service, Some(reader), None, settings)
        }
        Some(LogStream::Stderr) => {
            spawn_service_log_writers(service, None, Some(reader), settings)
        }
        _ => spawn_service_log_writers(service, Some(reader), None, settings),
    }
}

/// Spawns one canonical writer for a service's stdout and stderr streams.
pub fn spawn_service_log_writers(
    service: &str,
    stdout: Option<Box<dyn Read + Send>>,
    stderr: Option<Box<dyn Read + Send>>,
    settings: EffectiveLogsConfig,
) {
    let path = get_service_log_path(service);
    let service_label = service.to_string();
    let (sender, receiver) = mpsc::channel();

    {
        let service_label = service_label.clone();
        let path = path.clone();
        thread::spawn(move || {
            if let Err(err) =
                write_service_log(&service_label, path.clone(), receiver, settings)
            {
                eprintln!(
                    "Warning: Unable to write service log file at {:?}: {}",
                    path, err
                );
            }
        });
    }

    if let Some(stdout) = stdout {
        let service_label = service_label.clone();
        let sender = sender.clone();
        thread::spawn(move || {
            if let Err(err) =
                read_service_log_stream(&service_label, LogStream::Stdout, stdout, sender)
            {
                eprintln!(
                    "Warning: Unable to read stdout for [{}]: {}",
                    service_label, err
                );
            }
        });
    }

    if let Some(stderr) = stderr {
        let service_label = service_label.clone();
        thread::spawn(move || {
            if let Err(err) =
                read_service_log_stream(&service_label, LogStream::Stderr, stderr, sender)
            {
                eprintln!(
                    "Warning: Unable to read stderr for [{}]: {}",
                    service_label, err
                );
            }
        });
    }
}

/// Spawns a thread to capture and log output from dynamically spawned child processes.
///
/// # Arguments
///
/// * `root_service` - Optional parent service name for log organization
/// * `child_name` - Name of the child process being logged
/// * `pid` - Process ID of the child
/// * `reader` - Reader for the child's output stream
/// * `kind` - Type of stream (e.g., "stdout" or "stderr")
/// * `echo_to_console` - Whether to echo output to console in addition to file
pub fn spawn_dynamic_child_log_writer(
    root_service: Option<&str>,
    child_name: &str,
    pid: u32,
    reader: impl Read + Send + 'static,
    kind: &str,
    echo_to_console: bool,
) {
    let owner_component = root_service
        .map(normalize)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dynamic".to_string());
    let child_component = normalize(child_name);
    let child_component = if child_component.is_empty() {
        "child".to_string()
    } else {
        child_component
    };

    let mut path = runtime::log_dir();
    path.push("spawn");
    let file_name = format!(
        "{}_{}_{}_{}.log",
        owner_component, child_component, pid, kind
    );
    path.push(file_name);

    let owner_label = root_service.map(str::to_string);
    let child_label = child_name.to_string();

    thread::spawn(move || {
        if let Err(err) = stream_dynamic_child_log(
            &path,
            owner_label.as_deref(),
            &child_label,
            reader,
            echo_to_console,
        ) {
            eprintln!("Warning: Unable to write spawn log {:?}: {}", path, err);
        }
    });
}

/// Initializes logging for a service by spawning threads to write stdout and stderr to log files.
pub struct LogManager {
    /// The PID file containing service names and their respective PIDs.
    pid_file: Arc<Mutex<PidFile>>,
}

impl LogManager {
    /// Creates a new `LogManager` instance.
    pub fn new(pid_file: Arc<Mutex<PidFile>>) -> Self {
        Self { pid_file }
    }

    /// Collects the filtered one-shot log bytes for a single service without
    /// printing, for callers that reformat the output themselves.
    pub fn collect_service_log(
        &self,
        service_name: &str,
        lines: usize,
        kind: Option<&str>,
        filter: &LogFilter,
    ) -> Result<Vec<u8>, LogsManagerError> {
        let stdout_path = resolve_log_path(service_name, "stdout");
        let stderr_path = resolve_log_path(service_name, "stderr");
        let combined_path = resolve_combined_log_path(service_name);
        let mut bytes = Vec::new();
        for chunk in collect_log_tail(
            &stdout_path,
            &stderr_path,
            &combined_path,
            lines,
            kind,
            filter,
        )? {
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// Shows the logs for a specific service's stdout/stderr in real-time.
    pub fn show_log(
        &self,
        service_name: &str,
        pid: u32,
        lines: usize,
        kind: Option<&str>,
        filter: &LogFilter,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            Some(pid),
            lines,
            kind,
            resolve_tail_mode(TailMode::current(), filter),
            filter,
        )
    }

    /// Shows a one-shot snapshot of logs for a specific service.
    pub fn show_log_snapshot(
        &self,
        service_name: &str,
        pid: u32,
        lines: usize,
        kind: Option<&str>,
        filter: &LogFilter,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            Some(pid),
            lines,
            kind,
            TailMode::OneShot,
            filter,
        )
    }

    /// Shows logs for a service that is not currently running.
    pub fn show_inactive_log(
        &self,
        service_name: &str,
        lines: usize,
        kind: Option<&str>,
        filter: &LogFilter,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            None,
            lines,
            kind,
            resolve_tail_mode(TailMode::current(), filter),
            filter,
        )
    }

    /// Shows a one-shot snapshot of logs for a service that is not currently running.
    pub fn show_inactive_log_snapshot(
        &self,
        service_name: &str,
        lines: usize,
        kind: Option<&str>,
        filter: &LogFilter,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            None,
            lines,
            kind,
            TailMode::OneShot,
            filter,
        )
    }

    /// Streams service logs through an existing Unix socket connection.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[allow(clippy::too_many_arguments)]
    pub fn stream_log_to_socket(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        follow: bool,
        filter: &LogFilter,
        stream: &UnixStream,
    ) -> Result<(), LogsManagerError> {
        let mode = resolve_tail_mode(
            if follow {
                TailMode::Follow
            } else {
                TailMode::OneShot
            },
            filter,
        );
        if !filter.all
            && let Some(snapshot) = (kind.is_none()
                || kind.and_then(LogStream::from_filter).is_some())
            .then(|| snapshot_live_log(service_name, LogStream::Combined))
            .flatten()
            && !snapshot.is_empty()
        {
            write_log_header(stream.try_clone()?, service_name, pid)?;
            let mut socket = stream.try_clone()?;
            let snapshot = match kind {
                Some(kind_name) => filter_captured_log_bytes(&snapshot, kind_name),
                None => snapshot,
            };
            let snapshot = filter.apply(&snapshot);
            let tail = tail_log_bytes(&snapshot, lines);
            if !tail.is_empty() {
                socket.write_all(&tail)?;
                socket.flush()?;
            }
            if matches!(mode, TailMode::Follow) {
                let receiver = subscribe_live_log(service_name, LogStream::Combined);
                loop {
                    match receiver.recv_timeout(Duration::from_millis(250)) {
                        Ok(chunk) => {
                            let chunk = match kind {
                                Some(kind_name) => {
                                    filter_captured_log_bytes(&chunk, kind_name)
                                }
                                None => chunk,
                            };
                            let chunk = filter.apply(&chunk);
                            if chunk.is_empty() {
                                continue;
                            }
                            match socket.write_all(&chunk) {
                                Ok(()) => {
                                    socket.flush()?;
                                }
                                Err(err)
                                    if matches!(
                                        err.kind(),
                                        std::io::ErrorKind::BrokenPipe
                                            | std::io::ErrorKind::ConnectionReset
                                    ) =>
                                {
                                    break;
                                }
                                Err(err) => return Err(err.into()),
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if socket_peer_disconnected(&socket) {
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            }
            return Ok(());
        }
        self.stream_logs_platform_with_mode(
            service_name,
            pid,
            lines,
            kind,
            mode,
            filter,
            stream,
        )
    }

    /// Clears stdout and stderr logs for a specific service.
    pub fn clear_service_logs(&self, service_name: &str) -> Result<(), LogsManagerError> {
        validate_service_name(service_name)?;
        let stdout_path = resolve_log_path(service_name, "stdout");
        let stderr_path = resolve_log_path(service_name, "stderr");
        let combined_path = resolve_combined_log_path(service_name);

        truncate_log_file(&stdout_path)?;
        truncate_log_file(&stderr_path)?;
        truncate_log_file(&combined_path)?;
        remove_rotated_log_files(&stdout_path)?;
        remove_rotated_log_files(&stderr_path)?;
        remove_rotated_log_files(&combined_path)?;

        Ok(())
    }

    /// Clears all known service and supervisor log files.
    pub fn clear_all_logs(&self) -> Result<(), LogsManagerError> {
        let log_dir = runtime::log_dir();
        runtime::create_private_dir(&log_dir)?;

        for entry in fs::read_dir(&log_dir)? {
            let path = entry?.path();
            if !path.is_file() {
                continue;
            }

            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            if file_name == "supervisor.log"
                || file_name.ends_with("_stdout.log")
                || file_name.ends_with("_stderr.log")
                || (file_name.ends_with(".log")
                    && !file_name.contains("_stdout.log")
                    && !file_name.contains("_stderr.log"))
            {
                truncate_log_file(&path)?;
                remove_rotated_log_files(&path)?;
            } else if file_name.strip_suffix(".log").is_none()
                && (file_name.contains("_stdout.log.")
                    || file_name.contains("_stderr.log.")
                    || file_name.starts_with("supervisor.log."))
            {
                fs::remove_file(&path)?;
            }
        }

        Ok(())
    }

    /// Platform-specific implementation for showing logs.
    #[cfg(target_os = "linux")]
    fn show_logs_platform_with_mode(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        mode: TailMode,
        filter: &LogFilter,
    ) -> Result<(), LogsManagerError> {
        println!(
            "\n+{:-^33}+\n\
     | {:^31} |\n\
     +{:-^33}+\n",
            "-",
            Self::format_log_title(service_name, pid),
            "-"
        );

        let (stdout_path, stderr_path, combined_path) =
            resolve_tail_targets(service_name, pid)?;

        debug!("Streaming logs via tail for '{}'", service_name);

        if matches!(mode, TailMode::OneShot) {
            return write_log_file_tail(
                std::io::stdout().lock(),
                &stdout_path,
                &stderr_path,
                &combined_path,
                lines,
                kind,
                filter,
            );
        }

        if combined_path.exists()
            && (filter.grep.is_some() || kind.and_then(LogStream::from_filter).is_some())
        {
            return follow_filtered_log_file(
                std::io::stdout().lock(),
                &combined_path,
                lines,
                kind.and_then(LogStream::from_filter),
                filter,
            );
        }

        let mut cmd = Command::new("tail");
        #[cfg(target_os = "linux")]
        {
            if let Some(pid_value) = pid
                && !process_fds_present(pid_value)
            {
                debug!(
                    "Falling back to log files for '{}' because /proc/{pid_value} fds are unavailable",
                    service_name
                );
            }
        }
        mode.configure_command(
            &mut cmd,
            lines,
            &stdout_path,
            &stderr_path,
            &combined_path,
            kind,
        );
        cmd.stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());

        let status = cmd.status()?;
        if !status.success() {
            return Err(LogsManagerError::TailCommandFailed(status.code()));
        }

        Ok(())
    }

    /// Linux implementation for streaming logs through a Unix socket.
    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    fn stream_logs_platform_with_mode(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        mode: TailMode,
        filter: &LogFilter,
        stream: &UnixStream,
    ) -> Result<(), LogsManagerError> {
        write_log_header(stream.try_clone()?, service_name, pid)?;

        let (stdout_path, stderr_path, combined_path) =
            resolve_tail_targets(service_name, pid)?;

        debug!("Streaming logs via supervisor tail for '{}'", service_name);

        if matches!(mode, TailMode::OneShot) {
            return write_log_file_tail(
                stream.try_clone()?,
                &stdout_path,
                &stderr_path,
                &combined_path,
                lines,
                kind,
                filter,
            );
        }

        if combined_path.exists()
            && (filter.grep.is_some() || kind.and_then(LogStream::from_filter).is_some())
        {
            return follow_filtered_log_file(
                stream.try_clone()?,
                &combined_path,
                lines,
                kind.and_then(LogStream::from_filter),
                filter,
            );
        }

        let mut cmd = Command::new("tail");
        if let Some(pid_value) = pid
            && !process_fds_present(pid_value)
        {
            debug!(
                "Falling back to log files for '{}' because /proc/{pid_value} fds are unavailable",
                service_name
            );
        }
        mode.configure_command(
            &mut cmd,
            lines,
            &stdout_path,
            &stderr_path,
            &combined_path,
            kind,
        );

        let stdout_stream = stream.try_clone()?;
        let stderr_stream = stream.try_clone()?;
        let stdout_fd = stdout_stream.into_raw_fd();
        let stderr_fd = stderr_stream.into_raw_fd();
        unsafe {
            cmd.stdout(Stdio::from_raw_fd(stdout_fd));
            cmd.stderr(Stdio::from_raw_fd(stderr_fd));
        }

        let status = cmd.status()?;
        if !status.success() {
            return Err(LogsManagerError::TailCommandFailed(status.code()));
        }

        Ok(())
    }

    /// macOS implementation for showing logs using log files.
    #[cfg(target_os = "macos")]
    fn show_logs_platform_with_mode(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        mode: TailMode,
        filter: &LogFilter,
    ) -> Result<(), LogsManagerError> {
        println!(
            "\n+{:-^33}+\n\
     | {:^31} |\n\
     +{:-^33}+\n",
            "-",
            Self::format_log_title(service_name, pid),
            "-"
        );

        let (stdout_path, stderr_path, combined_path) =
            resolve_tail_targets(service_name, pid)?;

        debug!("Streaming logs via tail for '{}'", service_name);

        if matches!(mode, TailMode::OneShot) {
            return write_log_file_tail(
                std::io::stdout().lock(),
                &stdout_path,
                &stderr_path,
                &combined_path,
                lines,
                kind,
                filter,
            );
        }

        if combined_path.exists()
            && (filter.grep.is_some() || kind.and_then(LogStream::from_filter).is_some())
        {
            return follow_filtered_log_file(
                std::io::stdout().lock(),
                &combined_path,
                lines,
                kind.and_then(LogStream::from_filter),
                filter,
            );
        }

        let mut cmd = Command::new("tail");
        mode.configure_command(
            &mut cmd,
            lines,
            &stdout_path,
            &stderr_path,
            &combined_path,
            kind,
        );
        cmd.stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());

        let status = cmd.status()?;
        if !status.success() {
            return Err(LogsManagerError::TailCommandFailed(status.code()));
        }

        Ok(())
    }

    /// macOS implementation for streaming logs through a Unix socket.
    #[cfg(target_os = "macos")]
    #[allow(clippy::too_many_arguments)]
    fn stream_logs_platform_with_mode(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        mode: TailMode,
        filter: &LogFilter,
        stream: &UnixStream,
    ) -> Result<(), LogsManagerError> {
        write_log_header(stream.try_clone()?, service_name, pid)?;

        let (stdout_path, stderr_path, combined_path) =
            resolve_tail_targets(service_name, pid)?;

        debug!("Streaming logs via supervisor tail for '{}'", service_name);

        if matches!(mode, TailMode::OneShot) {
            return write_log_file_tail(
                stream.try_clone()?,
                &stdout_path,
                &stderr_path,
                &combined_path,
                lines,
                kind,
                filter,
            );
        }

        if combined_path.exists()
            && (filter.grep.is_some() || kind.and_then(LogStream::from_filter).is_some())
        {
            return follow_filtered_log_file(
                stream.try_clone()?,
                &combined_path,
                lines,
                kind.and_then(LogStream::from_filter),
                filter,
            );
        }

        let mut cmd = Command::new("tail");
        mode.configure_command(
            &mut cmd,
            lines,
            &stdout_path,
            &stderr_path,
            &combined_path,
            kind,
        );

        let stdout_stream = stream.try_clone()?;
        let stderr_stream = stream.try_clone()?;
        let stdout_fd = stdout_stream.into_raw_fd();
        let stderr_fd = stderr_stream.into_raw_fd();
        unsafe {
            cmd.stdout(Stdio::from_raw_fd(stdout_fd));
            cmd.stderr(Stdio::from_raw_fd(stderr_fd));
        }

        let status = cmd.status()?;
        if !status.success() {
            return Err(LogsManagerError::TailCommandFailed(status.code()));
        }

        Ok(())
    }

    /// Streams logs for all active services in real-time.
    pub fn show_logs(
        &self,
        lines: usize,
        kind: Option<&str>,
        config_path: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_with_mode(lines, kind, config_path, TailMode::current())
    }

    /// Streams one-shot snapshots for all active services.
    pub fn show_logs_snapshot(
        &self,
        lines: usize,
        kind: Option<&str>,
        config_path: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_with_mode(lines, kind, config_path, TailMode::OneShot)
    }

    /// Shows logs with mode.
    fn show_logs_with_mode(
        &self,
        lines: usize,
        kind: Option<&str>,
        config_path: Option<&str>,
        mode: TailMode,
    ) -> Result<(), LogsManagerError> {
        debug!("Fetching logs for all services...");

        println!(
            "\n\
            ╭{}╮\n\
            │ Warning  Showing latest logs per service (stdout & stderr)             │\n\
            │                                                                   │\n\
            │ For complete logs, run: sysg logs <service>                      │\n\
            ╰{}╯\n",
            "─".repeat(67),
            "─".repeat(67)
        );

        if matches!(kind, Some("supervisor")) {
            let _ = self.show_supervisor_log(lines).map_err(|err| {
                eprintln!("Failed to show supervisor logs: {}", err);
            });

            return Ok(());
        }

        let (pid_snapshot, store) = {
            let guard = self.pid_file.lock().unwrap();
            (guard.services().clone(), guard.store())
        };

        let cron_state = CronStateFile::load(store).unwrap_or_default();

        let hash_to_name: std::collections::HashMap<String, String> =
            crate::config::load_config(config_path)
                .ok()
                .map(|config| {
                    config
                        .services
                        .keys()
                        .map(|name| (config.state_key(name), name.clone()))
                        .collect()
                })
                .unwrap_or_default();

        let mut service_names: BTreeSet<String> = pid_snapshot.keys().cloned().collect();

        for hash in cron_state.jobs().keys() {
            if let Some(name) = hash_to_name.get(hash) {
                service_names.insert(name.clone());
            } else {
                service_names.insert(hash.clone());
            }
        }

        debug!("Services: {service_names:?}");

        if service_names.is_empty() {
            if kind.is_some() {
                println!("No active services");
            }
            return Ok(());
        }

        for service_name in service_names {
            if let Some(pid) = pid_snapshot.get(&service_name) {
                debug!("Service: {service_name}, PID: {pid}");
                let result = if matches!(mode, TailMode::OneShot) {
                    self.show_log_snapshot(
                        &service_name,
                        *pid,
                        lines,
                        kind,
                        &LogFilter::default(),
                    )
                } else {
                    self.show_log(&service_name, *pid, lines, kind, &LogFilter::default())
                };
                if let Err(err) = result {
                    eprintln!("Failed to stream logs for '{}': {}", service_name, err);
                }
                continue;
            }

            if let Ok(config) = crate::config::load_config(config_path)
                && config.services.contains_key(&service_name)
            {
                let service_hash = config.state_key(&service_name);
                if let Some(_cron_job) = cron_state.jobs().get(&service_hash) {
                    debug!("Showing inactive logs for cron service '{}'", service_name);
                    let result = if matches!(mode, TailMode::OneShot) {
                        self.show_inactive_log_snapshot(
                            &service_name,
                            lines,
                            kind,
                            &LogFilter::default(),
                        )
                    } else {
                        self.show_inactive_log(
                            &service_name,
                            lines,
                            kind,
                            &LogFilter::default(),
                        )
                    };
                    if let Err(err) = result {
                        eprintln!(
                            "Failed to stream logs for '{}': {}",
                            service_name, err
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Formats log title.
    fn format_log_title(service_name: &str, pid: Option<u32>) -> String {
        match pid {
            Some(pid) => format!("{service_name} [pid {pid}]"),
            None => format!("{service_name} [offline]"),
        }
    }

    /// Shows the supervisor logs
    fn show_supervisor_log(&self, lines: usize) -> Result<(), LogsManagerError> {
        let supervisor_log = runtime::log_dir().join("supervisor.log");

        if !supervisor_log.exists() {
            return Ok(());
        }

        println!(
            "\n+{:-^33}+\n\
             | {:^31} |\n\
             +{:-^33}+\n",
            "-", "Supervisor", "-"
        );

        let tail = tail_log_file(&supervisor_log, lines)?;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&tail)?;
        stdout.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::Cursor,
        path::Path,
        thread,
        time::Duration,
    };

    use tempfile::tempdir_in;

    use super::*;

    #[test]
    fn validate_service_name_accepts_plain_names() {
        for name in ["api", "web-1", "worker_2", "svc.v1", "A.B_c-3"] {
            assert!(validate_service_name(name).is_ok(), "rejected {name}");
        }
    }

    #[test]
    fn validate_service_name_rejects_traversal() {
        for name in [
            "",
            ".",
            "..",
            "../etc/passwd",
            "../../../../etc/cron.d/x",
            "a/b",
            "a\\b",
            "nul\0byte",
        ] {
            assert!(
                validate_service_name(name).is_err(),
                "accepted traversal name {name:?}"
            );
        }
    }

    #[test]
    fn tail_log_bytes_returns_last_lines_with_trailing_newline() {
        let bytes = b"line 1\nline 2\nline 3\nline 4\n";

        assert_eq!(tail_log_bytes(bytes, 2), b"line 3\nline 4\n");
    }

    #[test]
    fn tail_log_bytes_returns_last_lines_without_trailing_newline() {
        let bytes = b"line 1\nline 2\nline 3\nline 4";

        assert_eq!(tail_log_bytes(bytes, 2), b"line 3\nline 4");
    }

    #[test]
    fn tail_log_bytes_returns_all_bytes_when_line_count_fits() {
        let bytes = b"line 1\nline 2\n";

        assert_eq!(tail_log_bytes(bytes, 2), bytes);
        assert_eq!(tail_log_bytes(bytes, 5), bytes);
    }

    #[test]
    fn tail_log_bytes_returns_empty_when_zero_lines_requested() {
        assert_eq!(tail_log_bytes(b"line 1\nline 2\n", 0), b"");
    }

    #[test]
    fn resolve_log_path_matches_hyphenated_files() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let log_dir = canonical_log_path("dummy", "stdout")
            .parent()
            .map(Path::to_path_buf)
            .unwrap();
        fs::create_dir_all(&log_dir).unwrap();

        let target = log_dir.join("arb-rs_stdout.log");
        File::create(&target).unwrap();

        let resolved = resolve_log_path("arb_rs", "stdout");
        assert_eq!(resolved, target);

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn spawn_dynamic_child_log_writer_persists_output() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let reader = Cursor::new(b"hello\nworld\n".to_vec());
        super::spawn_dynamic_child_log_writer(
            Some("alpha"),
            "beta",
            123,
            reader,
            "stdout",
            false,
        );

        thread::sleep(Duration::from_millis(100));

        let log_path = crate::runtime::log_dir()
            .join("spawn")
            .join("alpha_beta_123_stdout.log");
        let contents =
            fs::read_to_string(&log_path).expect("spawn log should be written");
        assert!(contents.contains("hello"));
        assert!(contents.contains("world"));

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn spawn_log_writer_persists_unterminated_output() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        super::spawn_log_writer("svc", Cursor::new(b"partial line".to_vec()), "stdout");

        thread::sleep(Duration::from_millis(100));

        let log_path = get_service_log_path("svc");
        let contents =
            fs::read_to_string(&log_path).expect("service log should be written");
        assert!(contents.contains(" stdout partial line\n"));

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn spawn_log_writer_persists_non_utf8_output() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        super::spawn_log_writer("svc", Cursor::new(vec![0xff, b'a', b'\n']), "stderr");

        thread::sleep(Duration::from_millis(100));

        let log_path = get_service_log_path("svc");
        let contents =
            fs::read_to_string(&log_path).expect("service log should be written");
        assert!(contents.contains(" stderr "));
        assert!(contents.contains("a\n"));

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn spawn_log_writer_rotates_when_active_file_exceeds_limit() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let settings = EffectiveLogsConfig {
            sink: crate::config::LogSink::File,
            max_bytes: 6,
            max_files: 1,
        };
        let log_path = get_service_log_path("svc");
        fs::create_dir_all(log_path.parent().expect("log parent")).unwrap();
        fs::write(&log_path, "first\n").unwrap();
        super::spawn_log_writer_with_config(
            "svc",
            Cursor::new(b"second\n".to_vec()),
            "stdout",
            settings,
        );

        thread::sleep(Duration::from_millis(100));

        let active = fs::read_to_string(&log_path).expect("active log exists");
        let rotated = fs::read_to_string(rotated_log_path(&log_path, 1))
            .expect("rotated log exists");
        assert_eq!(rotated, "first\n");
        assert!(active.contains(" stdout second\n"));

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn truncate_log_payload_leaves_small_lines_untouched() {
        let line = b"short line";
        assert_eq!(truncate_log_payload(line), line);
    }

    #[test]
    fn truncate_log_payload_caps_oversized_lines() {
        let line = vec![b'a'; MAX_LOG_LINE_BYTES + 4096];
        let truncated = truncate_log_payload(&line);
        assert!(truncated.len() < line.len());
        let text = String::from_utf8(truncated).expect("valid utf8");
        assert!(text.contains("[truncated 4096 bytes]"));
    }

    #[test]
    fn parse_byte_size_handles_units() {
        assert_eq!(parse_byte_size("1024").unwrap(), 1024);
        assert_eq!(parse_byte_size("1kb").unwrap(), 1024);
        assert_eq!(parse_byte_size("2MB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_byte_size("1g").unwrap(), 1024 * 1024 * 1024);
        assert!(parse_byte_size("nonsense").is_err());
    }

    #[test]
    fn parse_age_seconds_handles_units() {
        assert_eq!(parse_age_seconds("30").unwrap(), 30);
        assert_eq!(parse_age_seconds("5m").unwrap(), 300);
        assert_eq!(parse_age_seconds("2h").unwrap(), 7200);
        assert_eq!(parse_age_seconds("7d").unwrap(), 7 * 24 * 60 * 60);
        assert!(parse_age_seconds("12x").is_err());
    }

    #[test]
    fn is_rotated_backup_matches_numbered_files_only() {
        assert!(is_rotated_backup("supervisor.log.1"));
        assert!(is_rotated_backup("api.log.12"));
        assert!(!is_rotated_backup("api.log"));
        assert!(!is_rotated_backup("api.log.bak"));
    }

    #[test]
    fn prune_logs_trims_backups_by_size() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let log_dir = crate::runtime::log_dir();
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(log_dir.join("svc.log"), vec![b'a'; 100]).unwrap();
        fs::write(log_dir.join("svc.log.1"), vec![b'a'; 100]).unwrap();
        fs::write(log_dir.join("svc.log.2"), vec![b'a'; 100]).unwrap();

        let summary = super::prune_logs(Some("150"), None).unwrap();

        assert!(summary.removed_files >= 1);
        assert!(log_dir.join("svc.log").exists());

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn rotating_log_writer_rotates_supervisor_output() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let path = supervisor_log_path();
        fs::create_dir_all(path.parent().expect("log parent")).unwrap();
        let settings = EffectiveLogsConfig {
            sink: crate::config::LogSink::File,
            max_bytes: 8,
            max_files: 1,
        };
        let mut writer = RotatingLogWriter::open(path.clone(), settings).unwrap();
        writer.write_all(b"first\n").unwrap();
        writer.write_all(b"second\n").unwrap();
        writer.flush().unwrap();

        let active = fs::read_to_string(&path).expect("active log exists");
        let rotated =
            fs::read_to_string(rotated_log_path(&path, 1)).expect("rotated log exists");
        assert_eq!(rotated, "first\n");
        assert_eq!(active, "second\n");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn forwarded_console_line_preserves_ansi_bytes() {
        let mut output = Vec::new();
        let line = "\u{1b}[34mDEBUG\u{1b}[0m child log";

        write_forwarded_console_line(&mut output, "[svc] ", line)
            .expect("console line should write");

        assert_eq!(
            String::from_utf8(output).expect("valid utf8"),
            format!("[svc] {line}\n")
        );
    }

    fn utc(text: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(text)
            .expect("valid rfc3339")
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn parse_time_bound_accepts_rfc3339_and_date() {
        let now = utc("2026-07-07T12:00:00Z");
        assert_eq!(
            parse_time_bound("2026-07-07T09:30:00Z", now).unwrap(),
            utc("2026-07-07T09:30:00Z")
        );
        assert_eq!(
            parse_time_bound("2026-07-07", now).unwrap(),
            utc("2026-07-07T00:00:00Z")
        );
    }

    #[test]
    fn parse_time_bound_accepts_relative_age() {
        let now = utc("2026-07-07T12:00:00Z");
        assert_eq!(
            parse_time_bound("2h", now).unwrap(),
            utc("2026-07-07T10:00:00Z")
        );
    }

    #[test]
    fn parse_time_bound_rejects_garbage() {
        let now = utc("2026-07-07T12:00:00Z");
        assert!(parse_time_bound("not-a-time", now).is_err());
    }

    #[test]
    fn log_filter_applies_time_window() {
        let bytes = b"2026-07-07T09:00:00Z stdout early\n\
2026-07-07T10:30:00Z stdout middle\n\
2026-07-07T12:00:00Z stdout late\n";
        let filter = LogFilter {
            since: Some(utc("2026-07-07T10:00:00Z")),
            until: Some(utc("2026-07-07T11:00:00Z")),
            ..LogFilter::default()
        };
        let out = String::from_utf8(filter.apply(bytes)).unwrap();
        assert_eq!(out, "2026-07-07T10:30:00Z stdout middle\n");
    }

    #[test]
    fn log_filter_applies_grep() {
        let bytes = b"2026-07-07T09:00:00Z stdout hello world\n\
2026-07-07T09:00:01Z stderr ERROR boom\n\
2026-07-07T09:00:02Z stdout all good\n";
        let filter = LogFilter {
            grep: Some(regex::Regex::new("ERROR|good").unwrap()),
            ..LogFilter::default()
        };
        let out = String::from_utf8(filter.apply(bytes)).unwrap();
        assert!(out.contains("ERROR boom"));
        assert!(out.contains("all good"));
        assert!(!out.contains("hello world"));
    }

    #[test]
    fn collect_all_ignores_default_lines_cap() {
        let dir = std::env::temp_dir().join(format!(
            "sysg_all_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();
        let combined = dir.join("svc.log");
        let mut body = String::new();
        for index in 0..200 {
            body.push_str(&format!(
                "2026-07-08T09:00:{index:02}Z stdout line {index}\n"
            ));
        }
        fs::write(&combined, body).unwrap();
        let missing = dir.join("svc_stdout.log");

        let filter = LogFilter {
            all: true,
            ..LogFilter::default()
        };
        let chunks =
            collect_log_tail(&missing, &missing, &combined, 50, None, &filter).unwrap();
        let text = String::from_utf8(chunks.concat()).unwrap();
        assert_eq!(text.lines().count(), 200);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn collect_all_time_window_spans_rotated_history() {
        let dir = std::env::temp_dir().join(format!(
            "sysg_rot_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();
        let combined = dir.join("svc.log");
        fs::write(
            dir.join("svc.log.1"),
            "2026-07-04T09:00:00Z stdout old rotated\n\
2026-07-08T09:00:00Z stdout kept rotated\n",
        )
        .unwrap();
        fs::write(
            &combined,
            "2026-07-08T10:00:00Z stdout kept active\n\
2026-07-09T09:00:00Z stdout too new\n",
        )
        .unwrap();
        let missing = dir.join("svc_stdout.log");

        let filter = LogFilter {
            since: Some(utc("2026-07-08T00:00:00Z")),
            until: Some(utc("2026-07-09T00:00:00Z")),
            all: true,
            ..LogFilter::default()
        };
        let chunks =
            collect_log_tail(&missing, &missing, &combined, 50, None, &filter).unwrap();
        let text = String::from_utf8(chunks.concat()).unwrap();
        assert!(text.contains("kept rotated"), "{text}");
        assert!(text.contains("kept active"), "{text}");
        assert!(!text.contains("old rotated"), "{text}");
        assert!(!text.contains("too new"), "{text}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn log_filter_noop_returns_input() {
        let bytes = b"line without leading timestamp\n";
        let filter = LogFilter::default();
        assert_eq!(filter.apply(bytes), bytes);
        assert!(filter.is_noop());
    }

    #[test]
    fn strip_ansi_removes_color_codes() {
        let input = b"\x1b[1;31mERROR\x1b[0m boom \x1b[34mblue\x1b[0m";
        assert_eq!(strip_ansi(input), b"ERROR boom blue");
    }

    #[test]
    fn strip_ansi_leaves_plain_text() {
        let input = b"plain line 42";
        assert_eq!(strip_ansi(input), input);
    }

    #[test]
    fn parse_captured_line_extracts_fields() {
        let parsed =
            parse_captured_line("2026-07-07T09:00:00Z stdout hello world").unwrap();
        assert_eq!(parsed.timestamp, "2026-07-07T09:00:00Z");
        assert_eq!(parsed.stream, "stdout");
        assert_eq!(parsed.message, "hello world");
    }

    #[test]
    fn parse_captured_line_rejects_banner() {
        assert!(parse_captured_line("┌─────────┐").is_none());
        assert!(parse_captured_line("Project: arbitration").is_none());
    }

    #[test]
    fn log_writer_json_emits_one_object_per_line() {
        let mut out = Vec::new();
        {
            let mut writer =
                LogWriter::new(&mut out, LogFormat::Json, true, Some("api".into()));
            writer
                .write_all(b"2026-07-07T09:00:00Z stdout \x1b[31mhello\x1b[0m\n")
                .unwrap();
            writer.write_all(b"\xe2\x94\x8c banner line\n").unwrap();
            writer.flush().unwrap();
        }
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text,
            "{\"ts\":\"2026-07-07T09:00:00Z\",\"stream\":\"stdout\",\"service\":\"api\",\"line\":\"hello\"}\n"
        );
    }

    #[test]
    fn log_writer_json_service_follows_marker_lines() {
        let mut out = Vec::new();
        {
            let mut writer = LogWriter::new(&mut out, LogFormat::Json, true, None);
            writer
                .write_all(&service_marker_line("arb_rs__server"))
                .unwrap();
            writer
                .write_all(b"2026-07-08T09:00:00Z stdout openai_call\n")
                .unwrap();
            writer
                .write_all(&service_marker_line("arb_py__curator"))
                .unwrap();
            writer
                .write_all(b"2026-07-08T09:00:01Z stderr ingest done\n")
                .unwrap();
            writer.flush().unwrap();
        }
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text,
            "{\"ts\":\"2026-07-08T09:00:00Z\",\"stream\":\"stdout\",\"service\":\"arb_rs__server\",\"line\":\"openai_call\"}\n\
{\"ts\":\"2026-07-08T09:00:01Z\",\"stream\":\"stderr\",\"service\":\"arb_py__curator\",\"line\":\"ingest done\"}\n"
        );
    }

    #[test]
    fn log_writer_drops_marker_lines_in_text_mode() {
        let mut out = Vec::new();
        {
            let mut writer = LogWriter::new(&mut out, LogFormat::Text, true, None);
            writer.write_all(&service_marker_line("svc")).unwrap();
            writer
                .write_all(b"2026-07-08T09:00:00Z stdout hello\n")
                .unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "2026-07-08T09:00:00Z stdout hello\n"
        );
    }

    #[test]
    fn log_writer_raw_strips_prefix_and_banner() {
        let mut out = Vec::new();
        {
            let mut writer = LogWriter::new(&mut out, LogFormat::Raw, true, None);
            writer
                .write_all(b"2026-07-07T09:00:00Z stderr actual message\n")
                .unwrap();
            writer.write_all(b"Running Services\n").unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(String::from_utf8(out).unwrap(), "actual message\n");
    }

    #[test]
    fn log_writer_text_strip_ansi_keeps_prefix() {
        let mut out = Vec::new();
        {
            let mut writer = LogWriter::new(&mut out, LogFormat::Text, true, None);
            writer
                .write_all(b"2026-07-07T09:00:00Z stdout \x1b[32mok\x1b[0m\n")
                .unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "2026-07-07T09:00:00Z stdout ok\n"
        );
    }

    #[test]
    fn rotated_history_paths_orders_oldest_to_newest() {
        let dir = std::env::temp_dir().join(format!(
            "sysg_hist_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();
        let active = dir.join("svc.log");
        for name in ["svc.log", "svc.log.1", "svc.log.2", "svc.log.10"] {
            fs::write(dir.join(name), b"x").unwrap();
        }

        let paths = rotated_history_paths(&active);
        let names: Vec<_> = paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["svc.log.10", "svc.log.2", "svc.log.1", "svc.log"]);

        fs::remove_dir_all(&dir).ok();
    }
}
