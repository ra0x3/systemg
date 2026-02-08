#[path = "common/mod.rs"]
mod common;

use std::fs;
#[cfg(target_os = "linux")]
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use assert_cmd::Command;
use common::HomeEnvGuard;
#[cfg(target_os = "linux")]
use common::wait_for_pid_removed;
use serde_json::Value;
#[cfg(target_os = "linux")]
use systemg::daemon::{Daemon, PidFile};
#[cfg(target_os = "linux")]
use systemg::{
    config::SpawnMode, spawn::DynamicSpawnManager, status::collect_runtime_snapshot,
};
use systemg::{
    config::load_config,
    daemon::{ServiceLifecycleStatus, ServiceStateFile},
};
use tempfile::tempdir;

#[test]
fn status_json_falls_back_to_snapshot_without_supervisor() {
    let temp = tempdir().expect("create tempdir");
    let home_guard = HomeEnvGuard::set(temp.path());

    let config_path = temp.path().join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "1"
services:
  demo:
    command: "/bin/true"
"#,
    )
    .expect("write config");

    let config = load_config(Some(config_path.to_string_lossy().as_ref()))
        .expect("load config for hash");
    let service = config.services.get("demo").expect("demo service");
    let hash = service.compute_hash();

    let mut state = ServiceStateFile::load().expect("load state");
    state
        .set(
            &hash,
            ServiceLifecycleStatus::ExitedSuccessfully,
            None,
            Some(0),
            None,
        )
        .expect("persist state");

    let sysg_bin = assert_cmd::cargo::cargo_bin!("sysg");
    let output = Command::new(sysg_bin)
        .arg("status")
        .arg("--config")
        .arg(config_path.as_os_str())
        .arg("--json")
        .output()
        .expect("run sysg status");

    let code = output.status.code().unwrap_or_default();
    assert!(
        (0..=2).contains(&code),
        "status command should exit with a defined health code before supervisor is running, got {code}"
    );

    let payload: Value = serde_json::from_slice(&output.stdout).expect("parse json");
    assert_eq!(payload["overall_health"], "healthy");

    drop(home_guard);
}

#[test]
fn inspect_json_falls_back_without_supervisor() {
    let temp = tempdir().expect("create tempdir");
    let home_guard = HomeEnvGuard::set(temp.path());

    let config_path = temp.path().join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "1"
services:
  demo:
    command: "/bin/true"
"#,
    )
    .expect("write config");

    let config = load_config(Some(config_path.to_string_lossy().as_ref()))
        .expect("load config for hash");
    let service = config.services.get("demo").expect("demo service");
    let hash = service.compute_hash();

    let mut state = ServiceStateFile::load().expect("load state");
    state
        .set(
            &hash,
            ServiceLifecycleStatus::Running,
            Some(4242),
            None,
            None,
        )
        .expect("persist state");

    let sysg_bin = assert_cmd::cargo::cargo_bin!("sysg");
    let output = Command::new(sysg_bin)
        .arg("inspect")
        .arg(&hash)
        .arg("--config")
        .arg(config_path.as_os_str())
        .arg("--json")
        .output()
        .expect("run sysg inspect");

    let code = output.status.code().unwrap_or_default();
    assert!(
        (0..=2).contains(&code),
        "inspect command should exit with a health code between 0 and 2, got {code}"
    );

    let payload: Value = serde_json::from_slice(&output.stdout).expect("parse json");
    assert_eq!(payload["unit"]["hash"], hash);
    assert!(payload["samples"].as_array().unwrap().is_empty());

    drop(home_guard);
}

#[cfg(target_os = "linux")]
#[test]
fn status_reports_untracked_descendants() {
    let temp = tempdir().expect("create tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home dir");
    let _home_guard = HomeEnvGuard::set(&home);

    let script_path = temp.path().join("spawn_children.py");
    fs::write(
        &script_path,
        r#"#!/usr/bin/env python3
import signal
import subprocess
import sys
import time

running = True


def handle_stop(signum, frame):
    global running
    running = False


signal.signal(signal.SIGTERM, handle_stop)
signal.signal(signal.SIGINT, handle_stop)

inner = "import subprocess,time\nproc = subprocess.Popen(['sleep','30'])\ntry:\n    time.sleep(30)\nfinally:\n    if proc.poll() is None:\n        proc.terminate()\n        try:\n            proc.wait(timeout=5)\n        except subprocess.TimeoutExpired:\n            proc.kill()\n"

child = subprocess.Popen([sys.executable, "-c", inner])

try:
    while running:
        time.sleep(0.25)
finally:
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
"#,
    )
    .expect("write spawn script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod script");
    }

    let config_path = temp.path().join("systemg.yaml");
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  root:
    command: "python3 {}"
    restart_policy: "never"
    spawn:
      mode: "dynamic"
      limits:
        children: 5
        depth: 3
        descendants: 10
