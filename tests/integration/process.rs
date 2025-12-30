#[path = "common/mod.rs"]
mod common;

use common::HomeEnvGuard;
use std::time::Duration;
use std::{fs, thread};
use systemg::{config::load_config, daemon::Daemon};
use tempfile::tempdir;

#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

#[test]
fn restart_kills_detached_descendants() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    // This test verifies that when we restart a service that spawns detached child processes,
    // those children are also terminated (not left as orphans).
    let pid_dir = dir.join("pids");
    fs::create_dir_all(&pid_dir).unwrap();

    let config_yaml = format!(
        r#"
version: '1'
services:
  spawner:
    command: "sh -c '
      mkdir -p {0} &&
      nohup sh -c \"echo \\$\\$ > {0}/child_1.pid && exec sleep 60\" >/dev/null 2>&1 &
      nohup sh -c \"echo \\$\\$ > {0}/child_2.pid && exec sleep 60\" >/dev/null 2>&1 &
      nohup sh -c \"echo \\$\\$ > {0}/child_3.pid && exec sleep 60\" >/dev/null 2>&1 &
      exec sleep 60
    '"
    deployment:
      strategy: "immediate"
"#,
        pid_dir.display()
    );

    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon = Daemon::from_config(config.clone(), false).unwrap();

    daemon.start_services_nonblocking().unwrap();
    thread::sleep(Duration::from_millis(500));

    // Collect child PIDs
    let mut child_pids = vec![];
    for i in 1..=3 {
        let pid_file = pid_dir.join(format!("child_{}.pid", i));
        if let Ok(content) = fs::read_to_string(&pid_file)
            && let Ok(pid) = content.trim().parse::<u32>()
        {
            child_pids.push(pid);
        }
    }
    assert!(
        !child_pids.is_empty(),
        "Should have spawned child processes"
    );

    // All children should be alive
    for &pid in &child_pids {
        assert!(
            common::is_process_alive(pid),
            "Child {} should be alive before restart",
            pid
        );
    }

    // Restart the service
    daemon
        .restart_service("spawner", &config.services["spawner"])
        .unwrap();
    thread::sleep(Duration::from_millis(500));

    // All old children should be dead
    for &pid in &child_pids {
        assert!(
            !common::is_process_alive(pid),
            "Old child {} should be terminated after restart",
            pid
        );
    }

    daemon.shutdown_monitor();
    daemon.stop_services().unwrap();
}

#[test]
fn stop_succeeds_with_stale_pidfile_entry() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    // Create a PID file with an entry for a non-existent process
    let mut pid_file = systemg::daemon::PidFile::default();
    pid_file.insert("ghost_service", 999999).unwrap(); // Non-existent PID

    let config_yaml = r#"
version: '1'
services:
  ghost_service:
    command: "echo 'should not run'"
"#;

    let config_path = dir.join("systemg.yaml");
    fs::write(&config_path, config_yaml).unwrap();

    let config = load_config(Some(config_path.to_str().unwrap())).unwrap();
    let daemon = Daemon::from_config(config, false).unwrap();

    // Stop should succeed even with stale PID
    daemon.stop_service("ghost_service").unwrap();

    // PID should be cleaned up
    let pid_file = systemg::daemon::PidFile::load().unwrap();
    assert!(pid_file.pid_for("ghost_service").is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn zombie_processes_detected() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let _home = HomeEnvGuard::set(&home);

    // Create a process that will become a zombie
    let mut parent = Command::new("sh")
        .arg("-c")
        .arg("sh -c 'sleep 0.1 && exit 0' & sleep 60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let parent_pid = parent.id();

    thread::sleep(Duration::from_millis(200));

    // Check /proc/{pid}/stat to verify zombie state
    let stat_path = format!("/proc/{}/stat", parent_pid);
    if let Ok(stat) = fs::read_to_string(&stat_path) {
        // Zombies are in state 'Z'
        if let Some(state_start) = stat.rfind(')') {
            let state_part = &stat[state_start + 1..].trim();
            if let Some(state_char) = state_part.chars().next()
                && state_char == 'Z'
            {
                // Found zombie process - test passes
                parent.kill().ok();
                parent.wait().ok();
                return;
            }
        }
    }

    // Clean up
    parent.kill().ok();
    parent.wait().ok();
}
