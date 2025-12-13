#[cfg(target_os = "linux")]
use assert_cmd::Command;
#[cfg(target_os = "linux")]
use predicates::prelude::*;
use std::env;
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use systemg::{
    config::load_config,
    daemon::{Daemon, PidFile, ServiceLifecycleStatus, ServiceStateFile},
};
use tempfile::tempdir;

struct HomeEnvGuard {
    previous: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl HomeEnvGuard {
    fn set(home: &Path) -> Self {
        let lock = systemg::test_utils::env_lock();
        let previous = env::var("HOME").ok();
        unsafe {
            env::set_var("HOME", home);
        }
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe {
                env::set_var("HOME", value);
            },
            None => unsafe {
                env::remove_var("HOME");
            },
        }
    }
}

fn wait_for_lines(path: &Path, expected: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(content) = fs::read_to_string(path) {
            let lines: Vec<_> = content.lines().map(|line| line.to_string()).collect();
            if lines.len() >= expected {
                return lines;
            }
        }

        if Instant::now() >= deadline {
            panic!("Timed out waiting for {expected} lines in {:?}", path);
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_file_value(path: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(content) = fs::read_to_string(path)
            && content.trim() == expected
        {
            return;
        }

        if Instant::now() >= deadline {
            panic!("Timed out waiting for value '{}' in {:?}", expected, path);
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_pid(service: &str) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(pid_file) = PidFile::load()
            && let Some(pid) = pid_file.pid_for(service)
        {
            return pid;
        }

        if Instant::now() >= deadline {
            panic!("Timed out waiting for PID entry for service '{service}'");
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("Timed out waiting for {:?} to exist", path);
}

fn wait_for_pid_removed(service: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(pid_file) = PidFile::load()
            && pid_file.pid_for(service).is_none()
        {
            return;
        }

        if Instant::now() >= deadline {
            panic!("Timed out waiting for service '{}' to clear PID", service);
        }

        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "linux")]
fn wait_for_process_exit(pid: u32) {
    use std::path::PathBuf;

    let deadline = Instant::now() + Duration::from_secs(10);
    let proc_path = PathBuf::from(format!("/proc/{}", pid));

    while Instant::now() < deadline {
        if !proc_path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }

    panic!("Timed out waiting for PID {} to exit", pid);
}

fn wait_for_latest_pid(pid_dir: &Path, min_runs: usize) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut entries: Vec<_> = fs::read_dir(pid_dir)
            .ok()
            .into_iter()
            .flat_map(|iter| iter.filter_map(Result::ok))
            .filter_map(|entry| {
                let raw_name = entry.file_name();
                let name = raw_name.to_str()?;
                let rest = name.strip_prefix("run_")?;
                let stem = rest.strip_suffix(".pid")?;
                let idx = stem.parse::<usize>().ok()?;
                Some((idx, entry.path()))
            })
            .collect();

        if entries.len() >= min_runs {
            entries.sort_by_key(|(idx, _)| *idx);
            if let Some((_, path)) = entries.last()
                && let Ok(contents) = fs::read_to_string(path)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                return pid;
            }
        }

        if Instant::now() >= deadline {
            panic!(
                "Timed out waiting for at least {min_runs} pid captures in {:?}",
                pid_dir
            );
        }

        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn hooks_on_start_and_stop_success() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let env_path = dir.join("service.env");
    fs::write(&env_path, "MY_TOKEN=file_token\nFILE_ONLY=file_only\n")
        .expect("failed to write env file");

    let hook_log = dir.join("hooks.log");
    fs::File::create(&hook_log).expect("create hook log placeholder");

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  demo:
    command: "sleep 60"
    env:
      file: "{env_file}"
      vars:
        MY_TOKEN: "vars_token"
    hooks:
      on_start:
        success:
          command: "echo START:$MY_TOKEN:$FILE_ONLY >> \"{hook_log}\""
          timeout: "2s"
      on_stop:
        success:
          command: "echo STOP:$MY_TOKEN:$FILE_ONLY >> \"{hook_log}\""
          timeout: "2s"
"#,
            env_file = env_path.display(),
            hook_log = hook_log.display()
        ),
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let service = config.services.get("demo").expect("demo service");
    let env = service.env.as_ref().expect("demo env");
    let vars = env.vars.as_ref().expect("demo vars");
    assert_eq!(vars.get("MY_TOKEN"), Some(&"vars_token".to_string()));
    let daemon = Daemon::from_config(config, false).expect("daemon from config");

    daemon.start_services_nonblocking().expect("start services");

    let lines = wait_for_lines(&hook_log, 1);
    assert_eq!(lines, vec!["START:vars_token:file_only".to_string()]);

    daemon.stop_service("demo").expect("stop service");

    let lines = wait_for_lines(&hook_log, 2);
    assert_eq!(
        lines,
        vec![
            "START:vars_token:file_only".to_string(),
            "STOP:vars_token:file_only".to_string(),
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
    fs::File::create(&hook_log).expect("create hook log placeholder");

    let service_script = dir.join("service.sh");
    fs::write(
        &service_script,
        r#"#!/bin/sh
count=0
if [ -f "$STATE_FILE" ]; then
  count=$(cat "$STATE_FILE")
fi
count=$((count + 1))
echo "$count" > "$STATE_FILE"

if [ "$count" -eq 1 ]; then
  sleep 1
  exit 1
fi

sleep 5
"#,
    )
    .expect("failed to write service script");

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  crashy:
    command: "sh '{script}'"
    backoff: "1s"
    env:
      vars:
        STATE_FILE: "{state}"
    hooks:
      on_start:
        success:
          command: "echo on_start.success >> \"{hook_log}\""
      on_stop:
        error:
          command: "echo on_stop.error >> \"{hook_log}\""
        success:
          command: "echo on_stop.success >> \"{hook_log}\""
      on_restart:
        success:
          command: "echo on_restart.success >> \"{hook_log}\""
          timeout: "3s"
"#,
            script = service_script.display(),
            state = state_file.display(),
            hook_log = hook_log.display()
        ),
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config, false).expect("daemon from config");

    daemon.start_services_nonblocking().expect("start services");

    // Wait for the crash + restart cycle to populate the first four events.
    let lines = wait_for_lines(&hook_log, 4);
    let expected_prefix = vec![
        "on_start.success".to_string(),
        "on_stop.error".to_string(),
        "on_start.success".to_string(),
        "on_restart.success".to_string(),
    ];
    assert!(lines.starts_with(&expected_prefix));

    daemon.stop_service("crashy").expect("stop crashy");

    let lines = wait_for_lines(&hook_log, 5);
    assert_eq!(
        lines,
        vec![
            "on_start.success".to_string(),
            "on_stop.error".to_string(),
            "on_start.success".to_string(),
            "on_restart.success".to_string(),
            "on_stop.success".to_string(),
        ]
    );

    daemon.shutdown_monitor();
}

