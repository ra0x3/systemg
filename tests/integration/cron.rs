#[path = "common/mod.rs"]
mod common;

use std::{fs, thread, time::Duration};

use common::{HomeEnvGuard, wait_for_file_value};
use systemg::{config::load_config, daemon::Daemon};
use tempfile::tempdir;

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

    daemon.start_services().expect("start services");

    wait_for_file_value(&normal_marker, "normal");
    thread::sleep(Duration::from_millis(300));
    assert!(
        !cron_marker.exists(),
        "cron-managed service should not run during bulk start"
    );

    daemon.stop_services().expect("stop services");
}

#[test]
fn restart_registers_new_cron_jobs() {
    use systemg::supervisor::Supervisor;

    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let config_path_1 = dir.join("config1.yaml");
    fs::write(
        &config_path_1,
        r#"version: "1"
services:
  normal:
    command: "sleep 1000"
    restart_policy: "never"
"#,
    )
    .expect("failed to write initial config");

    let mut supervisor =
        Supervisor::new(config_path_1.clone(), false, None).expect("create supervisor");
    let initial_jobs = supervisor.get_cron_jobs();
    assert_eq!(initial_jobs.len(), 0, "Should start with no cron jobs");

    let config_path_2 = dir.join("config2.yaml");
    fs::write(
        &config_path_2,
        r#"version: "1"
services:
  normal:
    command: "sleep 1000"
    restart_policy: "never"
  new_cron_job:
    command: "echo 'cron job executed'"
    restart_policy: "never"
    cron:
      expression: "* * * * *"
"#,
    )
    .expect("failed to write new config");

    supervisor
        .reload_config_for_test(config_path_2.as_path())
        .expect("reload config should succeed");

    let updated_jobs = supervisor.get_cron_jobs();
    assert_eq!(
        updated_jobs.len(),
        1,
        "Should have 1 cron job after restart"
    );
    assert_eq!(
        updated_jobs[0].service_name, "new_cron_job",
        "The new cron job should be registered"
    );

    supervisor.shutdown_for_test().expect("shutdown supervisor");
}
