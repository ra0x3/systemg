#[path = "common/mod.rs"]
mod common;

#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::Command as StdCommand;
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use assert_cmd::Command;
#[cfg(target_os = "linux")]
use common::HomeEnvGuard;
#[cfg(target_os = "linux")]
use predicates::prelude::PredicateBooleanExt;
#[cfg(target_os = "linux")]
use systemg::{
    config::load_config,
    daemon::{Daemon, PidFile, ServiceLifecycleStatus, ServiceStateFile},
    logs::{get_service_log_path, resolve_log_path},
    state_store::{LOOSE_PROJECT_ID, StateStore},
};
#[cfg(target_os = "linux")]
use tempfile::tempdir;

#[cfg(target_os = "linux")]
/// Writes a log fixture after creating its project directory.
fn write_log(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("log parent")).expect("create log parent");
    fs::write(path, contents).expect("write log fixture");
}

#[cfg(target_os = "linux")]
#[test]
/// Streams persisted output when the recorded process has no readable descriptors.
fn logs_streams_when_pid_has_no_fds() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
services:
  arb_rs:
    command: "/bin/sleep 30"
"#,
    )
    .expect("write config");

    let stdout_path = resolve_log_path(LOOSE_PROJECT_ID, "arb_rs", "stdout");
    let stderr_path = resolve_log_path(LOOSE_PROJECT_ID, "arb_rs", "stderr");
    write_log(&stdout_path, "streamed stdout line\n");
    write_log(&stderr_path, "");

    let mut pid_file = PidFile::load(StateStore::loose()).expect("load pid file");
    pid_file.insert("arb_rs", 999_999).expect("insert pid");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.env("SYSTEMG_TAIL_MODE", "oneshot")
        .arg("logs")
        .arg("--config")
        .arg(&config_path)
        .arg("--service")
        .arg("arb_rs")
        .arg("--kind")
        .arg("stdout")
        .arg("--lines")
        .arg("1")
        .assert()
        .success()
        .stdout(predicates::str::contains("arb_rs"))
        .stdout(predicates::str::contains("streamed stdout line"));

    unsafe { env::remove_var("SYSTEMG_TAIL_MODE") };
}

#[cfg(target_os = "linux")]
#[test]
/// Discards service output when the configured log sink is disabled.
fn logs_sink_none_discards_service_output_without_files() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
logs:
  sink: none
services:
  quiet:
    command: "sh -c 'echo stdout-line; echo stderr-line >&2; sleep 30'"
"#,
    )
    .expect("write config");

    let config =
        load_config(Some(config_path.to_string_lossy().as_ref())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("create daemon");
    let service = config.services.get("quiet").expect("quiet service");

    daemon
        .start_service("quiet", service)
        .expect("start quiet service");
    thread::sleep(Duration::from_millis(300));

    let stdout_path = resolve_log_path(LOOSE_PROJECT_ID, "quiet", "stdout");
    let stderr_path = resolve_log_path(LOOSE_PROJECT_ID, "quiet", "stderr");
    assert!(
        !stdout_path.exists(),
        "stdout log should not be created when logs.sink is none"
    );
    assert!(
        !stderr_path.exists(),
        "stderr log should not be created when logs.sink is none"
    );

    daemon.stop_service("quiet").expect("stop quiet service");
}

#[cfg(target_os = "linux")]
#[test]
/// Uses lifecycle state when a live service is absent from the PID file.
fn logs_uses_snapshot_runtime_when_pid_file_is_missing_service() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
services:
  demo:
    command: "/bin/sleep 30"
"#,
    )
    .expect("write config");

    let config =
        load_config(Some(config_path.to_string_lossy().as_ref())).expect("load config");
    let hash = config.state_key("demo");

    let mut child = StdCommand::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");

    let store = StateStore::for_project(&config.project.id);
    let mut state = ServiceStateFile::load(store.clone()).expect("load state");
    state
        .set(
            &hash,
            ServiceLifecycleStatus::Running,
            Some(child.id()),
            None,
            None,
        )
        .expect("persist state");

    let mut pid_file = PidFile::load(store).expect("load pid file");
    let _ = pid_file.remove("demo");

    let stdout_path = resolve_log_path(LOOSE_PROJECT_ID, "demo", "stdout");
    write_log(&stdout_path, "snapshot log line\n");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.env("SYSTEMG_TAIL_MODE", "oneshot")
        .arg("logs")
        .arg("--config")
        .arg(&config_path)
        .arg("--service")
        .arg("demo")
        .arg("--kind")
        .arg("stdout")
        .arg("--lines")
        .arg("1")
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "demo [pid {}]",
            child.id()
        )))
        .stdout(predicates::str::contains("snapshot log line"))
        .stdout(predicates::str::contains("offline").not());

    let _ = child.kill();
    let _ = child.wait();
    unsafe { env::remove_var("SYSTEMG_TAIL_MODE") };
}

