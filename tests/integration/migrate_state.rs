#[path = "common/mod.rs"]
mod common;

use std::{fs, path::Path};

use assert_cmd::Command;
use tempfile::tempdir;

/// Builds a home with the exact legacy layout the `__loose__` bug leaves behind:
/// three sibling manifests declaring the same service name, one uniquely-named
/// manifest, shared loose state, and orphan cron rows.
fn legacy_home(root: &Path) -> std::path::PathBuf {
    let state = root.join(".local/share/systemg");
    let units = state.join("units");
    let loose = state.join("projects/__loose__");
    let logs = state.join("logs/__loose__");
    for dir in [&units, &loose, &logs] {
        fs::create_dir_all(dir).unwrap();
    }

    for hash in ["3a2a1f8c6425", "5a9ce32857ea", "6df683933138"] {
        fs::write(
            units.join(format!("gamecast-tunnel-{hash}.yaml")),
            format!(
                "version: \"2\"\nservices:\n  gamecast-tunnel:\n    command: 'echo {hash}'\n"
            ),
        )
        .unwrap();
    }
    fs::write(
        units.join("ngrok-tunnel-8d7ba8fac7f4.yaml"),
        "version: \"2\"\nservices:\n  ngrok-tunnel:\n    command: 'echo ngrok'\n",
    )
    .unwrap();

    fs::write(
        loose.join("state.xml"),
        "<ServiceStateFile>\n  <services>\n    <name>v2:none:gamecast-tunnel</name>\n    \
         <state>\n      <status>stopped</status>\n    </state>\n  </services>\n  \
         <services>\n    <name>v2:none:ngrok-tunnel</name>\n    <state>\n      \
         <status>running</status>\n      <pid>19223</pid>\n    </state>\n  </services>\n\
         </ServiceStateFile>\n",
    )
    .unwrap();
    fs::write(
        loose.join("pid.xml"),
        "<PidFile>\n  <services>\n    <name>ngrok-tunnel</name>\n    <pid>19223</pid>\n  \
         </services>\n</PidFile>\n",
    )
    .unwrap();
    fs::write(
        loose.join("cron_state.xml"),
        "<CronStateFile>\n  <jobs>\n    <hash>de98291cbf657443</hash>\n    <state>\n      \
         <service_name>test_service</service_name>\n    </state>\n  </jobs>\n  <jobs>\n    \
         <hash>eb78b21f76b9fe8f</hash>\n    <state>\n      \
         <service_name>test_service</service_name>\n    </state>\n  </jobs>\n\
         </CronStateFile>\n",
    )
    .unwrap();

    fs::write(logs.join("gamecast-tunnel.log"), b"important log data").unwrap();
    fs::write(logs.join("ngrok-tunnel.log"), b"").unwrap();

    state
}

fn migrate(home: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::new(assert_cmd::cargo::cargo_bin!("sysg"))
        .env("HOME", home)
        .arg("migrate-state")
        .args(args)
        .assert()
}

#[test]
fn dry_run_reports_the_plan_and_changes_nothing() {
    let temp = tempdir().unwrap();
    let state = legacy_home(temp.path());

    migrate(temp.path(), &["--dry-run"])
        .success()
        .stdout(predicates::str::contains("Dry run: nothing was changed"));

    assert!(!state.join("loose_registry.json").exists());
    assert!(state.join("projects/__loose__/state.xml").exists());
}

#[test]
fn a_uniquely_declared_service_is_attributed_to_its_manifest() {
    let temp = tempdir().unwrap();
    legacy_home(temp.path());

    migrate(temp.path(), &["--dry-run"])
        .success()
        .stdout(predicates::str::contains(
            "ngrok-tunnel -> ngrok-tunnel-8d7ba8fac7f4-",
        ));
}

#[test]
fn a_service_three_manifests_declare_is_never_guessed() {
    let temp = tempdir().unwrap();
    legacy_home(temp.path());

    migrate(temp.path(), &["--dry-run"])
        .success()
        .stdout(predicates::str::contains(
            "[service] gamecast-tunnel: declared by 3 manifests",
        ));
}