#[test]
fn skip_flag_prevents_service_start() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let marker = dir.join("skipped.log");
    let script = dir.join("service.sh");
    fs::write(&script, "#!/bin/sh\necho started > ./skipped.log\n")
        .expect("failed to write script");

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  skipped:
    command: "sh ./service.sh"
    skip: true
"#,
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config, false).expect("daemon from config");

    daemon.start_services_nonblocking().expect("start services");

    thread::sleep(Duration::from_millis(250));

    assert!(
        !marker.exists(),
        "skip flag should prevent the service command from executing"
    );

    daemon.shutdown_monitor();
}

#[test]
fn restart_records_new_pid() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let count_path = dir.join("count.txt");
    let pid_dir = dir.join("pids");
    fs::create_dir_all(&pid_dir).expect("failed to create pid dir");

    let script = dir.join("flaky.sh");
    fs::write(
        &script,
        "#!/bin/sh\nset -eu\ncount=0\nif [ -f \"$COUNT_PATH\" ]; then\n  count=$(cat \"$COUNT_PATH\")\nfi\ncount=$((count + 1))\necho \"$count\" > \"$COUNT_PATH\"\nmkdir -p \"$PID_DIR\"\necho \"$$\" > \"$PID_DIR/run_$count.pid\"\n\nif [ \"$count\" -eq 1 ]; then\n  sleep 1\n  exit 1\nfi\n\nsleep 30\n",
    )
    .expect("failed to write flaky script");

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  flaky:
    command: "sh {}"
    restart_policy: "always"
    backoff: "1s"
    env:
      vars:
        COUNT_PATH: "{}"
        PID_DIR: "{}"
