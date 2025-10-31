//! Status management for services in the daemon.
use nix::unistd::{Pid, getpgid};
use std::{
    collections::HashMap,
    process::Command,
    sync::{Arc, Mutex},
};
use sysinfo::{ProcessesToUpdate, System};
use tracing::debug;

use crate::daemon::PidFile;

#[cfg(target_os = "linux")]
use std::time::UNIX_EPOCH;

#[cfg(target_os = "linux")]
use chrono::{DateTime, Utc};

const GREEN_BOLD: &str = "\x1b[1;32m"; // Bright Green
const MAGENTA_BOLD: &str = "\x1b[1;35m"; // Magenta
const RESET: &str = "\x1b[0m"; // Reset color

/// Manages service status information.
pub struct StatusManager {
    /// The PID file containing service names and their respective PIDs.
    pid_file: Arc<Mutex<PidFile>>,
}

impl StatusManager {
    /// Creates a new `StatusManager` instance.
    pub fn new(pid_file: Arc<Mutex<PidFile>>) -> Self {
        Self { pid_file }
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

    /// Shows the status of a **single service**.
    pub fn show_status(&self, service_name: &str) {
        debug!("Checking status for service: {service_name}");
        let pid_file = self.pid_file.lock().expect("Failed to lock PID file");
        let pid = match pid_file.get(service_name) {
            Some(pid) => pid,
            None => {
                println!("● {} - Not running", service_name);
                return;
            }
        };

        debug!("Checking status for PID: {pid}");

        if !Self::is_process_running(pid) {
            println!("● {} - Process {} not found", service_name, pid);
            return;
        }

        let uptime = Self::get_process_uptime(pid);
        let tasks = Self::get_task_count(pid);
        let memory = Self::get_memory_usage(pid);
        let cpu_time = Self::get_cpu_time(pid);
        let process_group = Self::get_process_group(pid);
        let command = Self::get_process_cmdline(pid);
        let child_processes = Self::get_child_processes(pid, 6);
        let uptime_label = Self::format_uptime(&uptime);

        println!("{}● - {} Running{}", GREEN_BOLD, service_name, RESET);
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
        let services: HashMap<String, u32> = self
            .pid_file
            .lock()
            .expect("Failed to lock PID file")
            .services()
            .clone();

        if services.is_empty() {
            println!("No active services.");
            return;
        }

        println!("Active services:");
        for (service, _) in services.iter() {
            self.show_status(service);
        }
    }

    /// Checks if a process is still running.
    fn is_process_running(pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            std::path::Path::new(&format!("/proc/{}", pid)).exists()
        }

        #[cfg(target_os = "macos")]
        {
            Command::new("ps")
                .arg("-p")
                .arg(pid.to_string())
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        }
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
