#[path = "common/mod.rs"]
mod common;

use assert_cmd::{Command, cargo::cargo_bin_cmd};
use common::{HomeEnvGuard, is_process_alive, wait_for_pid, wait_for_pid_removed};
use std::{fs, thread, time::Duration};
use systemg::{
    config::load_config,
    daemon::{Daemon, PidFile, ServiceLifecycleStatus, ServiceStateFile},
};
use tempfile::tempdir;

/// Regression test for critical bug #1: Individual service stop via CLI must actually kill the process
/// This tests the exact scenario reported: sysg stop -s <service> reports stopped but process still runs
// #[test] // Disabled - Covered by individual_service_start_stop in service_control.rs
#[allow(dead_code)]
fn individual_service_stop_kills_actual_process() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    // Use a unique command so we can verify it's actually killed
    let config_yaml = r#"
version: '1'
services:
  arb_py__snuper__events_monitor:
    command: "sh -c 'while true; do sleep 1; done'"
  other_service:
    command: "sleep 60"
"#;
    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    // Start supervisor with services
    let sysg_bin = assert_cmd::cargo::cargo_bin!("sysg");
    Command::new(sysg_bin)
        .arg("start")
        .arg("--config")
        .arg(&config_path)
        .arg("--daemonize")
        .assert()
        .success();

    thread::sleep(Duration::from_millis(500));

    // Get the actual process PID
    let pid = wait_for_pid("arb_py__snuper__events_monitor");
    assert!(
        is_process_alive(pid),
        "Service process should be running initially"
    );

    // Stop the individual service (mimicking: sysg stop -s arb_py__snuper__events_monitor)
    Command::new(sysg_bin)
        .arg("stop")
        .arg("--config")
        .arg(&config_path)
        .arg("--service")
        .arg("arb_py__snuper__events_monitor")
        .assert()
        .success();

    // Wait for PID to be removed from tracking
    wait_for_pid_removed("arb_py__snuper__events_monitor");

    // CRITICAL CHECK: The actual process must be dead, not just removed from tracking
    thread::sleep(Duration::from_millis(200));
    assert!(
        !is_process_alive(pid),
        "CRITICAL BUG: Process {} still alive after stop command! This is the exact bug reported.",
        pid
    );

    // Verify other_service is still running (single service stop shouldn't affect others)
    let other_pid = PidFile::load().unwrap().pid_for("other_service");
    assert!(
        other_pid.is_some(),
        "other_service should still have PID entry"
    );
    assert!(
        is_process_alive(other_pid.unwrap()),
        "other_service should still be running"
    );

    // Cleanup
    Command::new(sysg_bin)
        .arg("stop")
        .arg("--config")
        .arg(&config_path)
        .assert()
        .success();
}

/// Regression test for critical bug #2: Individual service start via CLI when supervisor is running
/// This tests: sysg start -s <service> when supervisor is already running
// #[test] // Disabled - Covered by individual_service_start_stop in service_control.rs
#[allow(dead_code)]
fn individual_service_start_when_supervisor_running() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let marker = dir.join("service_started.txt");
    let config_yaml = format!(
        r#"
version: '1'
services:
  test_service:
    command: "sh -c 'echo started > {} && sleep 60'"
    skip: true  # Start with service skipped
  other_service:
    command: "sleep 60"
"#,
        marker.display()
    );
    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    // Start supervisor (test_service will be skipped)
    let sysg_bin = assert_cmd::cargo::cargo_bin!("sysg");
    Command::new(sysg_bin)
        .arg("start")
        .arg("--config")
        .arg(&config_path)
        .arg("--daemonize")
        .assert()
        .success();

    thread::sleep(Duration::from_millis(500));

    // Verify test_service is not running (skipped)
    assert!(
        !marker.exists(),
        "test_service should not have started (skip: true)"
    );

    // Update config to remove skip flag
    let config_yaml = format!(
        r#"
version: '1'
services:
  test_service:
    command: "sh -c 'echo started > {} && sleep 60'"
    skip: false  # Now allow it to start
  other_service:
    command: "sleep 60"
"#,
        marker.display()
    );
    fs::write(&config_path, config_yaml).unwrap();

    // Now start the individual service while supervisor is running
    Command::new(sysg_bin)
        .arg("start")
        .arg("--config")
        .arg(&config_path)
        .arg("--service")
        .arg("test_service")
        .assert()
        .success();

    thread::sleep(Duration::from_millis(500));

    // Service should now be running
    assert!(
        marker.exists(),
        "test_service should have started after individual start command"
    );
    let pid = wait_for_pid("test_service");
    assert!(
        is_process_alive(pid),
        "test_service process should be running"
    );

    // Cleanup
    Command::new(sysg_bin)
        .arg("stop")
        .arg("--config")
        .arg(&config_path)
        .assert()
        .success();
}

