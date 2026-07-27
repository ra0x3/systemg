//! Regression coverage for the `__loose__` single-slot bug.
//!
//! Project-less manifests used to share one `__loose__` project. Starting a
//! second one therefore looked like an edit of the first, and the supervisor
//! reconciled the difference by stopping the service already running there —
//! killing a live process on an unrelated `start`. Each loose manifest now owns
//! a project derived from its path.

#[path = "common/mod.rs"]
mod common;

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use assert_cmd::Command;

/// Short base dir: a unix socket path must fit in `SUN_LEN`, and a tempdir under
/// the default temp root overflows it once the state path is appended.
fn short_home(tag: &str) -> PathBuf {
    let home = PathBuf::from(format!("/tmp/sysg-it-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join(".local/share/systemg/units")).unwrap();
    home
}

fn write_unit(home: &Path, name: &str) -> PathBuf {
    let path = home
        .join(".local/share/systemg/units")
        .join(format!("{name}.yaml"));
    // Emit a line before sleeping: a purge truncates rather than removes, so a
    // service with no output cannot demonstrate whether one reached it.
    fs::write(
        &path,
        format!(
            "version: \"2\"\nservices:\n  {name}-svc:\n    \
             command: 'sh -c \"echo OUTPUT_{name}; sleep 300\"'\n"
        ),
    )
    .unwrap();
    path
}

fn sysg(home: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.env("HOME", home);
    cmd
}

fn start(home: &Path, config: &Path) {
    sysg(home)
        .args(["start", "--daemonize", "--config"])
        .arg(config)
        .assert()
        .success();
}

fn projects(home: &Path) -> Vec<String> {
    let out = sysg(home).arg("status").output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("Project: "))
        .map(|line| {
            line.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

fn wait_for_projects(home: &Path, count: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        seen = projects(home);
        if seen.len() >= count {
            return seen;
        }
        thread::sleep(Duration::from_millis(250));
    }
    seen
}

/// Bytes currently in a service's active log. A purge TRUNCATES rather than
/// removes, so existence proves nothing — only the length does.
fn log_len(home: &Path, service: &str) -> u64 {
    let out = sysg(home)
        .args(["logs", "--path", "-s", service])
        .output()
        .unwrap();
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0)
}

fn teardown(home: &Path) {
    let _ = sysg(home).args(["stop", "--supervisor"]).output();
    thread::sleep(Duration::from_millis(500));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_second_loose_config_does_not_evict_the_first() {
    let home = short_home("evict");
    let alpha = write_unit(&home, "alpha-aaaa1111");
    let beta = write_unit(&home, "beta-bbbb2222");

    start(&home, &alpha);
    let after_first = wait_for_projects(&home, 1);
    assert_eq!(after_first.len(), 1, "first loose config should be up");

    start(&home, &beta);
    let after_second = wait_for_projects(&home, 2);

    assert_eq!(
        after_second.len(),
        2,
        "the second loose config must not replace the first: {after_second:?}"
    );
    assert!(after_second.iter().any(|id| id.starts_with("alpha-")));
    assert!(after_second.iter().any(|id| id.starts_with("beta-")));

    teardown(&home);
}

#[test]
fn sibling_manifests_declaring_the_same_service_stay_separate() {
    // The reported case: several unit files whose service name is identical and
    // which differ only by the writer's command hash.
    let home = short_home("siblings");
    let first = write_unit(&home, "tunnel-3a2a1f8c6425");
    let second = write_unit(&home, "tunnel-5a9ce32857ea");

    start(&home, &first);
    wait_for_projects(&home, 1);
    start(&home, &second);
    let ids = wait_for_projects(&home, 2);

    assert_eq!(ids.len(), 2, "sibling units must not collide: {ids:?}");
    assert!(ids.iter().any(|id| id.starts_with("tunnel-3a2a1f8c6425-")));
    assert!(ids.iter().any(|id| id.starts_with("tunnel-5a9ce32857ea-")));

    teardown(&home);
}

#[test]
fn no_loose_project_is_named_for_the_legacy_shared_id() {
    let home = short_home("notlegacy");
    let unit = write_unit(&home, "solo-1111");

    start(&home, &unit);
    let ids = wait_for_projects(&home, 1);

    assert!(
        !ids.iter().any(|id| id == "__loose__"),
        "loose projects must carry a derived id: {ids:?}"
    );

    teardown(&home);
}

#[test]
fn a_bare_service_selector_finds_a_service_in_any_loose_project() {
    // Every loose manifest is its own project now, so a bare `-s` must resolve
    // across all of them. Scoping it to whichever config happened to be in
    // reach — the cwd's, or the last one started — answers for one project
    // while the service runs under another.
    let home = short_home("bareselector");
    let first = write_unit(&home, "first-1111");
    let second = write_unit(&home, "second-2222");

    start(&home, &first);
    thread::sleep(Duration::from_millis(300));
    start(&home, &second);
    wait_for_projects(&home, 2);
    thread::sleep(Duration::from_secs(2));

    for service in ["first-1111-svc", "second-2222-svc"] {
        sysg(&home)
            .args(["logs", "-s", service, "--no-follow"])
            .assert()
            .success();
    }

    // A name no project declares must still be refused.
    sysg(&home)
        .args(["logs", "-s", "ghostsvc", "--no-follow"])
        .assert()
        .failure();

    teardown(&home);
}

#[test]
fn log_operations_target_the_service_s_own_project() {
    // `--path` and `--purge` used to resolve against whichever config was in
    // reach, so an unscoped operation on a service in one loose project could
    // report success while acting on another.
    let home = short_home("logscope");
    let first = write_unit(&home, "aa-1111");
    let second = write_unit(&home, "bb-2222");

    start(&home, &first);
    thread::sleep(Duration::from_millis(300));
    start(&home, &second);
    wait_for_projects(&home, 2);
    thread::sleep(Duration::from_secs(2));

    let out = sysg(&home)
        .args(["logs", "--path", "-s", "bb-2222-svc"])
        .output()
        .unwrap();
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        path.contains("bb-2222-"),
        "--path must point into the service's own project, got {path}"
    );

    let spared_before = log_len(&home, "aa-1111-svc");
    assert!(spared_before > 0, "the spared service should have output");

    sysg(&home)
        .args(["logs", "--purge", "-s", "bb-2222-svc"])
        .assert()
        .success();
    thread::sleep(Duration::from_millis(500));

    // A purge truncates in place, so only the byte count shows whether it
    // reached across into the other project.
    assert_eq!(
        log_len(&home, "bb-2222-svc"),
        0,
        "the target was not purged"
    );
    assert_eq!(
        log_len(&home, "aa-1111-svc"),
        spared_before,
        "purging one loose project truncated another's logs"
    );

    teardown(&home);
}

#[test]
fn a_project_scoped_log_purge_spares_every_other_project() {
    // `logs --purge -p <project>` ignored its selector and cleared every
    // project's logs — the same cross-project reach as the original bug.
    let home = short_home("purgescope");
    let first = write_unit(&home, "aa-1111");
    let second = write_unit(&home, "bb-2222");

    start(&home, &first);
    thread::sleep(Duration::from_millis(300));
    start(&home, &second);
    let ids = wait_for_projects(&home, 2);
    thread::sleep(Duration::from_secs(2));

    let target = ids
        .iter()
        .find(|id| id.starts_with("bb-2222-"))
        .expect("bb project should be up");

    let spared_before = log_len(&home, "aa-1111-svc");
    assert!(spared_before > 0, "the spared service should have output");

    sysg(&home)
        .args(["logs", "--purge", "-p", target])
        .assert()
        .success();
    thread::sleep(Duration::from_millis(500));

    assert_eq!(
        log_len(&home, "bb-2222-svc"),
        0,
        "the target was not purged"
    );
    assert_eq!(
        log_len(&home, "aa-1111-svc"),
        spared_before,
        "a project-scoped purge truncated another project's logs"
    );

    teardown(&home);
}

#[test]
fn a_service_that_writes_no_log_file_is_still_addressable() {
    // `logs.sink: none` produces no file at all, and file writes are async, so
    // the absence of a log cannot stand in for the absence of a service.
    let home = short_home("nosink");
    let normal = write_unit(&home, "aa-1111");
    let quiet = home.join(".local/share/systemg/units").join("bb-2222.yaml");
    fs::write(
        &quiet,
        "version: \"2\"\nservices:\n  bb-2222-svc:\n    command: 'sleep 300'\n    \
         logs:\n      sink: none\n",
    )
    .unwrap();

    start(&home, &normal);
    thread::sleep(Duration::from_millis(300));
    start(&home, &quiet);
    wait_for_projects(&home, 2);
    thread::sleep(Duration::from_secs(2));

    sysg(&home)
        .args(["logs", "-s", "bb-2222-svc", "--no-follow"])
        .assert()
        .success();

    // A name nothing declares is still refused, so the check keeps its purpose.
    sysg(&home)
        .args(["logs", "-s", "typosvc", "--no-follow"])
        .assert()
        .failure();

    teardown(&home);
}

#[test]
fn a_scoped_purge_cannot_escape_the_log_root() {
    let home = short_home("escape");
    let unit = write_unit(&home, "aa-1111");
    start(&home, &unit);
    wait_for_projects(&home, 1);
    thread::sleep(Duration::from_secs(1));

    let outside = home.join(".local/share/systemg/outside.log");
    fs::write(&outside, b"OUTSIDE DATA").unwrap();

    for selector in ["..", "/etc", "a/b", ""] {
        sysg(&home)
            .args(["logs", "--purge", "-p", selector])
            .assert()
            .failure();
        // The service-scoped path builds its own paths and needs the same guard.
        sysg(&home)
            .args(["logs", "--purge", "-p", selector, "-s", "outside"])
            .assert()
            .failure();
    }

    assert_eq!(
        fs::read(&outside).unwrap(),
        b"OUTSIDE DATA",
        "a traversing -p reached outside the log root"
    );

    // A project directory symlinked out of the log root must not be followed.
    let evil = home.join(".local/share/systemg/logs/evil");
    std::os::unix::fs::symlink(home.join(".local/share/systemg"), &evil).unwrap();
    sysg(&home)
        .args(["logs", "--purge", "-p", "evil"])
        .assert()
        .failure();
    assert_eq!(fs::read(&outside).unwrap(), b"OUTSIDE DATA");

    teardown(&home);
}

#[test]
fn a_project_prefixed_selector_resolves_to_that_project() {
    let home = short_home("prefix");
    let first = write_unit(&home, "aa-1111");
    let second = write_unit(&home, "bb-2222");

    start(&home, &first);
    thread::sleep(Duration::from_millis(300));
    start(&home, &second);
    let ids = wait_for_projects(&home, 2);
    thread::sleep(Duration::from_secs(2));

    let target = ids
        .iter()
        .find(|id| id.starts_with("bb-2222-"))
        .expect("bb project should be up");

    let out = sysg(&home)
        .args(["logs", "--path", "-s", &format!("{target}/bb-2222-svc")])
        .output()
        .unwrap();
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // The prefix must be split, not looked up whole as a service name.
    assert!(path.contains(target), "got {path}");
    assert!(path.ends_with("bb-2222-svc.log"), "got {path}");

    // The live purge path splits it too — it sends the name to the supervisor,
    // which matches on a bare name and would never find a slash-bearing one.
    let spared = log_len(&home, "aa-1111-svc");
    sysg(&home)
        .args(["logs", "--purge", "-s", &format!("{target}/bb-2222-svc")])
        .assert()
        .success();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        log_len(&home, "bb-2222-svc"),
        0,
        "the prefixed purge missed"
    );
    assert_eq!(log_len(&home, "aa-1111-svc"), spared);

    teardown(&home);
}

#[test]
fn a_purged_loose_project_does_not_come_back_on_the_next_boot() {
    let home = short_home("purge");
    let keep = write_unit(&home, "keep-1111");
    let drop = write_unit(&home, "drop-2222");

    start(&home, &keep);
    thread::sleep(Duration::from_millis(300));
    start(&home, &drop);
    let ids = wait_for_projects(&home, 2);
    let doomed = ids
        .iter()
        .find(|id| id.starts_with("drop-"))
        .expect("drop project should be up")
        .clone();

    sysg(&home)
        .args(["stop", "--supervisor"])
        .assert()
        .success();
    thread::sleep(Duration::from_secs(1));
    sysg(&home)
        .args(["purge", "-p", &doomed])
        .assert()
        .success();

    // The registry is what boot replays, so a purge that left its entry behind
    // would silently undo itself here.
    start(&home, &keep);
    let after = wait_for_projects(&home, 1);

    assert!(
        !after.iter().any(|id| id == &doomed),
        "purged project came back: {after:?}"
    );
    assert!(after.iter().any(|id| id.starts_with("keep-")));

    teardown(&home);
}

#[test]
fn every_loose_project_is_restored_after_a_supervisor_restart() {
    let home = short_home("restore");
    let one = write_unit(&home, "one-1111");
    let two = write_unit(&home, "two-2222");
    let three = write_unit(&home, "three-3333");

    for unit in [&one, &two, &three] {
        start(&home, unit);
        thread::sleep(Duration::from_millis(300));
    }
    let before = wait_for_projects(&home, 3);
    assert_eq!(before.len(), 3, "three loose projects should be up");

    sysg(&home)
        .args(["stop", "--supervisor"])
        .assert()
        .success();
    thread::sleep(Duration::from_secs(1));

    // Starting ONE of them must bring back all three: `config_hint` holds a
    // single path, so without the loose registry the other two would be lost.
    start(&home, &one);
    let after = wait_for_projects(&home, 3);

    assert_eq!(
        after.len(),
        3,
        "every registered loose project must be restored: {after:?}"
    );

    teardown(&home);
}
