//! Status management for services in the daemon.
use nix::unistd::{Pid, getpgid};
use std::{
    collections::BTreeSet,
    process::Command,
    sync::{Arc, Mutex},
};
use sysinfo::{ProcessesToUpdate, System};
use tracing::debug;

use crate::cron::{
    CronExecutionRecord, CronExecutionStatus, CronStateFile, PersistedCronJobState,
};
use crate::daemon::{PidFile, ServiceLifecycleStatus, ServiceStateFile};

#[cfg(not(target_os = "linux"))]
use nix::sys::signal;
#[cfg(target_os = "linux")]
use std::{fs, path::Path};

#[cfg(target_os = "linux")]
use std::time::UNIX_EPOCH;

use chrono::{DateTime, Local, Utc};
use chrono_tz::Tz;

const GREEN_BOLD: &str = "\x1b[1;32m"; // Bright Green
const RED_BOLD: &str = "\x1b[1;31m"; // Bright Red
const MAGENTA_BOLD: &str = "\x1b[1;35m"; // Magenta
const YELLOW_BOLD: &str = "\x1b[1;33m"; // Yellow/Gold
const RESET: &str = "\x1b[0m"; // Reset color

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessState {
    Running,
    Zombie,
    Missing,
}

/// Manages service status information.
pub struct StatusManager {
    /// The PID file containing service names and their respective PIDs.
    pid_file: Arc<Mutex<PidFile>>,
    /// Persistent record of last-seen service states.
    state_file: Arc<Mutex<ServiceStateFile>>,
}

impl StatusManager {
    /// Creates a new `StatusManager` instance.
    pub fn new(
        pid_file: Arc<Mutex<PidFile>>,
        state_file: Arc<Mutex<ServiceStateFile>>,
    ) -> Self {
        Self {
            pid_file,
            state_file,
        }
    }

    fn clear_service_pid(&self, service_name: &str) {
        if let Ok(mut guard) = self.pid_file.lock() {
            let _ = guard.remove(service_name);
        }

        if let Ok(mut state_guard) = self.state_file.lock() {
            let should_update = state_guard
                .get(service_name)
                .map(|entry| matches!(entry.status, ServiceLifecycleStatus::Running))
                .unwrap_or(false);

            if should_update
                && let Err(err) = state_guard.set(
                    service_name,
                    ServiceLifecycleStatus::ExitedWithError,
                    None,
                    None,
                    None,
                )
            {
                debug!(
                    "Failed to reflect cleared PID state for '{service_name}' in state file: {err}"
                );
            }
        }
    }

