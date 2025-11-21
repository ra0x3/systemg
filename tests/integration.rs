use std::env;
use std::sync::{Mutex, OnceLock};
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};
use systemg::{config::load_config, daemon::Daemon};
use tempfile::tempdir;

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct HomeEnvGuard {
    previous: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl HomeEnvGuard {
    fn set(home: &Path) -> Self {
        let lock = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
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
