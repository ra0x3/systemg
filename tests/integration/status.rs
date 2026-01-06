#[path = "common/mod.rs"]
mod common;

use assert_cmd::Command;
use common::HomeEnvGuard;
#[cfg(target_os = "linux")]
use common::wait_for_pid_removed;
use serde_json::Value;
use std::fs;
#[cfg(target_os = "linux")]
use systemg::daemon::{Daemon, PidFile};
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
    use std::path::Path;
    use std::time::{Duration, Instant};

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