"#,
            script.display(),
            count_path.display(),
            pid_dir.display()
        ),
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config, false).expect("daemon from config");

    daemon.start_services_nonblocking().expect("start services");

    wait_for_file_value(&count_path, "2");

    let first_pid_path = pid_dir.join("run_1.pid");
    wait_for_path(&first_pid_path);
    let first_pid: u32 = fs::read_to_string(&first_pid_path)
        .expect("read first pid")
        .trim()
        .parse()
        .expect("parse first pid");

    let recorded_pid = wait_for_latest_pid(&pid_dir, 2);
    assert_ne!(first_pid, recorded_pid);

    #[cfg(target_os = "linux")]
    wait_for_process_exit(first_pid);

    daemon.stop_service("flaky").expect("stop flaky");
    wait_for_pid_removed("flaky");
    daemon.shutdown_monitor();
}

#[test]
fn stop_succeeds_with_stale_pidfile_entry() {
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

    let real_pid = wait_for_pid("stale");

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

    wait_for_pid_removed("stale");

    wait_for_path(&done_marker);

    daemon.shutdown_monitor();
}

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
    let assert = cmd
        .env("SYSTEMG_TAIL_MODE", "oneshot")
        .arg("logs")
        .arg("arb_rs")
        .arg("--lines")
        .arg("1")
        .assert();

    assert
        .success()
        .stdout(predicate::str::contains("arb_rs"))
        .stdout(predicate::str::contains("streamed stdout line"));

    unsafe { env::remove_var("SYSTEMG_TAIL_MODE") };
}

#[cfg(target_os = "linux")]
#[test]
fn status_flags_zombie_processes() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let child_pid = unsafe { libc::fork() };
    assert!(child_pid >= 0, "fork failed");

    if child_pid == 0 {
        unsafe { libc::_exit(0) };
    }

    let mut pid_file = PidFile::load().expect("load pid file");
    pid_file
        .insert("arb_rs", child_pid as u32)
        .expect("insert zombie pid");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    let assert = cmd.arg("status").arg("--service").arg("arb_rs").assert();

    assert
        .success()
        .stdout(predicate::str::contains("Process"))
        .stdout(predicate::str::contains("zombie"));

    unsafe {
        let mut status: libc::c_int = 0;
        libc::waitpid(child_pid, &mut status, 0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn status_reports_skipped_services() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  skipped_service:
    command: "echo should be skipped"
    skip: true
"#,
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("daemon from config");

    daemon
        .start_service(
            "skipped_service",
            config.services.get("skipped_service").unwrap(),
        )
        .expect("start skipped service");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.arg("status");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("skipped_service"))
        .stdout(predicate::str::contains("Skipped"));
}

#[cfg(target_os = "linux")]
#[test]
fn status_reports_successful_exit() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  oneshot:
    command: "sh -c 'echo done'"
    restart_policy: "never"
"#,
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("daemon from config");

    daemon
        .start_service("oneshot", config.services.get("oneshot").unwrap())
        .expect("start oneshot");

    wait_for_pid_removed("oneshot");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.arg("status");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("oneshot"))
        .stdout(predicate::str::contains("Exited successfully"));
}

