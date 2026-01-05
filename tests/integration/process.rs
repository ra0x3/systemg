#[path = "common/mod.rs"]
mod common;

use common::HomeEnvGuard;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::path::Path;
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
fn restart_kills_detached_descendants_via_detacher() {
    use std::time::Instant;

    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let child_pid_path = dir.join("child.pid");
    let script = dir.join("detacher.py");
    fs::write(
        &script,
        r#"#!/usr/bin/env python3
import os
import signal
import time

child_path = os.environ["CHILD_PID_PATH"]


def spawn():
    pid = os.fork()
    if pid == 0:
        os.setsid()
        signal.signal(signal.SIGTERM, lambda *_: None)
        signal.signal(signal.SIGINT, lambda *_: None)
        with open(child_path, "w") as fh:
            fh.write(str(os.getpid()))
            fh.flush()
        while True:
            time.sleep(1)
    else:
        signal.signal(signal.SIGTERM, lambda *_: time.sleep(0.5))
        with open(child_path + ".parent", "w") as fh:
            fh.write(str(os.getpid()))
            fh.flush()
        while True:
            time.sleep(1)


spawn()
"#,
    )
    .expect("failed to write detacher script");
    let mut perms = fs::metadata(&script).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod script");

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  detacher:
    command: "{}"
    env:
      vars:
        CHILD_PID_PATH: "{}"
"#,
            script.display(),
            child_pid_path.display()
        ),
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let service_cfg = config
        .services
        .get("detacher")
        .expect("service present")
        .clone();
    let daemon = Daemon::from_config(config, false).expect("daemon from config");

    daemon.start_services_nonblocking().expect("start services");

    let first_parent_pid = common::wait_for_pid("detacher");
    common::wait_for_path(&child_pid_path);
    let read_child_pid = |path: &Path| -> u32 {
        fs::read_to_string(path)
            .expect("read child pid")
            .trim()
            .parse()
            .expect("parse child pid")
    };
    let first_child_pid = read_child_pid(&child_pid_path);
    assert!(
        common::is_process_alive(first_child_pid),
        "detached child should be running"
    );

    daemon
        .restart_service("detacher", &service_cfg)
        .expect("restart detacher");

    let new_parent_pid = common::wait_for_pid("detacher");
    assert_ne!(
        first_parent_pid, new_parent_pid,
        "restart should record a new parent pid"
    );

    let mut new_child_pid = 0u32;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(&child_pid_path)
            && let Ok(pid) = contents.trim().parse::<u32>()
            && pid != first_child_pid
        {
            new_child_pid = pid;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    assert!(new_child_pid != 0, "child pid should update after restart");

    let mut attempts = 0;
    while attempts < 50 && common::is_process_alive(first_child_pid) {
        thread::sleep(Duration::from_millis(100));
        attempts += 1;
    }
    assert!(
        !common::is_process_alive(first_child_pid),
        "detached child should be terminated after restart"
    );
    assert!(
        common::is_process_alive(new_child_pid),
        "replacement detached child should be running"
    );

    daemon.stop_service("detacher").expect("stop detacher");
    common::wait_for_pid_removed("detacher");
    daemon.shutdown_monitor();
}

#[test]
fn stop_succeeds_with_stale_pidfile_entry_with_corrupted_pid() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let script = dir.join("stale.sh");
    let done_marker = dir.join("stale.done");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nfinish() {{ touch \"{}\"; }}\ntrap finish EXIT TERM INT\nwhile true; do sleep 1; done\n",
            done_marker.display()
        ),
    )
    .expect("failed to write stale script");

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  stale:
    command: "sh ./stale.sh"
"#,
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config, false).expect("daemon from config");

    daemon.start_services_nonblocking().expect("start services");

    let real_pid = common::wait_for_pid("stale");

    let bogus_pid = real_pid.saturating_add(10_000);
    let pid_file_path = home.join(".local/share/systemg/pid.json");
    fs::create_dir_all(pid_file_path.parent().unwrap())
        .expect("failed to create pid directory");
    let fake_contents = format!(
        "{{\n  \"services\": {{\n    \"stale\": {}\n  }}\n}}\n",
        bogus_pid
    );
    fs::write(&pid_file_path, fake_contents).expect("failed to corrupt pid file");

    daemon.stop_service("stale").expect("stop stale");

    common::wait_for_pid_removed("stale");

    common::wait_for_path(&done_marker);

    daemon.shutdown_monitor();
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