"#,
            script_path.display()
        ),
    )
    .expect("write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let service_config = config.services.get("root").expect("root service");

    let daemon = Daemon::from_config(config.clone(), false).expect("daemon from config");

    let spawn_manager = DynamicSpawnManager::new();
    if let Some(spawn) = service_config.spawn.as_ref()
        && matches!(spawn.mode, Some(SpawnMode::Dynamic))
        && let Some(limits) = spawn.limits.as_ref()
    {
        spawn_manager
            .register_service("root".to_string(), limits)
            .expect("register service limits");
    }

    daemon.start_services().expect("start services");
    let root_pid = common::wait_for_pid("root");
    eprintln!("Root service started with PID: {}", root_pid);
    spawn_manager.register_service_pid("root".to_string(), root_pid);

    #[cfg(target_os = "linux")]
    thread::sleep(Duration::from_secs(2)); // Give ample time for child processes on Linux
    #[cfg(not(target_os = "linux"))]
    thread::sleep(Duration::from_millis(200));

    // Check if Python actually spawned children
    #[cfg(unix)]
    {
        use std::process::Command;
        let output = Command::new("pgrep")
            .arg("-P")
            .arg(root_pid.to_string())
            .output();
        if let Ok(output) = output {
            let children = String::from_utf8_lossy(&output.stdout);
            eprintln!(
                "Direct children of root PID {}: {}",
                root_pid,
                children.trim()
            );
        }
    }

    let config_arc = daemon.config();
    let pid_handle = daemon.pid_file_handle();
    let state_handle = daemon.service_state_handle();

    let deadline = Instant::now() + Duration::from_secs(20); // Extended timeout for Linux
    let mut found_python = false;
    let mut found_sleep = false;
    let mut iteration = 0;

    while Instant::now() < deadline {
        iteration += 1;
        let snapshot = collect_runtime_snapshot(
            Arc::clone(&config_arc),
            &pid_handle,
            &state_handle,
            None,
            Some(&spawn_manager),
        )
        .expect("collect snapshot");

        if let Some(unit) = snapshot.units.iter().find(|unit| unit.name == "root") {
            if iteration == 1 || iteration % 10 == 0 {
                eprintln!(
                    "Iteration {}: Root unit found, spawned_children count: {}",
                    iteration,
                    unit.spawned_children.len()
                );
            }
            found_python = false;
            found_sleep = false;

            fn walk(
                node: &systemg::status::SpawnedProcessNode,
                python: &mut bool,
                sleep: &mut bool,
            ) {
                let command = node.child.command.as_str();
                if command.contains("python") {
                    *python = true;
                }
                if command.contains("sleep") {
                    *sleep = true;
                }
                for child in &node.children {
                    walk(child, python, sleep);
                }
            }

            for child in &unit.spawned_children {
                walk(child, &mut found_python, &mut found_sleep);
            }

            if found_python && found_sleep {
                break;
            }
        }

        thread::sleep(Duration::from_millis(100));
    }

    // Add debug output if test fails
    if !found_python || !found_sleep {
        eprintln!(
            "Test failure - found_python: {}, found_sleep: {}",
            found_python, found_sleep
        );

        // Collect one final snapshot for debugging
        if let Ok(snapshot) = collect_runtime_snapshot(
            Arc::clone(&config_arc),
            &pid_handle,
            &state_handle,
            None,
            Some(&spawn_manager),
        ) && let Some(unit) = snapshot.units.iter().find(|unit| unit.name == "root")
        {
            eprintln!(
                "Root unit spawned_children count: {}",
                unit.spawned_children.len()
            );
            for child in &unit.spawned_children {
                eprintln!(
                    "  Child: {} (pid: {})",
                    child.child.command, child.child.pid
                );
            }
        }
    }

    assert!(
        found_python,
        "expected python supervisor child to be visible in status"
    );
    assert!(
        found_sleep,
        "expected nested sleep grandchild to be visible in status"
    );

    daemon.stop_services().ok();
    daemon.shutdown_monitor();
}

#[cfg(target_os = "linux")]
#[test]
fn status_flags_zombie_processes() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  arb_rs:
    command: "sleep 60"
