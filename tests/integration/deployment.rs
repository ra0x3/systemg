#[path = "common/mod.rs"]
mod common;

use common::{HomeEnvGuard, wait_for_path};
use std::fs;
use std::sync::{Arc, Mutex};
use systemg::config::load_config;
use systemg::daemon::{Daemon, PidFile, ServiceLifecycleStatus, ServiceStateFile};
use tempfile::tempdir;

#[test]
fn pre_start_failure_records_error_state() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
project_dir: "."
services:
  failing_pre_start:
    command: "sh -c 'echo should_not_run'"
    restart_policy: "always"
    deployment:
      pre_start: "sh -c 'exit 42'"
"#,
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let failing_config = config
        .services
        .get("failing_pre_start")
        .cloned()
        .expect("service exists");

    let pid_file = Arc::new(Mutex::new(PidFile::load().expect("load pid file")));
    let state_file = Arc::new(Mutex::new(
        ServiceStateFile::load().expect("load service state file"),
    ));

    let daemon = Daemon::new(
        config,
        Arc::clone(&pid_file),
        Arc::clone(&state_file),
        false,
    );

    let result = daemon.start_service("failing_pre_start", &failing_config);
    assert!(result.is_err(), "pre-start failure should surface as error");

    {
        let service_hash = failing_config.compute_hash();
        let guard = state_file.lock().expect("lock state file");
        let entry = guard.get(&service_hash).expect("service entry recorded");
        assert_eq!(
            entry.status,
            ServiceLifecycleStatus::ExitedWithError,
            "failed pre-start should mark service as exited with error",
        );
        assert_eq!(
            entry.exit_code,
            Some(42),
            "exit code from pre-start should be persisted",
        );
        assert!(entry.pid.is_none(), "no pid should be recorded");
    }

    {
        let guard = pid_file.lock().expect("lock pid file");
        assert!(
            guard.pid_for("failing_pre_start").is_none(),
            "pid file should not contain service entry after failed pre-start",
        );
    }

    daemon.shutdown_monitor();
}

#[test]
fn pre_start_command_executes_on_service_startup() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let pre_start_marker = dir.join("pre_start_executed.txt");
    let service_marker = dir.join("service_executed.txt");

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  test_app:
    command: "sh -c 'if [ -f \"{pre_marker}\" ]; then echo success > \"{svc_marker}\"; sleep 2; else echo pre_start_failed > \"{svc_marker}\"; sleep 2; fi'"
    deployment:
      strategy: "immediate"
      pre_start: "echo pre_start_done > \"{pre_marker}\""
"#,
            pre_marker = pre_start_marker.display(),
            svc_marker = service_marker.display()
        ),
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("daemon from config");

    daemon
        .start_service("test_app", config.services.get("test_app").unwrap())
        .expect("start test_app");

    wait_for_path(&service_marker);

    assert!(
        pre_start_marker.exists(),
        "pre_start command should have created marker file before service started"
    );

    let service_output = fs::read_to_string(&service_marker)
        .expect("read service marker")
        .trim()
        .to_string();
    assert_eq!(
        service_output, "success",
        "Service should have seen the pre_start marker file"
    );

    daemon.stop_service("test_app").expect("stop test_app");
    daemon.shutdown_monitor();
}
