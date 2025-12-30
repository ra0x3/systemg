//! Integration tests covering PID/state tracking after supervisor restarts

#[path = "common/mod.rs"]
mod common;

use assert_cmd::cargo::cargo_bin_cmd;
use common::{HomeEnvGuard, is_process_alive, wait_for_pid};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fs, thread};
use systemg::config::{Config, ServiceConfig};
use systemg::daemon::{Daemon, PidFile, ServiceLifecycleStatus, ServiceStateFile};
use tempfile::tempdir;

fn wait_for_pid_change(service: &str, previous: u32) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(pid_file) = PidFile::load()
            && let Some(pid) = pid_file.pid_for(service)
            && pid != previous
        {
            return pid;
        }

        if Instant::now() >= deadline {
            panic!("Timed out waiting for service '{service}' to record a new PID");
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn build_daemon(config: Config) -> Daemon {
    let pid_file = Arc::new(Mutex::new(PidFile::load().unwrap_or_default()));
    let state_file = Arc::new(Mutex::new(ServiceStateFile::load().unwrap_or_default()));
    Daemon::new(config, pid_file, state_file, false)
}

fn wait_for_state_status(
    service_hash: &str,
    expected: ServiceLifecycleStatus,
) -> ServiceStateFile {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state_file = ServiceStateFile::load().expect("load state file");
        if let Some(entry) = state_file.get(service_hash)
            && entry.status == expected
        {
            return state_file;
        }

        if Instant::now() >= deadline {
            panic!(
                "Timed out waiting for service hash {service_hash} to reach status {:?}",
                expected
            );
        }

        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn restart_updates_state_with_new_pid() {
    let temp = tempdir().expect("failed to create temp dir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home_guard = HomeEnvGuard::set(&home);

    let flag = temp.path().join("flaky.once");
    let flag_str = flag.to_string_lossy();
    let command = format!(
        "sh -c 'if [ ! -f \"{flag}\" ]; then touch \"{flag}\"; sleep 1; exit 1; else exec sleep 30; fi'",
        flag = flag_str
    );

    let mut services = HashMap::new();
    services.insert(
        "flaky".to_string(),
        ServiceConfig {
            command,
            env: None,
            restart_policy: Some("always".to_string()),
            backoff: Some("1s".to_string()),
            max_restarts: Some(3),
            depends_on: None,
            deployment: None,
            hooks: None,
            cron: None,
            skip: None,
        },
    );

    let config = Config {
        version: "1".to_string(),
        services,
        project_dir: Some(temp.path().to_string_lossy().to_string()),
        env: None,
    };

    let daemon = build_daemon(config);
    daemon
        .start_services_nonblocking()
        .expect("failed to start services");

    let initial_pid = wait_for_pid("flaky");
    let new_pid = wait_for_pid_change("flaky", initial_pid);

    let config_arc = daemon.config();
    let service_hash = config_arc
        .services
        .get("flaky")
        .expect("service present")
        .compute_hash();

    let state_file =
        wait_for_state_status(&service_hash, ServiceLifecycleStatus::Running);
    let entry = state_file
        .get(&service_hash)
        .expect("state entry present after restart");

    assert_eq!(
        entry.status,
        ServiceLifecycleStatus::Running,
        "Expected restart to mark service as running"
    );
    assert_eq!(
        entry.pid,
        Some(new_pid),
        "State file should record the new PID after restart"
    );
    assert!(
        is_process_alive(new_pid),
        "New PID {new_pid} reported by restart should still be alive"
    );

    daemon.stop_services().ok();
    daemon.shutdown_monitor();
}

#[test]
fn status_recovers_from_stale_exit_state() {
    let temp = tempdir().expect("failed to create temp dir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home_guard = HomeEnvGuard::set(&home);

    let config_path = home.join("sysg.yaml");
    let config_contents = r#"
version: "1"
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
    daemon
        .start_services_nonblocking()
        .expect("failed to start services");

    let pid = wait_for_pid("steady");
    assert!(is_process_alive(pid));

    // Force the state file to claim the service exited in error,
    // simulating the stale metadata the user encountered.
    let config_arc = daemon.config();
    let service_hash = config_arc
        .services
        .get("steady")
        .expect("service present")
        .compute_hash();
    let mut state_file = ServiceStateFile::load().expect("load state file");
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

    let refreshed_state = ServiceStateFile::load().expect("reload state file");
    let entry = refreshed_state
        .get(&service_hash)
        .expect("state entry present after status call");
    assert_eq!(entry.status, ServiceLifecycleStatus::Running);
    assert_eq!(entry.pid, Some(pid));

    daemon.stop_services().ok();
    daemon.shutdown_monitor();
}
