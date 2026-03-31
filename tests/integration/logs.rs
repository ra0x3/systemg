#[path = "common/mod.rs"]
mod common;

#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
use assert_cmd::Command;
#[cfg(target_os = "linux")]
use common::HomeEnvGuard;
#[cfg(target_os = "linux")]
use systemg::daemon::PidFile;
#[cfg(target_os = "linux")]
use tempfile::tempdir;

#[cfg(target_os = "linux")]
#[test]
fn logs_streams_when_pid_has_no_fds() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let log_dir = home.join(".local/share/systemg/logs");
    fs::create_dir_all(&log_dir).expect("make log dir");
    let stdout_path = log_dir.join("arb_rs_stdout.log");
    let stderr_path = log_dir.join("arb_rs_stderr.log");
    fs::write(&stdout_path, "streamed stdout line\n").expect("write stdout log");
    fs::write(&stderr_path, "").expect("write stderr log");

    let mut pid_file = PidFile::load().expect("load pid file");
    pid_file.insert("arb_rs", 999_999).expect("insert pid");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.env("SYSTEMG_TAIL_MODE", "oneshot")
        .arg("logs")
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
fn read_log(path: &Path) -> String {
    fs::read_to_string(path).expect("read log file")
}

#[cfg(target_os = "linux")]
#[test]
fn logs_clear_truncates_only_selected_service() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let log_dir = home.join(".local/share/systemg/logs");
    fs::create_dir_all(&log_dir).expect("make log dir");

    let api_stdout = log_dir.join("api_stdout.log");
    let api_stderr = log_dir.join("api_stderr.log");
    let worker_stdout = log_dir.join("worker_stdout.log");
    let worker_stderr = log_dir.join("worker_stderr.log");

    fs::write(&api_stdout, "api stdout\n").expect("write api stdout");
    fs::write(&api_stderr, "api stderr\n").expect("write api stderr");
    fs::write(&worker_stdout, "worker stdout\n").expect("write worker stdout");
    fs::write(&worker_stderr, "worker stderr\n").expect("write worker stderr");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.arg("logs")
        .arg("--service")
        .arg("api")
        .arg("--clear")
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
fn logs_clear_without_service_truncates_all_logs() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let log_dir = home.join(".local/share/systemg/logs");
    fs::create_dir_all(&log_dir).expect("make log dir");

    let api_stdout = log_dir.join("api_stdout.log");
    let api_stderr = log_dir.join("api_stderr.log");
    let supervisor = log_dir.join("supervisor.log");
    let spawn_log = log_dir.join("spawn/child_stdout.log");

    fs::write(&api_stdout, "api stdout\n").expect("write api stdout");
    fs::write(&api_stderr, "api stderr\n").expect("write api stderr");
    fs::write(&supervisor, "supervisor event\n").expect("write supervisor");
    fs::create_dir_all(spawn_log.parent().expect("spawn parent"))
        .expect("create spawn dir");
    fs::write(&spawn_log, "spawn output\n").expect("write spawn log");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.arg("logs")
        .arg("--clear")
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    assert_eq!(read_log(&api_stdout), "");
    assert_eq!(read_log(&api_stderr), "");
    assert_eq!(read_log(&supervisor), "");
    assert_eq!(read_log(&spawn_log), "spawn output\n");
}