#[cfg(target_os = "linux")]
#[test]
/// Returns only the requested number of trailing service log lines.
fn logs_lines_returns_last_lines_for_service() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
services:
  demo:
    command: "/bin/sleep 30"
"#,
    )
    .expect("write config");

    let config =
        load_config(Some(config_path.to_string_lossy().as_ref())).expect("load config");
    let hash = config.state_key("demo");

    let mut child = StdCommand::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");

    let mut state = ServiceStateFile::load(StateStore::for_project(&config.project.id))
        .expect("load state");
    state
        .set(
            &hash,
            ServiceLifecycleStatus::Running,
            Some(child.id()),
            None,
            None,
        )
        .expect("persist state");

    let stdout_path = resolve_log_path(LOOSE_PROJECT_ID, "demo", "stdout");
    write_log(
        &stdout_path,
        "first log line\nsecond log line\nthird log line\nfourth log line\n",
    );

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    let output = cmd
        .env("SYSTEMG_TAIL_MODE", "oneshot")
        .arg("logs")
        .arg("--config")
        .arg(&config_path)
        .arg("--service")
        .arg("demo")
        .arg("--kind")
        .arg("stdout")
        .arg("--lines")
        .arg("2")
        .output()
        .expect("run sysg logs");

    assert!(
        output.status.success(),
        "sysg logs should succeed, stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("first log line"));
    assert!(!stdout.contains("second log line"));
    assert!(stdout.contains("third log line"));
    assert!(stdout.contains("fourth log line"));

    let _ = child.kill();
    let _ = child.wait();
    unsafe { env::remove_var("SYSTEMG_TAIL_MODE") };
}

#[cfg(target_os = "linux")]
#[test]
/// Preserves capture order when reading the combined stdout and stderr log.
fn logs_default_reads_combined_stdout_stderr_in_capture_order() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
services:
  demo:
    command: "/bin/sleep 30"
"#,
    )
    .expect("write config");

    let config =
        load_config(Some(config_path.to_string_lossy().as_ref())).expect("load config");
    let hash = config.state_key("demo");

    let mut child = StdCommand::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");

    let mut state = ServiceStateFile::load(StateStore::for_project(&config.project.id))
        .expect("load state");
    state
        .set(
            &hash,
            ServiceLifecycleStatus::Running,
            Some(child.id()),
            None,
            None,
        )
        .expect("persist state");

    let combined_path = get_service_log_path(LOOSE_PROJECT_ID, "demo");
    let stdout_path = resolve_log_path(LOOSE_PROJECT_ID, "demo", "stdout");
    let stderr_path = resolve_log_path(LOOSE_PROJECT_ID, "demo", "stderr");
    write_log(
        &combined_path,
        "2026-05-14T02:00:00.000000Z stdout first\n\
2026-05-14T02:00:00.000001Z stderr second\n\
2026-05-14T02:00:00.000002Z stdout third\n",
    );
    write_log(&stdout_path, "stdout-only\n");
    write_log(&stderr_path, "stderr-only\n");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    let output = cmd
        .env("SYSTEMG_TAIL_MODE", "oneshot")
        .arg("logs")
        .arg("--config")
        .arg(&config_path)
        .arg("--service")
        .arg("demo")
        .arg("--lines")
        .arg("3")
        .output()
        .expect("run sysg logs");

    assert!(
        output.status.success(),
        "sysg logs should succeed, stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_idx = stdout.find("stdout first").expect("first combined line");
    let second_idx = stdout.find("stderr second").expect("second combined line");
    let third_idx = stdout.find("stdout third").expect("third combined line");

    assert!(first_idx < second_idx);
    assert!(second_idx < third_idx);
    assert!(!stdout.contains("stdout-only"));
    assert!(!stdout.contains("stderr-only"));

    let _ = child.kill();
    let _ = child.wait();
    unsafe { env::remove_var("SYSTEMG_TAIL_MODE") };
}

