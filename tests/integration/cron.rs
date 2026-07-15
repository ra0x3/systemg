#[path = "common/mod.rs"]
mod common;

use std::{
    fs, thread,
    time::{Duration, Instant},
};

use common::HomeEnvGuard;
use systemg::{
    config::load_config,
    cron::{CronExecutionStatus, CronStateFile},
    ipc::{self, ControlCommand, ControlError},
    state_store::StateStore,
};
use tempfile::tempdir;

/// Loads the cron state file from whichever project directory holds `hash`.
fn cron_state_with_hash(hash: &str) -> Option<CronStateFile> {
    let projects_dir = systemg::runtime::state_dir().join("projects");
    let entries = std::fs::read_dir(&projects_dir).ok()?;
    let mut fallback = None;
    for entry in entries.flatten() {
        let id = entry.file_name();
        let store = StateStore::for_project(&id.to_string_lossy());
        if let Ok(state) = CronStateFile::load(store) {
            if state.jobs().contains_key(hash) {
                return Some(state);
            }
            fallback = Some(state);
        }
    }
    fallback
}

#[test]
fn supervisor_persists_cron_execution_history() {
    use systemg::supervisor::Supervisor;

    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();
    let home = dir.join("home");
    fs::create_dir_all(&home).expect("failed to create home dir");
    let _home = HomeEnvGuard::set(&home);

    let marker = dir.join("cron-history-marker.txt");
    let config_path = dir.join("systemg.yaml");
    fs::write(
        &config_path,
        format!(
            r#"version: "1"
services:
  history_cron:
    command: "sh -c 'date +%s >> \"{marker}\"'"
    restart_policy: "never"
    cron:
      expression: "*/1 * * * * *"
"#,
            marker = marker.display()
        ),
    )
    .expect("failed to write config");

    let config =
        load_config(Some(config_path.to_string_lossy().as_ref())).expect("load config");
    let service_hash = config
        .services
        .get("history_cron")
        .expect("history cron service")
        .compute_hash();

    let config_for_thread = config_path.clone();
    let supervisor_thread = thread::spawn(move || {
        let mut supervisor =
            Supervisor::new(config_for_thread, false, None).expect("create supervisor");
        supervisor.run().expect("run supervisor");
    });

    wait_for_supervisor_socket();
    wait_for_cron_history_record(&service_hash);

    let content = fs::read_to_string(&marker).expect("read marker");
    assert!(
        !content.trim().is_empty(),
        "cron command should have written the marker"
    );

    let _ = ipc::send_command(&ControlCommand::Shutdown);
    supervisor_thread
        .join()
        .expect("supervisor thread should shut down cleanly");
}

fn wait_for_supervisor_socket() {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match ipc::send_command(&ControlCommand::Status { live: false }) {
            Ok(_) => return,
            Err(ControlError::NotAvailable) | Err(ControlError::Io(_)) => {}
            Err(err) => panic!("unexpected supervisor status error: {err}"),
        }

        if Instant::now() >= deadline {
            panic!("timed out waiting for supervisor socket");
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_cron_history_record(service_hash: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(state) = cron_state_with_hash(service_hash)
            && let Some(job) = state.jobs().get(service_hash)
            && let Some(record) = job.execution_history.back()
            && record.completed_at.is_some()
            && matches!(record.status, Some(CronExecutionStatus::Success))
            && record.exit_code == Some(0)
        {
            return;
        }

        if Instant::now() >= deadline {
            let state = cron_state_with_hash(service_hash);
            panic!(
                "timed out waiting for persisted cron execution history for {service_hash}; state={state:#?}"
            );
        }

        thread::sleep(Duration::from_millis(100));
    }
}
