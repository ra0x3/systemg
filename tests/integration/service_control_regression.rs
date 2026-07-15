#[path = "common/mod.rs"]
mod common;

use std::{fs, thread, time::Duration};

use assert_cmd::{Command, cargo::cargo_bin_cmd};
use common::HomeEnvGuard;
use systemg::{
    config::load_config,
    daemon::{PidFile, ServiceLifecycleStatus, ServiceStateFile},
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

/// First project service-state file that records `hash`.
fn any_state_file_with(hash: &str) -> Option<ServiceStateFile> {
    all_project_stores()
        .into_iter()
        .filter_map(|s| ServiceStateFile::load(s).ok())
        .find(|s| s.get(hash).is_some())
}

#[test]
fn purge_retains_config_services() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("sysg.yaml");
    let config_yaml = r#"
version: "1"
services:
  sample:
    command: "sleep 30"
    restart_policy: never
"#;
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let service_hash = config
        .services
        .get("sample")
        .expect("service exists")
        .compute_hash();

    // Seed some runtime state so status shows the service prior to purge.
    let store = StateStore::for_project(&config.project.id);
    let mut state_file = ServiceStateFile::load(store.clone()).unwrap_or_default();
    state_file
        .set(
            &service_hash,
            ServiceLifecycleStatus::Running,
            Some(4242),
            None,
            None,
        )
        .unwrap();
    drop(state_file);

    let mut pid_file = PidFile::load(store).unwrap_or_default();
    pid_file.insert("sample", 4242).unwrap();
    drop(pid_file);

    let status_before = cargo_bin_cmd!("sysg")
        .env("HOME", &home)
        .arg("status")
        .arg("-c")
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("status before purge to execute");
    let before_code = status_before.status.code().unwrap_or_default();
    assert!(
        before_code == 0 || before_code == 1,
        "status command should exit healthy or warn before purge, got {before_code}"
    );
    let stdout_before = String::from_utf8_lossy(&status_before.stdout);
    assert!(
        stdout_before.contains("sample"),
        "Expected seeded service to appear before purge"
    );

    let purge_output = cargo_bin_cmd!("sysg")
        .env("HOME", &home)
        .arg("purge")
        .output()
        .expect("purge command to execute");
    assert!(purge_output.status.success());

    let status_after = cargo_bin_cmd!("sysg")
        .env("HOME", &home)
        .arg("status")
        .arg("-c")
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("status after purge to execute");
    let after_code = status_after.status.code().unwrap_or_default();
    assert!(
        after_code == 0 || after_code == 1,
        "status command should exit healthy or warn after purge, got {after_code}"
    );
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

fn config_with_marker(marker: &std::path::Path, tag: &str) -> String {
    format!(
        r#"
version: '1'
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

fn service_hash(config_path: &std::path::Path, service: &str) -> String {
    load_config(Some(config_path.to_str().unwrap()))
        .unwrap()
        .get_service_hash(service)
        .expect("service defined in config")
}

fn wait_for_state_running(hash: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(state) = any_state_file_with(hash)
            && let Some(entry) = state.get(hash)
            && matches!(entry.status, ServiceLifecycleStatus::Running)
        {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("state file never reported hash '{hash}' as Running");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Regression: `restart -s <svc> -c <config>` must reconcile a changed command from
/// the manifest. The spawned command AND the stored per-hash lifecycle state must
/// track the new command; the stale hash must not remain the tracked Running entry.
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
    let hash_v1 = service_hash(&config_path, "worker");

    let sysg_bin = assert_cmd::cargo::cargo_bin!("sysg");
    Command::new(sysg_bin)
        .args(["start", "--config"])
        .arg(&config_path)
        .arg("--daemonize")
        .assert()
        .success();

    wait_for_marker_value(&marker, "V1");
    wait_for_state_running(&hash_v1);

    fs::write(&config_path, config_with_marker(&marker, "V2")).unwrap();
    let hash_v2 = service_hash(&config_path, "worker");
    assert_ne!(hash_v1, hash_v2, "changed command must change the hash");

    Command::new(sysg_bin)
        .args(["restart", "--service", "worker", "--config"])
        .arg(&config_path)
        .assert()
        .success();

    wait_for_marker_value(&marker, "V2");
    wait_for_state_running(&hash_v2);

    let state = any_state_file_with(&hash_v2).expect("state file with v2 hash");
    let v1_running = state
        .get(&hash_v1)
        .is_some_and(|entry| matches!(entry.status, ServiceLifecycleStatus::Running));
    assert!(
        !v1_running,
        "stale V1 hash must not remain a Running entry after config reconcile"
    );

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
    let hash_v1 = service_hash(&config_path, "worker");

    let sysg_bin = assert_cmd::cargo::cargo_bin!("sysg");
    Command::new(sysg_bin)
        .args(["start", "--config"])
        .arg(&config_path)
        .arg("--daemonize")
        .assert()
        .success();

    wait_for_marker_value(&marker, "V1");
    wait_for_state_running(&hash_v1);

    Command::new(sysg_bin)
        .args(["stop", "--project", "repro-proj"])
        .assert()
        .success();

    thread::sleep(Duration::from_millis(300));
    let _ = fs::remove_file(&marker);

    fs::write(&config_path, config_with_marker(&marker, "V2")).unwrap();
    let hash_v2 = service_hash(&config_path, "worker");
    Command::new(sysg_bin)
        .args(["start", "--config"])
        .arg(&config_path)
        .arg("--daemonize")
        .assert()
        .success();

    wait_for_marker_value(&marker, "V2");
    wait_for_state_running(&hash_v2);

    let state = any_state_file_with(&hash_v2).expect("state file with v2 hash");
    let v1_running = state
        .get(&hash_v1)
        .is_some_and(|entry| matches!(entry.status, ServiceLifecycleStatus::Running));
    assert!(
        !v1_running,
        "stale V1 hash must not remain a Running entry after stop/start reconcile"
    );

    Command::new(sysg_bin)
        .arg("stop")
        .arg("--supervisor")
        .assert()
        .success();
}
