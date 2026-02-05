#[path = "common/mod.rs"]
mod common;

use std::{
    fs, thread,
    time::{Duration, Instant},
};

use common::HomeEnvGuard;
use systemg::{
    config::load_config,
    cron::{CronExecutionStatus, CronManager},
    daemon::{Daemon, ServiceReadyState},
};
use tempfile::tempdir;

/// Regression coverage for fast-completing cron jobs. Historically the supervisor could
/// mis-diagnose a cron invocation as failed if the process exited before the PID was captured.
#[test]
fn fast_completing_cron_job_not_marked_as_failed() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let success_marker = dir.join("cron_success.txt");
    let config_path = dir.join("systemg.yaml");

    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  fast_cron_job:
    command: "sh -c 'echo SUCCESS > \"{marker}\"; true'"
    restart_policy: "never"
    cron:
      expression: "*/1 * * * * *"
"#,
            marker = success_marker.display()
        ),
    )
    .expect("failed to write config");

    let config = load_config(Some(config_path.to_string_lossy().as_ref()))
        .expect("load test config");

    let cron_manager = CronManager::new();
    cron_manager
        .sync_from_config(&config)
        .expect("sync cron config");

    let daemon = Daemon::from_config(config.clone(), false).expect("create daemon");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut fired = false;
    while Instant::now() < deadline {
        let due_jobs = cron_manager.get_due_jobs();
        if due_jobs.contains(&"fast_cron_job".to_string()) {
            fired = true;
            for job_name in due_jobs {
                let service = config
                    .services
                    .get(&job_name)
                    .expect("cron service present in config");

                match daemon.start_service(&job_name, service) {
                    Ok(ServiceReadyState::CompletedSuccess) => {}
                    Ok(ServiceReadyState::Running) => {
                        daemon
                            .stop_service(&job_name)
                            .expect("stop running cron process");
                    }
                    Err(err) => panic!("failed to start cron job {}: {err}", job_name),
                }

                cron_manager.mark_job_completed(
                    &job_name,
                    CronExecutionStatus::Success,
                    Some(0),
                    Vec::new(),
                );
            }
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    assert!(fired, "cron job should have been scheduled");

    assert!(
        success_marker.exists(),
        "Cron job should have created the success marker"
    );

    let content = fs::read_to_string(&success_marker).expect("read marker file");
    assert_eq!(content.trim(), "SUCCESS");

    let status = cron_manager
        .get_last_execution_status("fast_cron_job")
        .expect("cron execution status recorded");

    assert!(matches!(status, CronExecutionStatus::Success));
}
