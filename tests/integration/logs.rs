#[path = "common/mod.rs"]
mod common;

#[cfg(target_os = "linux")]
use assert_cmd::Command;
#[cfg(target_os = "linux")]
use common::HomeEnvGuard;
#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::fs;
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
        .arg("arb_rs")
        .arg("--lines")
        .arg("1")
        .assert()
        .success()
        .stdout(predicates::str::contains("arb_rs"))
        .stdout(predicates::str::contains("streamed stdout line"));

    unsafe { env::remove_var("SYSTEMG_TAIL_MODE") };
}
