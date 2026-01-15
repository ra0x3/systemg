#[path = "common/mod.rs"]
mod common;

use std::fs;

use common::{HomeEnvGuard, wait_for_lines};
use systemg::{config::load_config, daemon::Daemon};
use tempfile::tempdir;

#[test]
fn hooks_on_start_and_stop_success() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let hook_log = dir.join("hooks.log");

    let config_yaml = format!(
        r#"
version: '1'
env:
  vars:
    VARS_TOKEN: "file_only"
services:
  demo:
    command: "sleep 0.1"
    hooks:
      on_start:
        success:
          command: "echo 'START:'$VARS_TOKEN':'$ENV_VARS_TOKEN >> {}"
      on_stop:
        success:
          command: "echo 'STOP:'$VARS_TOKEN':'$ENV_VARS_TOKEN >> {}"
    env:
      vars:
        ENV_VARS_TOKEN: "service_only"
"#,
        hook_log.display(),
        hook_log.display()
    );
    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config, false).expect("create daemon");

    daemon.start_services_nonblocking().expect("start services");

    let lines = wait_for_lines(&hook_log, 1);
    assert_eq!(lines, vec!["START:file_only:service_only".to_string()]);

    daemon.stop_service("demo").expect("stop service");

    let lines = wait_for_lines(&hook_log, 2);
    assert_eq!(
        lines,
        vec![
            "START:file_only:service_only".to_string(),
            "STOP:file_only:service_only".to_string(),
        ]
    );

    daemon.shutdown_monitor();
}

#[test]
fn hooks_restart_flow() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let state_file = dir.join("state.txt");
    let hook_log = dir.join("hooks.log");

    let config_yaml = format!(
        r#"
version: '1'
services:
  test:
    command: "sh -c 'sleep 0.1; if [ -f {} ]; then exit 0; else exit 1; fi'"
    restart_policy: "never"
    hooks:
      on_start:
        success:
          command: "echo 'on_start.success' >> {}"
      on_stop:
        success:
          command: "echo 'on_stop.success' >> {}"
        error:
          command: "echo 'on_stop.error' >> {}"
      on_restart:
        success:
          command: "echo 'on_restart.success' >> {}"
"#,
        state_file.display(),
        hook_log.display(),
        hook_log.display(),
        hook_log.display(),
        hook_log.display(),
    );

    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("create daemon");

    // Start service (will fail since state.txt doesn't exist)
    daemon.start_services_nonblocking().expect("start services");
    std::thread::sleep(std::time::Duration::from_millis(200));

    let lines = wait_for_lines(&hook_log, 2);
    assert_eq!(
        lines,
        vec!["on_start.success".to_string(), "on_stop.error".to_string(),]
    );

    // Create state file and restart
    fs::write(&state_file, "exists").expect("write state");
    daemon
        .restart_service("test", &config.services["test"])
        .expect("restart service");
    std::thread::sleep(std::time::Duration::from_millis(200));

    let lines = wait_for_lines(&hook_log, 4);
    assert_eq!(
        lines,
        vec![
            "on_start.success".to_string(),
            "on_stop.error".to_string(),
            "on_stop.success".to_string(), // restart stops the (already stopped) service
            "on_start.success".to_string(), // then starts it again
        ]
    );

    daemon.stop_service("test").expect("stop service");
    let lines = wait_for_lines(&hook_log, 5);
    assert_eq!(
        lines,
        vec![
            "on_start.success".to_string(),
            "on_stop.error".to_string(),
            "on_stop.success".to_string(), // from restart stopping the service
            "on_start.success".to_string(), // from restart starting it again
            "on_stop.success".to_string(), // from manual stop
        ]
    );

    daemon.shutdown_monitor();
}