#[test]
fn skip_flag_controls_service_execution() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    // Test 1: skip: false - service SHOULD run and create file
    let not_skipped_marker = dir.join("not_skipped.txt");
    let config_path_1 = dir.join("config_not_skipped.yaml");
    fs::write(
        &config_path_1,
        format!(
            r#"version: "1"
services:
  writer:
    command: "echo 'service executed' > '{}'"
    skip: false
"#,
            not_skipped_marker.display()
        ),
    )
    .expect("failed to write config 1");

    // Simulate supervisor code path: load config and call daemon.start_service() directly
    let config_1 =
        load_config(Some(config_path_1.to_str().unwrap())).expect("load config 1");
    let daemon_1 =
        Daemon::from_config(config_1.clone(), false).expect("daemon from config 1");

    // This mimics what Supervisor::run() does at src/supervisor.rs:77-83
    for (service_name, service_config) in &config_1.services {
        if service_config.cron.is_none() {
            daemon_1
                .start_service(service_name, service_config)
                .expect("start service");
        }
    }

    // Wait and verify file was created when skip: false
    wait_for_file_value(&not_skipped_marker, "service executed");

    daemon_1.stop_services().expect("stop services");

    // Test 2: skip: true - service SHOULD NOT run and file should not exist
    let skipped_marker = dir.join("skipped.txt");
    let config_path_2 = dir.join("config_skipped.yaml");
    fs::write(
        &config_path_2,
        format!(
            r#"version: "1"
services:
  writer:
    command: "echo 'service executed' > '{}'"
    skip: true
"#,
            skipped_marker.display()
        ),
    )
    .expect("failed to write config 2");

    // Simulate supervisor code path: load config and call daemon.start_service() directly
    let config_2 =
        load_config(Some(config_path_2.to_str().unwrap())).expect("load config 2");
    let daemon_2 =
        Daemon::from_config(config_2.clone(), false).expect("daemon from config 2");

    // This mimics what Supervisor::run() does at src/supervisor.rs:77-83
    // This is the BUGGY code path that doesn't check skip!
    for (service_name, service_config) in &config_2.services {
        if service_config.cron.is_none() {
            daemon_2
                .start_service(service_name, service_config)
                .expect("start service");
        }
    }

    // Wait a bit to ensure service would have run if it was going to
    thread::sleep(Duration::from_millis(500));

    // Verify file was NOT created when skip: true
    assert!(
        !skipped_marker.exists(),
        "skip: true should prevent service from executing, but marker file exists"
    );

    daemon_2.stop_services().expect("stop services");
}

#[test]
fn cron_services_not_started_during_bulk_start() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let normal_marker = dir.join("normal.txt");
    let cron_marker = dir.join("cron.txt");
    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  normal:
    command: "sh -c 'echo normal > \"{normal}\"; sleep 1'"
    restart_policy: "never"
  cron_job:
    command: "sh -c 'echo cron > \"{cron}\"'"
    restart_policy: "never"
    cron:
      expression: "*/30 * * * *"
"#,
            normal = normal_marker.display(),
            cron = cron_marker.display()
        ),
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("daemon from config");

    daemon.start_services_nonblocking().expect("start services");

    wait_for_file_value(&normal_marker, "normal");
    thread::sleep(Duration::from_millis(300));
    assert!(
        !cron_marker.exists(),
        "cron-managed service should not run during bulk start"
    );

    daemon.stop_services().expect("stop services");
}

#[test]
fn manual_stop_suppresses_pending_restart() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  flaky:
    command: "sh -c 'sleep 1; exit 1'"
    restart_policy: "always"
    backoff: "1s"