#[cfg(target_os = "linux")]
#[test]
/// Orders running services before offline services without process-group state.
fn logs_without_service_groups_running_before_offline() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("systemg.yaml");
    fs::write(
        &config_path,
        r#"
version: "2"
projects:
  logs-fixture:
    name: Logs Fixture
    services:
      beta:
        command: "/bin/sleep 30"
      alpha:
        command: "/bin/echo alpha"
"#,
    )
    .expect("write config");

    let config =
        load_config(Some(config_path.to_string_lossy().as_ref())).expect("load config");
    let beta_hash = config.state_key("beta");
    let project_id = config.project.id.clone();

    let mut child = StdCommand::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");

    let mut state = ServiceStateFile::load(StateStore::for_project(&config.project.id))
        .expect("load state");
    state
        .set(
            &beta_hash,
            ServiceLifecycleStatus::Running,
            Some(child.id()),
            None,
            None,
        )
        .expect("persist state");

    let beta_log = resolve_log_path(&project_id, "beta", "stdout");
    let alpha_log = resolve_log_path(&project_id, "alpha", "stdout");
    write_log(&beta_log, "beta line\n");
    write_log(&alpha_log, "alpha line\n");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    let output = cmd
        .env("SYSTEMG_TAIL_MODE", "oneshot")
        .arg("logs")
        .arg("--config")
        .arg(&config_path)
        .arg("--project")
        .arg(&project_id)
        .arg("--kind")
        .arg("stdout")
        .arg("--lines")
        .arg("1")
        .output()
        .expect("run sysg logs");

    assert!(
        output.status.success(),
        "sysg logs should succeed, stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let running_idx = stdout.find("Running Services").expect("running section");
    let offline_idx = stdout.find("Offline Services").expect("offline section");
    let beta_idx = stdout.find("beta [pid ").expect("running service heading");
    let alpha_idx = stdout
        .find("alpha [offline]")
        .expect("offline service heading");

    assert!(
        running_idx < offline_idx,
        "running section should come first"
    );
    assert!(
        running_idx < beta_idx,
        "running service should appear in running section"
    );
    assert!(
        offline_idx < alpha_idx,
        "offline service should appear in offline section"
    );
    assert!(
        beta_idx < alpha_idx,
        "running service output should appear before offline service output"
    );
    assert!(stdout.contains("beta line"));
    assert!(stdout.contains("alpha line"));

    let _ = child.kill();
    let _ = child.wait();
    unsafe { env::remove_var("SYSTEMG_TAIL_MODE") };
}

#[cfg(target_os = "linux")]
/// Reads a fixture log as UTF-8 text.
fn read_log(path: &Path) -> String {
    fs::read_to_string(path).expect("read log file")
}

#[cfg(target_os = "linux")]
#[test]
/// Purges only the selected service's log streams.
fn logs_purge_truncates_only_selected_service() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let api_stdout = resolve_log_path(LOOSE_PROJECT_ID, "api", "stdout");
    let api_stderr = resolve_log_path(LOOSE_PROJECT_ID, "api", "stderr");
    let worker_stdout = resolve_log_path(LOOSE_PROJECT_ID, "worker", "stdout");
    let worker_stderr = resolve_log_path(LOOSE_PROJECT_ID, "worker", "stderr");

    write_log(&api_stdout, "api stdout\n");
    write_log(&api_stderr, "api stderr\n");
    write_log(&worker_stdout, "worker stdout\n");
    write_log(&worker_stderr, "worker stderr\n");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.arg("logs")
        .arg("--service")
        .arg("api")
        .arg("--project")
        .arg(LOOSE_PROJECT_ID)
        .arg("--purge")
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    assert_eq!(read_log(&api_stdout), "");
    assert_eq!(read_log(&api_stderr), "");
    assert_eq!(read_log(&worker_stdout), "worker stdout\n");
    assert_eq!(read_log(&worker_stderr), "worker stderr\n");
}

#[cfg(target_os = "linux")]
#[test]
/// Purges every managed log when no service selector is present.
fn logs_purge_without_service_truncates_all_logs() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let log_dir = home.join(".local/share/systemg/logs");
    fs::create_dir_all(&log_dir).expect("make log dir");

    let api_stdout = log_dir.join("alpha/api_stdout.log");
    let api_stderr = log_dir.join("beta/api_stderr.log");
    let supervisor = log_dir.join("supervisor.log");
    let spawn_log = log_dir.join("spawn/child_stdout.log");

    write_log(&api_stdout, "api stdout\n");
    write_log(&api_stderr, "api stderr\n");
    write_log(&supervisor, "supervisor event\n");
    write_log(&spawn_log, "spawn output\n");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.arg("logs")
        .arg("--purge")
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    assert_eq!(read_log(&api_stdout), "");
    assert_eq!(read_log(&api_stderr), "");
    assert_eq!(read_log(&supervisor), "");
    assert_eq!(read_log(&spawn_log), "spawn output\n");
}
