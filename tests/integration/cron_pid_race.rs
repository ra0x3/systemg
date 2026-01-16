#[path = "common/mod.rs"]
mod common;

use std::{fs, thread, time::Duration};

use common::HomeEnvGuard;
use systemg::supervisor::Supervisor;
use tempfile::tempdir;

/// Test for the race condition where a fast-completing cron job gets incorrectly
/// marked as failed with "Failed to get PID from PID file" error.
///
/// This reproduces the bug where:
/// 1. A cron job starts and completes quickly
/// 2. The supervisor tries to find the PID after the process has already exited
/// 3. The PID has been cleaned up, causing a false "Failed to get PID" error
#[test]
fn fast_completing_cron_job_not_marked_as_failed() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let success_marker = dir.join("cron_success.txt");
    let config_path = dir.join("config.yaml");

    // Create a cron job that completes instantaneously (just true command)
    // This maximizes the chance of hitting the race condition
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  fast_cron_job:
    command: "sh -c 'echo SUCCESS > \"{marker}\"; true'"
    restart_policy: "never"
    cron:
      expression: "* * * * *"
"#,
            marker = success_marker.display()
        ),
    )
    .expect("failed to write config");

    let mut supervisor =
        Supervisor::new(config_path.clone(), false, None).expect("create supervisor");

    // Get a reference to the cron manager before moving supervisor
    let cron_manager = supervisor.get_cron_manager_for_test();

    // Start supervisor in a background thread
    let handle = thread::spawn(move || supervisor.run());

    // Wait for the cron job to execute (it runs every minute)
    let start = std::time::Instant::now();
    while !success_marker.exists() && start.elapsed() < Duration::from_secs(70) {
        thread::sleep(Duration::from_millis(100));
    }

    // Ensure the cron job actually ran
    assert!(
        success_marker.exists(),
        "Cron job should have executed and created the success marker"
    );

    // Verify the file content to ensure the command ran successfully
    let content = fs::read_to_string(&success_marker).expect("read marker file");
    assert_eq!(
        content.trim(),
        "SUCCESS",
        "Cron job should have written SUCCESS"
    );

    // Give the supervisor time to process the completion
    thread::sleep(Duration::from_secs(2));

    // Check the last execution status - this is where the bug manifests
    let last_status = cron_manager.get_last_execution_status("fast_cron_job");

    match last_status {
        Some(systemg::cron::CronExecutionStatus::Success) => {
            // Test passes - job correctly marked as successful
            println!("Test passed: Cron job correctly marked as successful");
        }
        Some(systemg::cron::CronExecutionStatus::Failed(reason)) => {
            // This is the bug - fast completing job incorrectly marked as failed
            panic!(
                "Fast-completing cron job incorrectly marked as failed: {}. \
                The job actually succeeded (marker file exists with SUCCESS content), \
                but was marked as failed due to PID race condition.",
                reason
            );
        }
        _ => {
            panic!("Expected to find execution status for fast_cron_job");
        }
    }

    // Terminate the supervisor thread
    drop(handle);
}
