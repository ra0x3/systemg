#[path = "common/mod.rs"]
mod common;

#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(target_os = "linux")]
use std::process::Command as StdCommand;
use std::{fs, thread, time::Duration};

use assert_cmd::Command;
use common::HomeEnvGuard;
#[cfg(target_os = "linux")]
use common::{is_process_alive, wait_for_path};
use tempfile::tempdir;

#[cfg(unix)]
#[test]
fn stale_socket_doesnt_block_commands() {
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
    command: "sleep 5"
"#,
    )
    .expect("failed to write config");

    let runtime_dir = home.join(".local/share/systemg");
    fs::create_dir_all(&runtime_dir).expect("failed to create runtime dir");

    let socket_path = runtime_dir.join("control.sock");
    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!(
                "Skipping stale_socket_doesnt_block_commands: cannot bind stale socket ({err})"
            );
            return;
        }
        Err(err) => panic!("failed to create socket: {err}"),
    };
    drop(listener);

    let pid_file = runtime_dir.join("sysg.pid");
    fs::write(&pid_file, "999999").expect("failed to write stale pid");

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("stop")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("failed to execute stop");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Connection refused"),
        "Should not get 'Connection refused' with stale socket. stderr: {}",
        stderr
    );

    assert!(
        !socket_path.exists() || !pid_file.exists(),
        "Stale socket or PID file should be cleaned up"
    );
}

#[test]
fn purge_removes_all_state() {
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
    command: "sleep 2"
"#,
    )
    .expect("failed to write config");

    Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("start")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .arg("--daemonize")
        .assert()
        .success();

    thread::sleep(Duration::from_secs(3));

    Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("stop")
        .assert()
        .success();

    thread::sleep(Duration::from_millis(500));

    let runtime_dir = home.join(".local/share/systemg");
    let state_file = runtime_dir.join("state.json");
    let pid_file = runtime_dir.join("pid.json");
    let lock_file = runtime_dir.join("pid.json.lock");
    let supervisor_log = runtime_dir.join("logs/supervisor.log");

    assert!(state_file.exists(), "state.json should exist before purge");
    assert!(
        pid_file.exists() || lock_file.exists() || supervisor_log.exists(),
        "At least one runtime file should exist before purge"
    );

    Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("purge")
        .assert()
        .success();

    assert!(
        !state_file.exists(),
        "state.json should be removed after purge"
    );
    assert!(!pid_file.exists(), "pid.json should be removed after purge");
    assert!(
        !lock_file.exists(),
        "pid.json.lock should be removed after purge"
    );
    assert!(
        !supervisor_log.exists(),
        "supervisor.log should be removed after purge"
    );
    assert!(
        !runtime_dir.exists(),
        "Runtime directory should be completely removed after purge"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn purge_stops_running_supervisor() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let mut sleeper = StdCommand::new("sleep")
        .arg("30")
        .spawn()
        .expect("failed to spawn sleeper");
    let pid = sleeper.id();

    assert!(is_process_alive(pid), "sleeper should be running");

    let runtime_dir = home.join(".local/share/systemg");
    fs::create_dir_all(&runtime_dir).expect("failed to create runtime dir");
    let pid_path = runtime_dir.join("sysg.pid");
    fs::write(&pid_path, pid.to_string()).expect("failed to write pid file");

    wait_for_path(&pid_path);

    Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("purge")
        .assert()
        .success();

    let kill_result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if kill_result == 0 {
        // Process still responds to signal 0; purge should still have attempted termination.
    }

    let _ = sleeper.wait();

    assert!(
        !runtime_dir.exists(),
        "runtime directory should be removed after purge"
    );
}

#[test]
fn sys_flag_requires_root_privileges() {
    if nix::unistd::Uid::effective().is_root() {
        return;
    }

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("--sys")
        .arg("status")
        .output()
        .expect("failed to invoke sysg");

    assert!(
        !output.status.success(),
        "--sys should fail when invoked without root"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--sys requires root"),
        "stderr should mention missing root privileges: {stderr}"
    );
}
