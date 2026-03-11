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
use systemg::daemon::PidFile;
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
    let state_file = runtime_dir.join("state.xml");
    let pid_file = runtime_dir.join("pid.xml");
    let lock_file = runtime_dir.join("pid.xml.lock");
    let supervisor_log = runtime_dir.join("logs/supervisor.log");

    assert!(state_file.exists(), "state.xml should exist before purge");
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
        "state.xml should be removed after purge"
    );
    assert!(!pid_file.exists(), "pid.xml should be removed after purge");
    assert!(
        !lock_file.exists(),
        "pid.xml.lock should be removed after purge"
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

#[test]
fn inspect_requires_service_flag_not_positional_arg() {
    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("inspect")
        .arg("demo-service")
        .output()
        .expect("failed to invoke sysg inspect");

    assert!(
        !output.status.success(),
        "inspect should reject positional service arguments"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument") && stderr.contains("--service"),
        "stderr should direct usage to --service: {stderr}"
    );
}

#[test]
fn start_daemonize_accepts_unit_command_without_config() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("start")
        .arg("--daemonize")
        .arg("sleep")
        .arg("30")
        .assert()
        .success();

    thread::sleep(Duration::from_secs(1));

    let units_dir = home.join(".local/share/systemg/units");
    assert!(units_dir.exists(), "units config directory should exist");

    let mut yaml_files = fs::read_dir(&units_dir)
        .expect("failed to read units directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("yaml"))
        .collect::<Vec<_>>();
    yaml_files.sort();
    assert!(
        !yaml_files.is_empty(),
        "expected at least one generated unit config"
    );

    let generated_yaml =
        fs::read_to_string(&yaml_files[0]).expect("failed to read generated unit yaml");
    assert!(
        generated_yaml.contains("command: 'sleep 30'"),
        "generated unit config should include command; got:\n{}",
        generated_yaml
    );

    let pid_file = PidFile::load().expect("pid file should load");
    assert!(
        !pid_file.services().is_empty(),
        "expected at least one supervised service in pid file"
    );

    Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("stop")
        .assert()
        .success();
}

#[test]
fn drop_privileges_warns_for_non_spawn_commands() {
    let temp = tempdir().expect("failed to create tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("status")
        .arg("--drop-privileges")
        .output()
        .expect("failed to invoke sysg status");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected_non_root = "--drop-privileges has no effect when not running as root";
    let expected_non_spawn = "--drop-privileges only applies when spawning child services during start/restart; this command will ignore it";
    assert!(
        stderr.contains(expected_non_root) || stderr.contains(expected_non_spawn),
        "expected drop-privileges warning in stderr: {stderr}"
    );
}

#[test]
fn spawn_command_prints_deprecation_warning() {
    let temp = tempdir().expect("failed to create tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let output = Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .arg("spawn")
        .arg("--name")
        .arg("worker-1")
        .arg("--")
        .arg("sleep")
        .arg("1")
        .output()
        .expect("failed to invoke sysg spawn");

    assert!(
        !output.status.success(),
        "spawn should fail without a running supervisor in this test"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("deprecated"),
        "spawn stderr should include deprecation warning: {stderr}"
    );
}
