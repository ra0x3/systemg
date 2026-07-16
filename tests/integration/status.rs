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
use systemg::{
    config::load_config,
    daemon::{ServiceLifecycleStatus, ServiceStateFile},
    ipc::InspectPayload,
    state_store::StateStore,
    status::{OverallHealth, StatusSnapshot},
};
#[cfg(target_os = "linux")]
use systemg::{
    config::{SpawnMode, StatusSnapshotMode},
    spawn::DynamicSpawnManager,
    status::collect_runtime_snapshot,
};
use tempfile::tempdir;

#[test]
fn status_without_config_reports_missing_supervisor_not_missing_manifest() {
    let temp = tempdir().expect("create tempdir");
    let home_guard = HomeEnvGuard::set(temp.path());

    let sysg_bin = assert_cmd::cargo::cargo_bin!("sysg");
    let output = Command::new(sysg_bin)
        .arg("status")
        .args(["--format", "json"])
        .output()
        .expect("run sysg status");

    assert!(
        !output.status.success(),
        "status should fail when no supervisor is running and no config was requested"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No running supervisor"),
        "stderr should explain the missing supervisor, got: {stderr}"
    );
    assert!(
        !stderr.contains("systemg.yaml"),
        "plain status should not try to load a default manifest, got: {stderr}"
    );

    drop(home_guard);
}

#[test]
fn status_json_falls_back_to_snapshot_without_supervisor() {
    let temp = tempdir().expect("create tempdir");
    let home_guard = HomeEnvGuard::set(temp.path());

    let config_path = temp.path().join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
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

    let mut state = ServiceStateFile::load(StateStore::for_project(&config.project.id))
        .expect("load state");
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
        .args(["--format", "json"])
        .output()
        .expect("run sysg status");

    // With NO supervisor running, status must NOT present the disk state as a
    // supervised HEALTHY stack. It exits non-zero (2) and names SG0206 on
    // stderr, while still serializing the disk-read units on stdout.
    let code = output.status.code().unwrap_or_default();
    assert_eq!(
        code, 2,
        "status with no supervisor must exit 2 (offline), got {code}"
    );

    let payload: Value = serde_json::from_slice(&output.stdout).expect("parse json");
    assert!(
        payload.get("units").is_some(),
        "offline status should still serialize the disk-read units"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SG0206"),
        "offline status must name SG0206 on stderr, got: {stderr}"
    );

    drop(home_guard);
}

#[test]
fn status_format_defaults_to_json_when_value_is_omitted() {
    let temp = tempdir().expect("create tempdir");
    let home_guard = HomeEnvGuard::set(temp.path());

    let config_path = temp.path().join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
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

    let mut state = ServiceStateFile::load(StateStore::for_project(&config.project.id))
        .expect("load state");
    state
        .set(
            &hash,
            ServiceLifecycleStatus::ExitedSuccessfully,
            None,
            Some(0),
            None,
        )
        .expect("persist state");

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("status")
        .arg("--config")
        .arg(config_path.as_os_str())
        .arg("--format")
        .output()
        .expect("run sysg status");

    let payload: Value = serde_json::from_slice(&output.stdout)
        .expect("bare --format should default to json output");
    assert_eq!(payload["overall_health"], "healthy");

    drop(home_guard);
}

#[test]
fn status_xml_falls_back_to_snapshot_without_supervisor() {
    let temp = tempdir().expect("create tempdir");
    let home_guard = HomeEnvGuard::set(temp.path());

    let config_path = temp.path().join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
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

    let mut state = ServiceStateFile::load(StateStore::for_project(&config.project.id))
        .expect("load state");
    state
        .set(
            &hash,
            ServiceLifecycleStatus::ExitedSuccessfully,
            None,
            Some(0),
            None,
        )
        .expect("persist state");

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("status")
        .arg("--config")
        .arg(config_path.as_os_str())
        .args(["--format", "xml"])
        .output()
        .expect("run sysg status");

    let xml = std::str::from_utf8(&output.stdout).expect("status xml is utf8");
    let payload: StatusSnapshot = quick_xml::de::from_str(xml).expect("parse status xml");
    assert_eq!(payload.overall_health, OverallHealth::Healthy);
    assert_eq!(payload.units.len(), 1);
    assert_eq!(payload.units[0].hash, hash);

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
version: "2"
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

    let mut state = ServiceStateFile::load(StateStore::for_project(&config.project.id))
        .expect("load state");
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
        .arg("--service")
        .arg("demo")
        .arg("--config")
        .arg(config_path.as_os_str())
        .args(["--format", "json"])
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
    assert!(
        output.stderr.is_empty(),
        "inspect json output should not emit progress noise on stderr in non-interactive mode"
    );

    drop(home_guard);
}