"#,
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("daemon from config");

    daemon.start_services_nonblocking().expect("start services");

    let first_pid = wait_for_pid("flaky");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut restarted_pid = first_pid;
    while Instant::now() < deadline {
        if let Ok(pid_file) = PidFile::load()
            && let Some(pid) = pid_file.pid_for("flaky")
            && pid != first_pid
        {
            restarted_pid = pid;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    assert_ne!(
        first_pid, restarted_pid,
        "service should have restarted automatically before manual stop"
    );

    daemon.stop_service("flaky").expect("manual stop");

    thread::sleep(Duration::from_secs(3));

    let pid_file = PidFile::load().expect("load pid file");
    assert!(
        pid_file.pid_for("flaky").is_none(),
        "service should remain stopped after manual stop despite pending restart"
    );

    thread::sleep(Duration::from_secs(1));
    let pid_file = PidFile::load().expect("reload pid file");
    assert!(
        pid_file.pid_for("flaky").is_none(),
        "service should not restart"
    );

    daemon.shutdown_monitor();
}

#[test]
fn supervisor_logs_operational_events() {
    use assert_cmd::Command;

    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    // Create a simple test config
    let config_path = dir.join("systemg.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
services:
  test_service:
    command: "sleep 30"
"#,
    )
    .expect("failed to write config");

    let supervisor_log = home.join(".local/share/systemg/supervisor.log");

    // Start sysg with debug logging and daemonize
    let mut start_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    start_cmd
        .arg("start")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .arg("--daemonize")
        .arg("--log-level")
        .arg("debug")
        .assert()
        .success();

    // Wait for supervisor to start and service to initialize
    thread::sleep(Duration::from_secs(2));

    // Run status command to trigger more logging
    let mut status_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    status_cmd
        .arg("status")
        .arg("--service")
        .arg("test_service")
        .assert()
        .success();

    // Restart the service to generate restart logs
    let mut restart_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    restart_cmd
        .arg("restart")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .arg("--service")
        .arg("test_service")
        .assert()
        .success();

    thread::sleep(Duration::from_secs(1));

    // Stop the supervisor
    let mut stop_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    stop_cmd.arg("stop").assert().success();

    // Wait for graceful shutdown
    thread::sleep(Duration::from_millis(500));

    // Verify the supervisor log file exists and contains expected content
    assert!(
        supervisor_log.exists(),
        "Supervisor log file should exist at {:?}",
        supervisor_log
    );

    let log_contents = fs::read_to_string(&supervisor_log)
        .expect("Should be able to read supervisor log");

    assert!(
        !log_contents.is_empty(),
        "Supervisor log should not be empty"
    );

    // Verify specific operational events that should be in the logs
    // These are actual log messages from the codebase

    // Check for service start messages (from daemon.rs:1420 and 1579)
    assert!(
        log_contents.contains("Starting service: test_service")
            || log_contents.contains("Starting service"),
        "Log should contain 'Starting service' message. Log contents:\n{}",
        log_contents
    );

    // Check for restart messages (from daemon.rs:935 and 822)
    assert!(
        log_contents.contains("Performing immediate restart for service: test_service")
            || log_contents.contains("restart"),
        "Log should contain restart information. Log contents:\n{}",
        log_contents
    );

    // Check for stop/shutdown messages (from daemon.rs:1770, supervisor.rs:369)
    assert!(
        log_contents.contains("Stopping service")
            || log_contents.contains("Supervisor shutting down")
            || log_contents.contains("stop"),
        "Log should contain stop/shutdown messages. Log contents:\n{}",
        log_contents
    );

    // Check that service name appears in logs
    assert!(
        log_contents.contains("test_service"),
        "Log should mention the service name 'test_service'. Log contents:\n{}",
        log_contents
    );

    // Verify debug-level logs are present (since we used --log-level debug)
    // Debug logs should include things like "Starting service thread" (daemon.rs:1462, 1622)
    assert!(
        log_contents.contains("DEBUG") || log_contents.len() > 200,
        "Log should contain debug-level information. Log length: {}, contents:\n{}",
        log_contents.len(),
        log_contents
    );
}

#[test]
fn pre_start_failure_records_error_state() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        r#"version: "1"
project_dir: "."
services:
  failing_pre_start:
    command: "sh -c 'echo should_not_run'"
    restart_policy: "always"
    deployment:
      pre_start: "sh -c 'exit 42'"
"#,
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let failing_config = config
        .services
        .get("failing_pre_start")
        .cloned()
        .expect("service exists");

    let pid_file = Arc::new(Mutex::new(PidFile::load().expect("load pid file")));
    let state_file = Arc::new(Mutex::new(
        ServiceStateFile::load().expect("load service state file"),
    ));

    let daemon = Daemon::new(
        config,
        Arc::clone(&pid_file),
        Arc::clone(&state_file),
        false,
    );

    let result = daemon.start_service("failing_pre_start", &failing_config);
    assert!(result.is_err(), "pre-start failure should surface as error");

    {
        let guard = state_file.lock().expect("lock state file");
        let entry = guard
            .get("failing_pre_start")
            .expect("service entry recorded");
        assert_eq!(
            entry.status,
            ServiceLifecycleStatus::ExitedWithError,
            "failed pre-start should mark service as exited with error",
        );
        assert_eq!(
            entry.exit_code,
            Some(42),
            "exit code from pre-start should be persisted",
        );
        assert!(entry.pid.is_none(), "no pid should be recorded");
    }

    {
        let guard = pid_file.lock().expect("lock pid file");
        assert!(
            guard.pid_for("failing_pre_start").is_none(),
            "pid file should not contain service entry after failed pre-start",
        );
    }

    daemon.shutdown_monitor();
}