"#,
    )
    .expect("failed to write config");

    let child_pid = unsafe { libc::fork() };
    assert!(child_pid >= 0, "fork failed");

    if child_pid == 0 {
        unsafe { libc::_exit(0) };
    }

    let mut pid_file = PidFile::load().expect("load pid file");
    pid_file
        .insert("arb_rs", child_pid as u32)
        .expect("insert zombie pid");

    wait_for_z_state(child_pid as u32);

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("status")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .arg("--service")
        .arg("arb_rs")
        .arg("--json")
        .arg("--no-color")
        .output()
        .expect("run sysg status for zombie detection");

    let payload: Value =
        serde_json::from_slice(&output.stdout).expect("parse status json");
    let expected_exit = match payload["overall_health"].as_str() {
        Some("healthy") => 0,
        Some("degraded") => 1,
        Some("failing") => 2,
        _ => 2,
    };
    let code = output.status.code().unwrap_or_default();
    assert_eq!(
        code, expected_exit,
        "unexpected exit code for zombie process"
    );
    assert_eq!(payload["overall_health"].as_str(), Some("failing"));

    let units = payload["units"].as_array().expect("units array");
    let unit = units
        .iter()
        .find(|entry| entry["name"].as_str() == Some("arb_rs"))
        .expect("arb_rs unit present");
    assert_eq!(
        unit["process"]["state"].as_str(),
        Some("zombie"),
        "arb_rs process should be classified as zombie"
    );
    assert_eq!(unit["health"].as_str(), Some("failing"));

    unsafe {
        let mut status: libc::c_int = 0;
        libc::waitpid(child_pid, &mut status, 0);
    }
}

#[cfg(target_os = "linux")]
fn wait_for_z_state(pid: u32) {
    use std::{
        path::Path,
        time::{Duration, Instant},
    };

    let path = format!("/proc/{pid}/stat");
    let stat_path = Path::new(&path);
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(stat_path) {
            // find state char following the right parenthesis per procfs format
            if let Some(state_char) = contents
                .split_whitespace()
                .nth(2)
                .and_then(|field| field.chars().next())
                && (state_char == 'Z' || state_char == 'X')
            {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn status_reports_skipped_services() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  skipped_service:
    command: "echo should be skipped"
    skip: true
"#,
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("daemon from config");

    daemon
        .start_service(
            "skipped_service",
            config.services.get("skipped_service").unwrap(),
        )
        .expect("start skipped service");

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("status")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .arg("--json")
        .arg("--no-color")
        .output()
        .expect("run sysg status for skipped service");

    let payload: Value =
        serde_json::from_slice(&output.stdout).expect("parse status json");
    let expected_exit = match payload["overall_health"].as_str() {
        Some("healthy") => 0,
        Some("degraded") => 1,
        Some("failing") => 2,
        _ => 1,
    };
    assert_eq!(output.status.code(), Some(expected_exit));
    // Skipped services have Inactive health, which doesn't affect overall health
    assert_eq!(payload["overall_health"].as_str(), Some("healthy"));

    let units = payload["units"].as_array().expect("units array");
    let unit = units
        .iter()
        .find(|entry| entry["name"].as_str() == Some("skipped_service"))
        .expect("skipped service unit present");
    assert_eq!(unit["lifecycle"].as_str(), Some("skipped"));
    assert_eq!(unit["health"].as_str(), Some("inactive"));
    assert!(
        unit.get("process").is_none() || unit["process"].is_null(),
        "skipped service should not report a running process"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn status_reports_successful_exit() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  oneshot:
    command: "sh -c 'echo done'"
    restart_policy: "never"
"#,
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("daemon from config");

    daemon
        .start_service("oneshot", config.services.get("oneshot").unwrap())
        .expect("start oneshot");

    wait_for_pid_removed("oneshot");

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("status")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .arg("--json")
        .arg("--no-color")
        .output()
        .expect("run sysg status for exited service");

    let payload: Value =
        serde_json::from_slice(&output.stdout).expect("parse status json");
    let expected_exit = match payload["overall_health"].as_str() {
        Some("healthy") => 0,
        Some("degraded") => 1,
        Some("failing") => 2,
        _ => 0,
    };
    assert_eq!(output.status.code(), Some(expected_exit));
    assert_eq!(payload["overall_health"].as_str(), Some("healthy"));

    let units = payload["units"].as_array().expect("units array");
    let unit = units
        .iter()
        .find(|entry| entry["name"].as_str() == Some("oneshot"))
        .expect("oneshot unit present");
    assert_eq!(unit["lifecycle"].as_str(), Some("exited_successfully"));
    assert_eq!(unit["health"].as_str(), Some("healthy"));
    assert_eq!(
        unit["last_exit"]["exit_code"].as_i64(),
        Some(0),
        "exit code should be recorded as 0"
    );
}
