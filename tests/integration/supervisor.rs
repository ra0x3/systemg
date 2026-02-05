#[path = "common/mod.rs"]
mod common;

use std::{fs, thread, time::Duration};

use assert_cmd::Command;
use common::HomeEnvGuard;
use tempfile::tempdir;

#[test]
fn supervisor_logs_operational_events() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("systemg.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  test_service:
    command: "sleep 30"
"#,
    )
    .expect("failed to write config");

    let supervisor_log = home.join(".local/share/systemg/logs/supervisor.log");

    let mut start_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    start_cmd
        .arg("start")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .arg("--daemonize")
        .arg("--log-level")
        .arg("debug")
        .assert()
        .success();

    thread::sleep(Duration::from_secs(2));

    let mut status_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    status_cmd
        .arg("status")
        .arg("--service")
        .arg("test_service")
        .assert()
        .success();

    let mut restart_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    restart_cmd
        .arg("restart")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .arg("--service")
        .arg("test_service")
        .assert()
        .success();

    thread::sleep(Duration::from_secs(1));

    let mut stop_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    stop_cmd.arg("stop").assert().success();

    thread::sleep(Duration::from_millis(500));

    assert!(
        supervisor_log.exists(),
        "Supervisor log file should exist at {:?}",
        supervisor_log
    );

    let log_contents = fs::read_to_string(&supervisor_log)
        .expect("Should be able to read supervisor log");

    assert!(
        !log_contents.is_empty(),
        "Supervisor log should not be empty"
    );

    assert!(
        log_contents.contains("Starting service: test_service")
            || log_contents.contains("Starting service"),
        "Log should contain 'Starting service' message. Log contents:\n{}",
        log_contents
    );

    assert!(
        log_contents.contains("Performing immediate restart for service: test_service")
            || log_contents.contains("restart"),
        "Log should contain restart information. Log contents:\n{}",
        log_contents
    );

    assert!(
        log_contents.contains("Stopping service")
            || log_contents.contains("Supervisor shutting down")
            || log_contents.contains("stop")
            || log_contents.contains("was terminated with ExitStatus"),
        "Log should contain stop/shutdown messages. Log contents:\n{}",
        log_contents
    );

    assert!(
        log_contents.contains("test_service"),
        "Log should mention the service name 'test_service'. Log contents:\n{}",
        log_contents
    );

    assert!(
        log_contents.contains("DEBUG") || log_contents.len() > 200,
        "Log should contain debug-level information. Log length: {}, contents:\n{}",
        log_contents.len(),
        log_contents
    );
}
