#[path = "common/mod.rs"]
mod common;

use std::{fs, thread, time::Duration};

use assert_cmd::{Command, cargo::cargo_bin_cmd};
use common::HomeEnvGuard;
use systemg::{
    config::load_config,
    daemon::{PidFile, ServiceLifecycleStatus, ServiceStateFile},
    diag::SgCode,
    state_store::StateStore,
};
use tempfile::tempdir;

/// Every project's state store (single-project test configs have exactly one).
fn all_project_stores() -> Vec<StateStore> {
    let projects_dir = systemg::runtime::state_dir().join("projects");
    match std::fs::read_dir(&projects_dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| StateStore::for_project(&e.file_name().to_string_lossy()))
            .collect(),
        Err(_) => vec![],
    }
}

/// Returns the first project service-state file that records `key`.
fn any_state_file_with(key: &str) -> Option<ServiceStateFile> {
    all_project_stores()
        .into_iter()
        .filter_map(|s| ServiceStateFile::load(s).ok())
        .find(|s| s.get(key).is_some())
}

#[test]
/// Verifies forced purge removes tracked state without hiding configured services.
fn forced_purge_retains_config_services() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("sysg.yaml");
    let config_yaml = r#"
version: "2"
services:
  sample:
    command: "sleep 30"
    restart_policy: never
"#;
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let service_key = config.state_key("sample");
    let mut exited = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn short-lived process");
    exited.wait().expect("reap short-lived process");
    let dead_pid = exited.id();

    let store = StateStore::for_project(&config.project.id);
    let mut state_file = ServiceStateFile::load(store.clone()).unwrap_or_default();
    state_file
        .set(
            &service_key,
            ServiceLifecycleStatus::Running,
            Some(dead_pid),
            None,
            None,
        )
        .unwrap();
    drop(state_file);

    let mut pid_file = PidFile::load(store).unwrap_or_default();
    pid_file.insert("sample", dead_pid).unwrap();
    drop(pid_file);

    let status_before = cargo_bin_cmd!("sysg")
        .env("HOME", &home)
        .arg("status")
        .arg("-c")
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("status before purge to execute");
    assert!(
        !status_before.status.success(),
        "disk-only status should report the supervisor as offline"
    );
    let stderr_before = String::from_utf8_lossy(&status_before.stderr);
    assert!(stderr_before.contains(SgCode::SupervisorOffline.as_str()));
    let stdout_before = String::from_utf8_lossy(&status_before.stdout);
    assert!(
        stdout_before.contains("sample"),
        "Expected seeded service to appear before purge"
    );

    let purge_output = cargo_bin_cmd!("sysg")
        .env("HOME", &home)
        .arg("purge")
        .arg("--force")
        .output()
        .expect("purge command to execute");
    assert!(
        purge_output.status.success(),
        "purge failed: {}",
        String::from_utf8_lossy(&purge_output.stderr)
    );

    let status_after = cargo_bin_cmd!("sysg")
        .env("HOME", &home)
        .arg("status")
        .arg("-c")
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("status after purge to execute");
    assert!(
        !status_after.status.success(),
        "disk-only status should remain offline after purge"
    );
    let stderr_after = String::from_utf8_lossy(&status_after.stderr);
    assert!(stderr_after.contains(SgCode::SupervisorOffline.as_str()));
    let stdout_after = String::from_utf8_lossy(&status_after.stdout);
    assert!(
        stdout_after.contains("sample"),
        "Expected configured service to remain visible after purge"
    );
    assert!(
        stdout_after.contains("Stopped"),
        "Service should show as Stopped after purge"
    );
}

/// Builds a project manifest whose worker writes `tag` before sleeping.
fn config_with_marker(marker: &std::path::Path, tag: &str) -> String {
    format!(
        r#"
version: '2'
project:
  id: repro-proj
services:
  worker:
    command: "sh -c 'echo {tag} > {} && exec sleep 300'"
    restart_policy: always
"#,
        marker.display()
    )
}

/// Waits until the worker marker contains `expected`.
fn wait_for_marker_value(marker: &std::path::Path, expected: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(value) = fs::read_to_string(marker)
            && value.trim() == expected
        {
            return;
        }
        if std::time::Instant::now() >= deadline {
            let got = fs::read_to_string(marker).unwrap_or_default();
            panic!("marker never became '{expected}' (last: '{}')", got.trim());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Returns the stable lifecycle state key for `service`.
fn service_key(config_path: &std::path::Path, service: &str) -> String {
    load_config(Some(config_path.to_str().unwrap()))
        .unwrap()
        .state_key(service)
}

/// Waits until the state entry under `key` is recorded as running.
fn wait_for_state_running(key: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(state) = any_state_file_with(key)
            && let Some(entry) = state.get(key)
            && matches!(entry.status, ServiceLifecycleStatus::Running)
        {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("state file never reported key '{key}' as Running");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Regression: `restart -s <svc> -c <config>` must reconcile a changed command
/// while retaining the service's stable lifecycle identity.
#[test]
fn restart_with_config_reconciles_changed_command() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let marker = dir.join("marker.txt");
    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_with_marker(&marker, "V1")).unwrap();
    let key_v1 = service_key(&config_path, "worker");

    let sysg_bin = assert_cmd::cargo::cargo_bin!("sysg");
    Command::new(sysg_bin)
        .args(["start", "--config"])
        .arg(&config_path)
        .arg("--daemonize")
        .assert()
        .success();

    wait_for_marker_value(&marker, "V1");
    wait_for_state_running(&key_v1);

    fs::write(&config_path, config_with_marker(&marker, "V2")).unwrap();
    let key_v2 = service_key(&config_path, "worker");
    assert_eq!(key_v1, key_v2, "service state identity must remain stable");

    Command::new(sysg_bin)
        .args(["restart", "--service", "worker", "--config"])
        .arg(&config_path)
        .assert()
        .success();

    wait_for_marker_value(&marker, "V2");
    wait_for_state_running(&key_v2);

    Command::new(sysg_bin)
        .arg("stop")
        .arg("--supervisor")
        .assert()
        .success();
}

/// Regression: `stop -p <project>` then `start -c <edited-config>` must launch the new
/// command from the manifest, not the stale cached command.
#[test]
fn stop_project_then_start_reconciles_changed_command() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let marker = dir.join("marker.txt");
    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_with_marker(&marker, "V1")).unwrap();
    let key_v1 = service_key(&config_path, "worker");

    let sysg_bin = assert_cmd::cargo::cargo_bin!("sysg");
    Command::new(sysg_bin)
        .args(["start", "--config"])
        .arg(&config_path)
        .arg("--daemonize")
        .assert()
        .success();

    wait_for_marker_value(&marker, "V1");
    wait_for_state_running(&key_v1);

    Command::new(sysg_bin)
        .args(["stop", "--project", "repro-proj"])
        .assert()
        .success();

    thread::sleep(Duration::from_millis(300));
    let _ = fs::remove_file(&marker);

    fs::write(&config_path, config_with_marker(&marker, "V2")).unwrap();
    let key_v2 = service_key(&config_path, "worker");
    assert_eq!(key_v1, key_v2, "service state identity must remain stable");
    Command::new(sysg_bin)
        .args(["start", "--config"])
        .arg(&config_path)
        .arg("--daemonize")
        .assert()
        .success();

    wait_for_marker_value(&marker, "V2");
    wait_for_state_running(&key_v2);

    Command::new(sysg_bin)
        .arg("stop")
        .arg("--supervisor")
        .assert()
        .success();
}