    fn process_state(pid: u32) -> ProcessState {
        #[cfg(target_os = "linux")]
        {
            let proc_path = format!("/proc/{pid}");
            if !Path::new(&proc_path).exists() {
                return ProcessState::Missing;
            }

            if let Some(state) = Self::read_proc_state(pid)
                && matches!(state, 'Z' | 'X')
            {
                return ProcessState::Zombie;
            }

            ProcessState::Running
        }

        #[cfg(not(target_os = "linux"))]
        {
            let target = Pid::from_raw(pid as i32);
            match signal::kill(target, None) {
                Ok(_) => ProcessState::Running,
                Err(err) => {
                    if err == nix::Error::from(nix::errno::Errno::ESRCH) {
                        ProcessState::Missing
                    } else {
                        ProcessState::Running
                    }
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn read_proc_state(pid: u32) -> Option<char> {
        let stat_path_str = format!("/proc/{pid}/stat");
        let stat_path = Path::new(&stat_path_str);
        let contents = fs::read_to_string(stat_path).ok()?;
        let mut parts = contents.split_whitespace();
        parts.next()?; // pid
        let mut name_part = parts.next()?; // (comm)
        // The state follows the command, but command may contain spaces. The stat format ensures
        // the executable name is wrapped in parentheses, so consume until the closing ')'.
        if !name_part.ends_with(')') {
            for part in parts.by_ref() {
                name_part = part;
                if name_part.ends_with(')') {
                    break;
                }
            }
        }

        parts.next()?.chars().next()
    }

    /// Parses an uptime string in "HH:MM" format and returns a human-readable string.
    pub fn format_uptime(uptime_str: &str) -> String {
        if let Some(total_seconds) = Self::parse_elapsed_seconds(uptime_str) {
            return Self::format_elapsed(total_seconds);
        }

        #[cfg(target_os = "linux")]
        {
            if let Ok(parsed) =
                chrono::DateTime::parse_from_str(uptime_str, "%a %Y-%m-%d %H:%M:%S %Z")
                && let Ok(duration) = chrono::Utc::now()
                    .signed_duration_since(parsed.with_timezone(&chrono::Utc))
                    .to_std()
            {
                return Self::format_elapsed(duration.as_secs());
            }
        }

        "Unknown".to_string()
    }

    fn parse_elapsed_seconds(uptime_str: &str) -> Option<u64> {
        let trimmed = uptime_str.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Handle optional day component in formats like "2-03:04:05" emitted by `ps -o etime`
        let (day_component, time_part) = match trimmed.split_once('-') {
            Some((days, rest)) => (days.trim().parse::<u64>().ok()?, rest),
            None => (0, trimmed),
        };

        let segments: Vec<&str> = time_part.split(':').collect();
        if segments.is_empty() || segments.len() > 3 {
            return None;
        }

        let mut values = [0u64; 3];
        // Right-align parsed values into [hours, minutes, seconds]
        for (idx, segment) in segments.iter().rev().enumerate() {
            values[2 - idx] = segment.trim().parse::<u64>().ok()?;
        }

        let hours = values[0];
        let minutes = values[1];
        let seconds = values[2];

        let total_seconds = seconds
            + minutes.saturating_mul(60)
            + hours.saturating_mul(3600)
            + day_component.saturating_mul(86_400);

        Some(total_seconds)
    }

    fn format_elapsed(total_seconds: u64) -> String {
        match total_seconds {
            0..=59 => format!("{} secs ago", total_seconds),
            60..=3_599 => format!("{} mins ago", total_seconds / 60),
            3_600..=86_399 => format!("{} hours ago", total_seconds / 3_600),
            86_400..=604_799 => format!("{} days ago", total_seconds / 86_400),
            _ => format!("{} weeks ago", total_seconds / 604_800),
        }
    }

    /// Retrieves all child processes of a given PID and nests them properly.
    fn get_child_processes(pid: u32, indent: usize) -> Vec<String> {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);
        let mut children = Vec::new();

        for (proc_pid, process) in system.processes() {
            if let Some(parent) = process.parent()
                && parent.as_u32() == pid
            {
                //let proc_name = process.name().to_string_lossy().to_string();
                let proc_name = Self::get_process_cmdline(proc_pid.as_u32());
                let formatted = format!(
                    "{} ├─{} {}",
                    " ".repeat(indent),
                    proc_pid.as_u32(),
                    proc_name
                );
                children.push(formatted);

                // Recursively add grandchildren, increasing indentation
                let grand_children =
                    Self::get_child_processes(proc_pid.as_u32(), indent + 4);
                children.extend(grand_children);
            }
        }

        children
    }

    /// Shows the status of a **single service** with optional cron designation.
    fn show_status_with_cron_info(&self, service_name: &str, is_cron: bool) {
        let display_name = if is_cron {
            format!("{}[cron]{} {}", YELLOW_BOLD, RESET, service_name)
        } else {
            service_name.to_string()
        };

        self.show_status_impl(&display_name, service_name);
    }

    /// Shows the status of a **single service**.
    pub fn show_status(&self, service_name: &str) {
        self.show_status_impl(service_name, service_name);
    }

    /// Internal implementation for showing service status.
    fn show_status_impl(&self, display_name: &str, service_name: &str) {
        debug!("Checking status for service: {service_name}");
        let state_entry = {
            let guard = self
                .state_file
                .lock()
                .expect("Failed to lock service state file");
            guard.get(service_name).cloned()
        };

        if let Some(entry) = state_entry.clone() {
            match entry.status {
                ServiceLifecycleStatus::Skipped => {
                    println!("● {} - Skipped via configuration", display_name);
                    return;
                }
                ServiceLifecycleStatus::ExitedSuccessfully => {
                    let note = entry
                        .exit_code
                        .map(|code| format!(" (exit code {code})"))
                        .unwrap_or_default();
                    println!(
                        "● {} - {}Exited successfully{}{}",
                        display_name, GREEN_BOLD, note, RESET
                    );
                    return;
                }
                ServiceLifecycleStatus::ExitedWithError => {
                    let detail = match (entry.exit_code, entry.signal) {
                        (Some(code), _) => format!("exit code {code}"),
                        (None, Some(sig)) => format!("signal {sig}"),
                        _ => "unknown reason".to_string(),
                    };
                    println!(
                        "● {} - {}Exited with error ({}){}",
                        display_name, RED_BOLD, detail, RESET
                    );
                    return;
                }
                ServiceLifecycleStatus::Stopped => {
                    println!("● {} - Stopped", display_name);
                    return;
                }
                ServiceLifecycleStatus::Running => {}
            }
        }

        let pid = if let Some(entry) = state_entry.as_ref()
            && let Some(pid) = entry.pid
        {
            pid
        } else {
            let pid_file = self.pid_file.lock().expect("Failed to lock PID file");
            match pid_file.get(service_name) {
                Some(pid) => pid,
                None => {
                    println!("● {} - Not running", display_name);
                    return;
                }
            }
        };

        debug!("Checking status for PID: {pid}");

        match Self::process_state(pid) {
            ProcessState::Running => {}
            ProcessState::Zombie => {
                println!(
                    "● {} - Process {} is zombie (defunct); service is no longer running",
                    display_name, pid
                );
                self.clear_service_pid(service_name);
                return;
            }
            ProcessState::Missing => {
                println!("● {} - Process {} not found", display_name, pid);
                self.clear_service_pid(service_name);
                return;
            }
        }

        let uptime = Self::get_process_uptime(pid);
        let tasks = Self::get_task_count(pid);
        let memory = Self::get_memory_usage(pid);
        let cpu_time = Self::get_cpu_time(pid);
        let process_group = Self::get_process_group(pid);
        let command = Self::get_process_cmdline(pid);
        let child_processes = Self::get_child_processes(pid, 6);
        let uptime_label = Self::format_uptime(&uptime);

        println!("{}● {} Running{}", GREEN_BOLD, display_name, RESET);
        println!(
            "   Active: {}active (running){} since {}; {}",
            GREEN_BOLD, RESET, uptime, uptime_label
        );
        println!(" Main PID: {}", pid);
        println!("    {}Tasks: {} (limit: N/A){}", MAGENTA_BOLD, tasks, RESET);
        println!("   {}Memory: {:.1}M{}", MAGENTA_BOLD, memory, RESET);
        println!("      {}CPU: {:.3}s{}", MAGENTA_BOLD, cpu_time, RESET);
        println!(" Process Group: {}", process_group);

        println!("     |-{} {}", pid, command.trim());
        for child in child_processes {
            println!("{}", child);
        }
    }

    /// Shows the status of **all services**.
    pub fn show_statuses(&self) {
        let mut services: BTreeSet<String> = BTreeSet::new();

        {
            let pid_guard = self.pid_file.lock().expect("Failed to lock PID file");
            services.extend(pid_guard.services().keys().cloned());
        }

        {
            let state_guard = self
                .state_file
                .lock()
                .expect("Failed to lock service state file");
            services.extend(state_guard.services().keys().cloned());
        }

        let cron_state =
            CronStateFile::load().unwrap_or_else(|_| CronStateFile::default());
        services.extend(cron_state.jobs().keys().cloned());

        if services.is_empty() {
            println!("No managed services.");
            return;
        }

        println!("Service statuses:");
        for service in services {
            let is_cron = cron_state.jobs().contains_key(&service);
            self.show_status_with_cron_info(&service, is_cron);
            if let Some(cron_job) = cron_state.jobs().get(&service) {
                Self::print_cron_history(&service, cron_job);
            }
        }
    }

    fn print_cron_history(service_name: &str, job_state: &PersistedCronJobState) {
        let label = if job_state.timezone_label.trim().is_empty() {
            "UTC".to_string()
        } else {
            job_state.timezone_label.trim().to_string()
        };

        println!("  Cron history ({label}) for {service_name}:");

        if job_state.execution_history.is_empty() {
            println!("    - No runs recorded yet.");
            return;
        }

        for record in job_state.execution_history.iter().rev() {
            let timestamp = record.completed_at.unwrap_or(record.started_at);
            let ts =
                Self::format_cron_timestamp(timestamp, job_state.timezone.as_deref());
            let status_str = Self::format_cron_status(record);
            println!("    - {ts} | {status_str}");
        }

        // Visually separate cron details from subsequent services.
        println!();
    }

    fn format_cron_status(record: &CronExecutionRecord) -> String {
        match record.status.as_ref() {
            Some(CronExecutionStatus::Success) => {
                let code = record.exit_code.unwrap_or(0);
                format!("{GREEN_BOLD}exit {code}{RESET}")
            }
            Some(CronExecutionStatus::Failed(reason)) => {
                let base = if let Some(code) = record.exit_code {
                    format!("{RED_BOLD}exit {code}{RESET}")
                } else {
                    format!("{RED_BOLD}failed{RESET}")
                };

                if reason.trim().is_empty() {
                    base
                } else {
                    format!("{base} - {reason}")
                }
            }
            Some(CronExecutionStatus::OverlapError) => {
                format!("{RED_BOLD}overlap detected{RESET}")
            }
            None => format!("{MAGENTA_BOLD}in progress{RESET}"),
        }
    }

    fn format_cron_timestamp(
        time: std::time::SystemTime,
        tz_hint: Option<&str>,
    ) -> String {
        let datetime_utc: DateTime<Utc> = time.into();

        if let Some(hint) = tz_hint {
            if hint.eq_ignore_ascii_case("utc") {
                return datetime_utc.format("%Y-%m-%d %H:%M:%S %Z").to_string();
            }

            if let Ok(tz) = hint.parse::<Tz>() {
                return datetime_utc
                    .with_timezone(&tz)
                    .format("%Y-%m-%d %H:%M:%S %Z")
                    .to_string();
            }
        } else {
            let datetime_local: DateTime<Local> = DateTime::from(time);
            return datetime_local.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        }

        datetime_utc.format("%Y-%m-%d %H:%M:%S %Z").to_string()
    }

    /// Gets the **uptime** of a process.
    fn get_process_uptime(pid: u32) -> String {
        #[cfg(target_os = "linux")]
        {
            let start_time = std::fs::metadata(format!("/proc/{}", pid))
                .and_then(|meta| meta.modified())
                .unwrap_or(UNIX_EPOCH);
            let start_time: DateTime<Utc> = start_time.into();
            start_time.format("%a %Y-%m-%d %H:%M:%S UTC").to_string()
        }

        #[cfg(target_os = "macos")]
        {
            Command::new("ps")
                .arg("-p")
                .arg(pid.to_string())
                .arg("-o")
                .arg("etime=")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string()) // Strip newlines and any extra spaces
                .unwrap_or_else(|| "Unknown".to_string())
        }
    }

    /// Gets the **task count** (threads).
    fn get_task_count(pid: u32) -> u32 {
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("thcount=")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
    }

    /// Gets the **memory usage** in MB.
    fn get_memory_usage(pid: u32) -> f64 {
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("rss=")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|kb| kb / 1024.0)
            .unwrap_or(0.0)
    }

    /// Gets the **CPU time** used by the process.
    fn get_cpu_time(pid: u32) -> f64 {
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("time=")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|s| {
                let time_parts: Vec<&str> = s.trim().split(':').collect();
                match time_parts.as_slice() {
                    [mins, secs] => Some(
                        (mins.parse::<f64>().unwrap_or(0.0) * 60.0)
                            + secs.parse::<f64>().unwrap_or(0.0),
                    ),
                    _ => None,
                }
            })
            .unwrap_or(0.0)
    }

