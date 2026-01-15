#[path = "common/mod.rs"]
mod common;

use std::{fs, thread, time::Duration};

use common::{HomeEnvGuard, wait_for_file_value, wait_for_pid, wait_for_pid_removed};
use systemg::{config::load_config, daemon::Daemon};
use tempfile::tempdir;

#[test]
fn individual_service_start_stop() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let marker = dir.join("marker.txt");

    let config_yaml = format!(
        r#"
version: '1'
services:
  test_service:
    command: "sh -c 'echo running > {} && sleep 2'"
  other_service:
    command: "sleep 10"
"#,
        marker.display()
    );
    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon = Daemon::from_config(config.clone(), false).unwrap();

    // Start all services
    daemon.start_services_nonblocking().unwrap();

    thread::sleep(Duration::from_millis(500));

    // Both services should be running
    wait_for_pid("test_service");
    wait_for_pid("other_service");
    wait_for_file_value(&marker, "running");

    // Stop only test_service
    daemon.stop_service("test_service").unwrap();
    wait_for_pid_removed("test_service");

    // other_service should still be running
    assert!(
        systemg::daemon::PidFile::load()
            .unwrap()
            .pid_for("other_service")
            .is_some()
    );

    // Start test_service again
    let test_service_config = &config.services["test_service"];
    daemon
        .start_service("test_service", test_service_config)
        .unwrap();

    thread::sleep(Duration::from_millis(500));

    // test_service should be running again
    wait_for_pid("test_service");

    // Clean up
    daemon.shutdown_monitor();
    daemon.stop_services().unwrap();
}

#[test]
fn restart_records_new_pid() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let config_yaml = r#"
version: '1'
services:
  sleepy:
    command: "sleep 60"
"#;
    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon = Daemon::from_config(config.clone(), false).unwrap();

    daemon.start_services_nonblocking().unwrap();

    let pid1 = wait_for_pid("sleepy");
    assert!(common::is_process_alive(pid1));

    daemon
        .restart_service("sleepy", &config.services["sleepy"])
        .unwrap();

    let pid2 = wait_for_pid("sleepy");
    assert_ne!(pid1, pid2, "Should have new PID after restart");
    assert!(
        !common::is_process_alive(pid1),
        "Old process should be gone"
    );
    assert!(
        common::is_process_alive(pid2),
        "New process should be running"
    );

    daemon.shutdown_monitor();
    daemon.stop_services().unwrap();
}

#[test]
fn manual_stop_suppresses_pending_restart() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let counter = dir.join("counter.txt");
    let config_yaml = format!(
        r#"
version: '1'
services:
  flaky:
    command: "sh -c 'echo $$ >> {}; sleep 0.5; exit 1'"
    restart_policy: "always"
    backoff: "1s"
"#,
        counter.display()
    );

    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon = Daemon::from_config(config, false).unwrap();

    // Start services - the flaky service should start running briefly before failing
    let result = daemon.start_services_nonblocking();
    println!("start_services_nonblocking result: {:?}", result);
    result.unwrap();

    thread::sleep(Duration::from_secs(4));

    // Service should have restarted at least once
    let content = fs::read_to_string(&counter).unwrap_or_else(|_| String::from(""));
    let initial_runs = content.lines().count();
    println!("Initial runs after 4s: {}", initial_runs);
    assert!(
        initial_runs >= 2,
        "Service should have restarted, but only ran {} times",
        initial_runs
    );

    // Manual stop
    daemon.stop_service("flaky").unwrap();

    thread::sleep(Duration::from_millis(500));

    // Count should not have increased
    let content = fs::read_to_string(&counter).unwrap();
    let final_runs = content.lines().count();
    assert_eq!(
        initial_runs, final_runs,
        "Service should not restart after manual stop"
    );

    daemon.shutdown_monitor();
}

#[test]
fn skip_flag_prevents_service_start() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let marker = dir.join("marker.txt");
    let config_yaml = format!(
        r#"
version: '1'
services:
  skipped:
    command: "touch {}"
    skip: true
"#,
        marker.display()
    );

    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon = Daemon::from_config(config, false).unwrap();

    daemon.start_services_nonblocking().unwrap();
    thread::sleep(Duration::from_millis(200));

    assert!(!marker.exists(), "Skipped service should not execute");

    daemon.shutdown_monitor();
}

#[test]
fn skip_flag_controls_service_execution() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    let marker = dir.join("marker.txt");

    // Test 1: skip: false - service should execute
    let config_yaml_false = format!(
        r#"
version: '1'
services:
  test:
    command: "sh -c 'echo \"service executed\" > {}'"
    skip: false
"#,
        marker.display()
    );

    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml_false).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon = Daemon::from_config(config.clone(), false).unwrap();

    // Start service directly (mimicking what Supervisor does)
    for (service_name, service_config) in &config.services {
        if service_config.cron.is_none() {
            daemon.start_service(service_name, service_config).unwrap();
        }
    }

    wait_for_file_value(&marker, "service executed");
    daemon.stop_services().unwrap();

    // Clean up marker
    let _ = fs::remove_file(&marker);

    // Test 2: skip: true - service should NOT execute
    let config_yaml_true = format!(
        r#"
version: '1'
services:
  test:
    command: "sh -c 'echo \"service executed\" > {}'"
    skip: true
"#,
        marker.display()
    );

    fs::write(&config_path, config_yaml_true).unwrap();

    let config2 = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon2 = Daemon::from_config(config2.clone(), false).unwrap();

    // Start service directly (mimicking what Supervisor does)
    for (service_name, service_config) in &config2.services {
        if service_config.cron.is_none() {
            daemon2.start_service(service_name, service_config).unwrap();
        }
    }

    thread::sleep(Duration::from_millis(500));
    assert!(
        !marker.exists(),
        "skip: true should prevent service from executing, but marker file exists"
    );

    daemon2.shutdown_monitor();
}