#[test]
fn inspect_xml_falls_back_without_supervisor() {
    let temp = tempdir().expect("create tempdir");
    let home_guard = HomeEnvGuard::set(temp.path());

    let config_path = temp.path().join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
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

    let mut state = ServiceStateFile::load(StateStore::for_project(&config.project.id))
        .expect("load state");
    state
        .set(
            &hash,
            ServiceLifecycleStatus::Running,
            Some(4242),
            None,
            None,
        )
        .expect("persist state");

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("inspect")
        .arg("--service")
        .arg("demo")
        .arg("--config")
        .arg(config_path.as_os_str())
        .args(["--format", "xml"])
        .output()
        .expect("run sysg inspect");

    let xml = std::str::from_utf8(&output.stdout).expect("inspect xml is utf8");
    let payload: InspectPayload =
        quick_xml::de::from_str(xml).expect("parse inspect xml");
    let unit = payload.unit.expect("inspect xml should include unit");
    assert_eq!(unit.hash, hash);
    assert!(payload.samples.is_empty());

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
        r#"version: "2"
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
        .args(["--format", "json"])
        .arg("--no-color")
        .output()
        .expect("run sysg status for zombie detection");

    let payload: Value =
        serde_json::from_slice(&output.stdout).expect("parse status json");
    let code = output.status.code().unwrap_or_default();
    assert_eq!(
        code, 2,
        "status with no supervisor must exit 2 (offline), got {code}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SG0206"),
        "offline status must name SG0206 on stderr, got: {stderr}"
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
        r#"version: "2"
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
        .args(["--format", "json"])
        .arg("--no-color")
        .output()
        .expect("run sysg status for skipped service");

    let payload: Value =
        serde_json::from_slice(&output.stdout).expect("parse status json");
    let code = output.status.code().unwrap_or_default();
    assert_eq!(
        code, 2,
        "status with no supervisor must exit 2 (offline), got {code}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SG0206"),
        "offline status must name SG0206 on stderr, got: {stderr}"
    );
    assert_eq!(payload["overall_health"].as_str(), Some("healthy"));

    let units = payload["units"].as_array().expect("units array");
    let unit = units
        .iter()
        .find(|entry| entry["name"].as_str() == Some("skipped_service"))
        .expect("skipped service unit present");
    assert_eq!(unit["lifecycle"].as_str(), Some("skipped"));
    assert_eq!(unit["state"].as_str(), Some("skipped"));
    assert_eq!(unit["intent"].as_str(), Some("skip"));
    assert_eq!(unit["health"].as_str(), Some("idle"));
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
        r#"version: "2"
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
        .args(["--format", "json"])
        .arg("--no-color")
        .output()
        .expect("run sysg status for exited service");

    let payload: Value =
        serde_json::from_slice(&output.stdout).expect("parse status json");
    let code = output.status.code().unwrap_or_default();
    assert_eq!(
        code, 2,
        "status with no supervisor must exit 2 (offline), got {code}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SG0206"),
        "offline status must name SG0206 on stderr, got: {stderr}"
    );
    assert_eq!(payload["overall_health"].as_str(), Some("healthy"));

    let units = payload["units"].as_array().expect("units array");
    let unit = units
        .iter()
        .find(|entry| entry["name"].as_str() == Some("oneshot"))
        .expect("oneshot unit present");
    assert_eq!(unit["lifecycle"].as_str(), Some("exited_successfully"));
    assert_eq!(unit["state"].as_str(), Some("done"));
    assert_eq!(unit["intent"].as_str(), Some("once"));
    assert_eq!(unit["health"].as_str(), Some("healthy"));
    assert_eq!(
        unit["last_exit"]["exit_code"].as_i64(),
        Some(0),
        "exit code should be recorded as 0"
    );
}