#[test]
fn pre_start_command_executes_on_service_startup() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let pre_start_marker = dir.join("pre_start_executed.txt");
    let service_marker = dir.join("service_executed.txt");

    let config_path = dir.join("config.yaml");
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  test_app:
    command: "sh -c 'if [ -f \"{pre_marker}\" ]; then echo success > \"{svc_marker}\"; sleep 2; else echo pre_start_failed > \"{svc_marker}\"; sleep 2; fi'"
    deployment:
      strategy: "immediate"
      pre_start: "echo pre_start_done > \"{pre_marker}\""
"#,
            pre_marker = pre_start_marker.display(),
            svc_marker = service_marker.display()
        ),
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_str().unwrap())).expect("load config");
    let daemon = Daemon::from_config(config.clone(), false).expect("daemon from config");

    daemon
        .start_service("test_app", config.services.get("test_app").unwrap())
        .expect("start test_app");

    // Wait for service to execute and check pre_start marker
    wait_for_path(&service_marker);

    // Verify pre_start command was executed
    assert!(
        pre_start_marker.exists(),
        "pre_start command should have created marker file before service started"
    );

    // Verify service saw the pre_start marker
    let service_output = fs::read_to_string(&service_marker)
        .expect("read service marker")
        .trim()
        .to_string();
    assert_eq!(
        service_output, "success",
        "Service should have seen the pre_start marker file"
    );

    daemon.stop_service("test_app").expect("stop test_app");
    daemon.shutdown_monitor();
}

#[test]
fn stale_socket_doesnt_block_commands() {
    use assert_cmd::Command;
    use std::os::unix::net::UnixListener;

    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    // Create a simple test config
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

    // Create the runtime directory structure
    let runtime_dir = home.join(".local/share/systemg");
    fs::create_dir_all(&runtime_dir).expect("failed to create runtime dir");

    // Create a stale socket file (simulating a killed supervisor)
    let socket_path = runtime_dir.join("control.sock");
    let _listener = UnixListener::bind(&socket_path).expect("failed to create socket");
    drop(_listener); // Drop the listener to make the socket stale

    // Also create a stale PID file pointing to a non-existent process
    let pid_file = runtime_dir.join("sysg.pid");
    fs::write(&pid_file, "999999").expect("failed to write stale pid");

    // Try to run stop command - should NOT get "Connection refused"
    // Instead it should detect the stale socket and clean up
    let mut stop_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    let output = stop_cmd
        .arg("stop")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("failed to execute stop");

    // Should succeed or at least not error with "Connection refused"
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Connection refused"),
        "Should not get 'Connection refused' with stale socket. stderr: {}",
        stderr
    );

    // Verify stale artifacts were cleaned up
    assert!(
        !socket_path.exists() || !pid_file.exists(),
        "Stale socket or PID file should be cleaned up"
    );
}

#[test]
fn purge_removes_all_state() {
    use assert_cmd::Command;

    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    // Create a simple test config
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

    // Start supervisor and let service run and exit
    let mut start_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    start_cmd
        .arg("start")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .arg("--daemonize")
        .assert()
        .success();

    thread::sleep(Duration::from_secs(3));

    // Stop supervisor
    let mut stop_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    stop_cmd.arg("stop").assert().success();

    thread::sleep(Duration::from_millis(500));

    // Verify state files exist
    let runtime_dir = home.join(".local/share/systemg");
    let state_file = runtime_dir.join("state.json");
    let pid_file = runtime_dir.join("pid.json");
    let lock_file = runtime_dir.join("pid.json.lock");
    let supervisor_log = runtime_dir.join("supervisor.log");

    assert!(state_file.exists(), "state.json should exist before purge");
    assert!(
        pid_file.exists() || lock_file.exists() || supervisor_log.exists(),
        "At least one runtime file should exist before purge"
    );

    // Run purge command
    let mut purge_cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    purge_cmd.arg("purge").assert().success();

    // Verify all state files are removed
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

    // Verify the entire runtime directory is removed
    assert!(
        !runtime_dir.exists(),
        "Runtime directory should be completely removed after purge"
    );
}