    /// Gets the **process group ID**.
    fn get_process_group(pid: u32) -> String {
        getpgid(Some(Pid::from_raw(pid as i32)))
            .map(|pgid| pgid.to_string())
            .unwrap_or_else(|_| "Unknown".to_string())
    }

    /// Gets the **command line** of a process.
    fn get_process_cmdline(pid: u32) -> String {
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("command=")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .unwrap_or_else(|| "Unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn format_cron_status_success_includes_green_exit_code() {
        let record = CronExecutionRecord {
            started_at: SystemTime::now(),
            completed_at: Some(SystemTime::now()),
            status: Some(CronExecutionStatus::Success),
            exit_code: Some(0),
        };

        let formatted = StatusManager::format_cron_status(&record);
        assert!(formatted.contains("exit 0"));
        assert!(formatted.contains(GREEN_BOLD));
    }

    #[test]
    fn format_cron_status_failure_shows_reason() {
        let record = CronExecutionRecord {
            started_at: SystemTime::now(),
            completed_at: Some(SystemTime::now()),
            status: Some(CronExecutionStatus::Failed("boom".into())),
            exit_code: Some(2),
        };

        let formatted = StatusManager::format_cron_status(&record);
        assert!(formatted.contains("exit 2"));
        assert!(formatted.contains("boom"));
        assert!(formatted.contains(RED_BOLD));
    }
}