#[test]
fn orphan_cron_rows_are_archived_rather_than_assigned() {
    let temp = tempdir().unwrap();
    legacy_home(temp.path());

    migrate(temp.path(), &["--dry-run"])
        .success()
        .stdout(predicates::str::contains(
            "[cron] de98291cbf657443: no manifest declares it",
        ))
        .stdout(predicates::str::contains(
            "[cron] eb78b21f76b9fe8f: no manifest declares it",
        ));
}

#[test]
fn migrating_publishes_a_registry_and_preserves_every_source_byte() {
    let temp = tempdir().unwrap();
    let state = legacy_home(temp.path());

    migrate(temp.path(), &[]).success();

    let registry = fs::read_to_string(state.join("loose_registry.json")).unwrap();
    assert!(registry.contains("ngrok-tunnel-8d7ba8fac7f4-"));
    assert!(
        !registry.contains("gamecast-tunnel"),
        "an ambiguous manifest must not be registered"
    );

    // The legacy tree is left untouched, and the archive holds a faithful copy.
    assert!(state.join("projects/__loose__/state.xml").exists());
    let archived = find_archived(temp.path(), "gamecast-tunnel.log");
    assert_eq!(fs::read(archived).unwrap(), b"important log data");
}

#[test]
fn a_completed_migration_can_be_run_again_safely() {
    let temp = tempdir().unwrap();
    let state = legacy_home(temp.path());

    migrate(temp.path(), &[]).success();
    let first = fs::read_to_string(state.join("loose_registry.json")).unwrap();
    migrate(temp.path(), &[]).success();
    let second = fs::read_to_string(state.join("loose_registry.json")).unwrap();

    // Re-running must converge, not accumulate a second entry for the same file.
    assert_eq!(first, second);
    let parsed: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(parsed["entries"].as_object().unwrap().len(), 1);
}

#[test]
fn attributed_state_is_published_into_the_derived_project() {
    let temp = tempdir().unwrap();
    let state = legacy_home(temp.path());

    migrate(temp.path(), &[]).success();

    let project = find_project_dir(&state, "ngrok-tunnel-");
    let published = fs::read_to_string(project.join("state.xml")).unwrap();

    // Re-keyed onto the derived project, and still parseable.
    let id = project.file_name().unwrap().to_string_lossy().to_string();
    assert!(published.contains(&format!("<name>v2:{id}:ngrok-tunnel</name>")));
    assert_eq!(published.matches("</name>").count(), 1);
    assert!(!published.contains("v2:none:"));

    // The ambiguous service must not be dragged along into it.
    assert!(!published.contains("gamecast-tunnel"));

    let pid = fs::read_to_string(project.join("pid.xml")).unwrap();
    assert!(pid.contains("<name>ngrok-tunnel</name>"));
    assert!(pid.contains("19223"));
}

#[test]
fn an_unattributable_service_gets_no_project_directory() {
    let temp = tempdir().unwrap();
    let state = legacy_home(temp.path());

    migrate(temp.path(), &[]).success();

    let projects = fs::read_dir(state.join("projects")).unwrap();
    let names: Vec<String> = projects
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("gamecast-tunnel-")),
        "ambiguous state must not be materialised anywhere: {names:?}"
    );
    // The legacy tree stays put so the data is still recoverable.
    assert!(names.iter().any(|name| name == "__loose__"));
}

#[test]
fn a_home_with_no_legacy_state_reports_nothing_to_do() {
    let temp = tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".local/share/systemg/units")).unwrap();

    migrate(temp.path(), &[])
        .success()
        .stdout(predicates::str::contains("No legacy `__loose__` state"));
}

fn find_project_dir(state: &Path, prefix: &str) -> std::path::PathBuf {
    fs::read_dir(state.join("projects"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(prefix))
        })
        .unwrap_or_else(|| panic!("no project directory starting with {prefix}"))
}

fn find_archived(home: &Path, name: &str) -> std::path::PathBuf {
    let migrations = home.join(".local/share/systemg-migrations");
    let mut stack = vec![migrations];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|file| file == name) {
                return path;
            }
        }
    }
    panic!("no archived copy of {name}");
}
