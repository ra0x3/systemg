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

/// Returns the legacy path to the log file for a given service and kind.
pub fn get_log_path(service: &str, kind: &str) -> PathBuf {
    resolve_log_path(service, kind)
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

/// Returns the current capture timestamp for persisted service output.
fn capture_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// Formats a captured stdout/stderr line.
fn format_captured_log_line(kind: &str, line: &[u8]) -> Vec<u8> {
    let line = String::from_utf8_lossy(line);
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
    let stdout_path = resolve_log_path(service_name, "stdout");
    let stderr_path = resolve_log_path(service_name, "stderr");
    let combined_path = resolve_combined_log_path(service_name);

    let stdout_exists = stdout_path.exists();
    let stderr_exists = stderr_path.exists();

    if !combined_path.exists() && !stdout_exists {
        touch_log_file(&stdout_path);
    }
    if !combined_path.exists() && !stderr_exists {
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

/// Follows a canonical service log while emitting only the requested stream.
fn follow_filtered_log_file(
    mut writer: impl Write,
    path: &Path,
    lines: usize,
    stream: LogStream,
) -> Result<(), LogsManagerError> {
    writer.write_all(&tail_log_file_filtered(path, lines, stream.as_str())?)?;
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
            if captured_log_line_matches_kind(line.trim_ascii_end(), stream.as_str()) {
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
) -> Result<(), LogsManagerError> {
    match kind.and_then(LogStream::from_filter) {
        Some(LogStream::Stdout) => {
            if combined_path.exists() {
                writer.write_all(&tail_log_file_filtered(
                    combined_path,
                    lines,
                    LogStream::Stdout.as_str(),
                )?)?;
            } else {
                writer.write_all(&tail_log_file(stdout_path, lines)?)?;
            }
        }
        Some(LogStream::Stderr) => {
            if combined_path.exists() {
                writer.write_all(&tail_log_file_filtered(
                    combined_path,
                    lines,
                    LogStream::Stderr.as_str(),
                )?)?;
            } else {
                writer.write_all(&tail_log_file(stderr_path, lines)?)?;
            }
        }
        _ => {
            if combined_path.exists() {
                writer.write_all(&tail_log_file(combined_path, lines)?)?;
            } else {
                writer.write_all(&tail_log_file(stdout_path, lines)?)?;
                writer.write_all(&tail_log_file(stderr_path, lines)?)?;
            }
        }
    }
    writer.flush()?;
    Ok(())
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

    /// Shows the logs for a specific service's stdout/stderr in real-time.
    pub fn show_log(
        &self,
        service_name: &str,
        pid: u32,
        lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            Some(pid),
            lines,
            kind,
            TailMode::current(),
        )
    }

    /// Shows a one-shot snapshot of logs for a specific service.
    pub fn show_log_snapshot(
        &self,
        service_name: &str,
        pid: u32,
        lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            Some(pid),
            lines,
            kind,
            TailMode::OneShot,
        )
    }

    /// Shows logs for a service that is not currently running.
    pub fn show_inactive_log(
        &self,
        service_name: &str,
        lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            None,
            lines,
            kind,
            TailMode::current(),
        )
    }

    /// Shows a one-shot snapshot of logs for a service that is not currently running.
    pub fn show_inactive_log_snapshot(
        &self,
        service_name: &str,
        lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            None,
            lines,
            kind,
            TailMode::OneShot,
        )
    }

    /// Streams service logs through an existing Unix socket connection.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn stream_log_to_socket(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        follow: bool,
        stream: &UnixStream,
    ) -> Result<(), LogsManagerError> {
        let mode = if follow {
            TailMode::Follow
        } else {
            TailMode::OneShot
        };
        if let Some(snapshot) = (kind.is_none()
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
        self.stream_logs_platform_with_mode(service_name, pid, lines, kind, mode, stream)
    }

    /// Clears stdout and stderr logs for a specific service.
    pub fn clear_service_logs(&self, service_name: &str) -> Result<(), LogsManagerError> {
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
        fs::create_dir_all(&log_dir)?;

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
            );
        }

        if combined_path.exists()
            && let Some(stream) = kind.and_then(LogStream::from_filter)
        {
            return follow_filtered_log_file(
                std::io::stdout().lock(),
                &combined_path,
                lines,
                stream,
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
    fn stream_logs_platform_with_mode(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        mode: TailMode,
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
            );
        }

        if combined_path.exists()
            && let Some(stream_filter) = kind.and_then(LogStream::from_filter)
        {
            return follow_filtered_log_file(
                stream.try_clone()?,
                &combined_path,
                lines,
                stream_filter,
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
            );
        }

        if combined_path.exists()
            && let Some(stream) = kind.and_then(LogStream::from_filter)
        {
            return follow_filtered_log_file(
                std::io::stdout().lock(),
                &combined_path,
                lines,
                stream,
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
    fn stream_logs_platform_with_mode(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        mode: TailMode,
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
            );
        }

        if combined_path.exists()
            && let Some(stream_filter) = kind.and_then(LogStream::from_filter)
        {
            return follow_filtered_log_file(
                stream.try_clone()?,
                &combined_path,
                lines,
                stream_filter,
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

        let pid_snapshot = {
            let guard = self.pid_file.lock().unwrap();
            guard.services().clone()
        };

        let cron_state =
            CronStateFile::load().unwrap_or_else(|_| CronStateFile::default());

        let hash_to_name: std::collections::HashMap<String, String> =
            crate::config::load_config(config_path)
                .ok()
                .map(|config| {
                    config
                        .services
                        .iter()
                        .map(|(name, svc_config)| {
                            (svc_config.compute_hash(), name.clone())
                        })
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
                    self.show_log_snapshot(&service_name, *pid, lines, kind)
                } else {
                    self.show_log(&service_name, *pid, lines, kind)
                };
                if let Err(err) = result {
                    eprintln!("Failed to stream logs for '{}': {}", service_name, err);
                }
                continue;
            }

            if let Ok(config) = crate::config::load_config(config_path)
                && let Some(service_config) = config.services.get(&service_name)
            {
                let service_hash = service_config.compute_hash();
                if let Some(_cron_job) = cron_state.jobs().get(&service_hash) {
                    debug!("Showing inactive logs for cron service '{}'", service_name);
                    let result = if matches!(mode, TailMode::OneShot) {
                        self.show_inactive_log_snapshot(&service_name, lines, kind)
                    } else {
                        self.show_inactive_log(&service_name, lines, kind)
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
}
