//! Integration tests covering PID/state tracking after supervisor restarts

#[path = "common/mod.rs"]
mod common;

use std::{
    fs,
    sync::{Arc, Mutex},
};

use assert_cmd::cargo::cargo_bin_cmd;
use common::{HomeEnvGuard, is_process_alive, wait_for_pid};
use systemg::{
    config::Config,
    daemon::{Daemon, PidFile, ServiceLifecycleStatus, ServiceStateFile},
    state_store::StateStore,
};
use tempfile::tempdir;

fn build_daemon(config: Config) -> Daemon {
    let store = StateStore::for_project(&config.project.id);
    let pid_file = Arc::new(Mutex::new(PidFile::load(store.clone()).unwrap_or_default()));
    let state_file = Arc::new(Mutex::new(
        ServiceStateFile::load(store).unwrap_or_default(),
    ));
    Daemon::new(config, pid_file, state_file, false)
}

#[test]
fn status_recovers_from_stale_exit_state() {
    let temp = tempdir().expect("failed to create temp dir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home_guard = HomeEnvGuard::set(&home);

    let config_path = home.join("sysg.yaml");
    let config_contents = r#"
version: "2"
services:
  steady:
    command: "sleep 30"
    restart_policy: always
    backoff: 1s
"#;
    fs::write(&config_path, config_contents).expect("write config yaml");

    let config = systemg::config::load_config(Some(config_path.to_str().unwrap()))
        .expect("load config");
    let daemon = build_daemon(config);
    daemon.start_services().expect("failed to start services");

    let pid = wait_for_pid("steady");
    assert!(is_process_alive(pid));

    // Force the state file to claim the service exited in error,
    // simulating the stale metadata the user encountered.
    let config_arc = daemon.config();
    let service_hash = config_arc.state_key("steady");
    let mut state_file = ServiceStateFile::load(daemon.store()).expect("load state file");
    state_file
        .set(
            &service_hash,
            ServiceLifecycleStatus::ExitedWithError,
            None,
            Some(1),
            None,
        )
        .expect("write stale state");

    let output = cargo_bin_cmd!("sysg")
        .arg("status")
        .arg("-c")
        .arg(config_path.to_str().unwrap())
        .arg("-s")
        .arg("steady")
        .env("HOME", &home)
        .output()
        .expect("run sysg status");

    assert!(
        output.status.success(),
        "sysg status command should succeed even with stale state"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Running"),
        "Expected status output to treat the alive process as running, got: {stdout}"
    );

    let refreshed_state =
        ServiceStateFile::load(daemon.store()).expect("reload state file");
    let entry = refreshed_state
        .get(&service_hash)
        .expect("state entry present after status call");
    assert_eq!(entry.status, ServiceLifecycleStatus::Running);
    assert_eq!(entry.pid, Some(pid));

    daemon.stop_services().ok();
    daemon.shutdown_monitor();
}
