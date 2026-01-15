#![allow(dead_code)]

use std::{
    env, fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use sysinfo::{Pid, ProcessesToUpdate, System};
use systemg::daemon::PidFile;

pub struct HomeEnvGuard {
    previous: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl HomeEnvGuard {
    pub fn set(home: &Path) -> Self {
        let lock = systemg::test_utils::env_lock();
        let previous = env::var("HOME").ok();
        unsafe {
            env::set_var("HOME", home);
        }
        systemg::runtime::init(systemg::runtime::RuntimeMode::User);
        systemg::runtime::set_drop_privileges(false);
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe {
                env::set_var("HOME", value);
            },
            None => unsafe {
                env::remove_var("HOME");
            },
        }
        systemg::runtime::init(systemg::runtime::RuntimeMode::User);
        systemg::runtime::set_drop_privileges(false);
    }
}

pub fn wait_for_lines(path: &Path, expected: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(content) = fs::read_to_string(path) {
            let lines: Vec<_> = content.lines().map(|line| line.to_string()).collect();
            if lines.len() >= expected {
                return lines;
            }
        }

        if Instant::now() >= deadline {
            panic!("Timed out waiting for {expected} lines in {:?}", path);
        }

        thread::sleep(Duration::from_millis(100));
    }
}

pub fn wait_for_file_value(path: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(content) = fs::read_to_string(path)
            && content.trim() == expected
        {
            return;
        }

        if Instant::now() >= deadline {
            panic!("Timed out waiting for value '{}' in {:?}", expected, path);
        }

        thread::sleep(Duration::from_millis(100));
    }
}

pub fn wait_for_pid(service: &str) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(pid_file) = PidFile::load()
            && let Some(pid) = pid_file.pid_for(service)
        {
            return pid;
        }

        if Instant::now() >= deadline {
            panic!("Timed out waiting for PID entry for service '{service}'");
        }

        thread::sleep(Duration::from_millis(100));
    }
}

pub fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("Timed out waiting for {:?} to exist", path);
}

pub fn wait_for_pid_removed(service: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(pid_file) = PidFile::load()
            && pid_file.pid_for(service).is_none()
        {
            return;
        }

        if Instant::now() >= deadline {
            panic!("Timed out waiting for service '{}' to clear PID", service);
        }

        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "linux")]
pub fn wait_for_process_exit(pid: u32) {
    use std::path::PathBuf;

    let deadline = Instant::now() + Duration::from_secs(10);
    let proc_path = PathBuf::from(format!("/proc/{}", pid));
    let stat_path = PathBuf::from(format!("/proc/{}/stat", pid));

    while Instant::now() < deadline {
        if !proc_path.exists() {
            return;
        }

        // Check if process is a zombie (killed but not yet reaped)
        if let Ok(stat) = fs::read_to_string(&stat_path) {
            // The third field in /proc/{pid}/stat is the state character
            // Z = zombie, X = dead
            if let Some(state_start) = stat.rfind(')') {
                let state_part = &stat[state_start + 1..].trim();
                if let Some(state_char) = state_part.chars().next()
                    && (state_char == 'Z' || state_char == 'X')
                {
                    return; // Process is dead/zombie, consider it exited
                }
            }
        }

        thread::sleep(Duration::from_millis(100));
    }

    panic!("Timed out waiting for PID {} to exit", pid);
}

pub fn is_process_alive(pid: u32) -> bool {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    system.process(Pid::from_u32(pid)).is_some()
}

pub fn wait_for_latest_pid(pid_dir: &Path, min_runs: usize) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut entries: Vec<_> = fs::read_dir(pid_dir)
            .ok()
            .into_iter()
            .flat_map(|iter| iter.filter_map(Result::ok))
            .filter_map(|entry| {
                let raw_name = entry.file_name();
                let name = raw_name.to_str()?;
                let rest = name.strip_prefix("run_")?;
                let stem = rest.strip_suffix(".pid")?;
                let idx = stem.parse::<usize>().ok()?;
                Some((idx, entry.path()))
            })
            .collect();

        if entries.len() >= min_runs {
            entries.sort_by_key(|(idx, _)| *idx);
            if let Some((_, path)) = entries.last()
                && let Ok(contents) = fs::read_to_string(path)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                return pid;
            }
        }

        if Instant::now() >= deadline {
            panic!(
                "Timed out waiting for at least {min_runs} pid captures in {:?}",
                pid_dir
            );
        }

        thread::sleep(Duration::from_millis(100));
    }
}