/// Regression test for critical bug #3: Status must accurately reflect actual process state
/// This tests that sysg status correctly shows when a process is actually dead vs alive
// #[test] // Disabled - Functionality covered by other tests
#[allow(dead_code)]
fn status_reflects_actual_process_state() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let config_yaml = r#"
version: '1'
services:
  test_service:
    command: "sleep 60"
"#;
    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon = Daemon::from_config(config, false).unwrap();

    // Start service
    daemon.start_services_nonblocking().unwrap();
    let pid = wait_for_pid("test_service");
    assert!(is_process_alive(pid), "Process should be alive initially");

    // Manually kill the process (simulating unexpected termination)
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    thread::sleep(Duration::from_millis(200));

    // Process should be dead
    assert!(
        !is_process_alive(pid),
        "Process should be dead after SIGKILL"
    );

    // Status should reflect that the process is dead, not "running"
    // The PidFile might still have the entry, but the process is dead
    let pid_file = PidFile::load().unwrap();
    if let Some(recorded_pid) = pid_file.pid_for("test_service") {
        assert_eq!(recorded_pid, pid, "PID should match");
        // The critical check: even if PID is in file, process is actually dead
        assert!(
            !is_process_alive(recorded_pid),
            "Status should detect process {} is dead, not report as running",
            recorded_pid
        );
    }

    daemon.shutdown_monitor();
}

/// Test the exact scenario from the bug report: stopping a service individually leaves processes running
// #[test] // Disabled - Covered by restart_kills_detached_descendants in process.rs
#[allow(dead_code)]
fn stop_service_terminates_all_child_processes() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    // Simulate a Python service that spawns child processes (like snuper)
    let config_yaml = r#"
version: '1'
services:
  python_service:
    command: "sh -c 'python -c \"import time; time.sleep(60)\" & python -c \"import time; time.sleep(60)\" & sleep 60'"
"#;
    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon = Daemon::from_config(config.clone(), false).unwrap();

    daemon.start_services_nonblocking().unwrap();
    thread::sleep(Duration::from_millis(500));

    let main_pid = wait_for_pid("python_service");

    // Note: In real scenario, we'd verify child processes are spawned
    // For testing, we just verify the main process is properly terminated

    // Stop the service
    daemon.stop_service("python_service").unwrap();
    thread::sleep(Duration::from_millis(500));

    // Main process should be dead
    assert!(!is_process_alive(main_pid), "Main process should be dead");

    // In the actual bug, child processes were left running
    // This test verifies the main process is properly killed

    daemon.shutdown_monitor();
}

#[test]
fn purge_retains_configured_services_in_status() {
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
    let mut state_file = ServiceStateFile::load().unwrap_or_default();
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

    let mut pid_file = PidFile::load().unwrap_or_default();
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
        "status command should exit healthy or degraded before purge, got {before_code}"
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
        "status command should exit healthy or degraded after purge, got {after_code}"
    );
    let stdout_after = String::from_utf8_lossy(&status_after.stdout);
    assert!(
        stdout_after.contains("sample"),
        "Expected configured service to remain visible after purge"
    );
    assert!(
        stdout_after.contains("Not running"),
        "Service should show as Not running after purge"
    );
}
