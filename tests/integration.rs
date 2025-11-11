use assert_cmd::Command;
use predicates::boolean::PredicateBooleanExt;
use predicates::str::contains;
use std::{fs, thread, time::Duration};
use tempfile::tempdir;

#[test]
#[ignore]
fn test_systemg_start_and_status_and_logs() {
    let temp = tempdir().expect("failed to create tempdir");
    let dir = temp.path();

    // Python program.
    fs::write(
        dir.join("main.py"),
        r#"
import os
import time
import logging

logging.basicConfig(
    level=logging.DEBUG,
    format="%(asctime)s - %(levelname)s - %(message)s",
    handlers=[logging.StreamHandler()]
)

logger = logging.getLogger(__name__)

def main():
    logger.info("Starting...")
    time.sleep(5)
    foo = os.environ["FOO"]
    bar = os.environ["BAR"]

    for x in range(100):
        logger.info(f"This is my log foo({foo}) and bar({bar}): ({x})")
        time.sleep(1)

if __name__ == "__main__":
    main()
"#,
    )
    .unwrap();

    // Perl program.
    fs::write(
        dir.join("main.pl"),
        r#"
use strict;
use warnings;
use Time::HiRes qw(sleep);

for my $i (1..100) {
    my $timestamp = localtime();
    print "$timestamp INFO - Hello World ($i)\n";
    sleep(1);
}
"#,
    )
    .unwrap();

    // .env file.
    fs::write(dir.join(".env"), "BAR=bar\n").unwrap();

    // Systemg manifest.
    fs::write(
        dir.join("config.yaml"),
        r#"
version: "1"
services:
  perl:
    command: "perl main.pl"
    restart_policy: "on_failure"
    retries: 5
    backoff: "5s"

  py:
    env:
      file: ".env"
      vars:
        FOO: "foo"
    command: "python3 main.py"
    restart_policy: "on_failure"
    retries: 5
    backoff: "5s"
"#,
    )
    .unwrap();

    // Start systemg.
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    cmd.current_dir(dir)
        .arg("start")
        .arg("-c")
        .arg("config.yaml");
    cmd.assert().success();

    // Give services time to boot,
    thread::sleep(Duration::from_secs(3));

    // Check general status.
    let mut status = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    status.current_dir(dir).arg("status");
    status
        .assert()
        .success()
        .stdout(contains("perl").and(contains("py")));

    // Check specific service status.
    let mut short_status = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    short_status.current_dir(dir).arg("status").arg("-s");
    short_status
        .assert()
        .success()
        .stdout(contains("perl").and(contains("py")));

    // Check general logs.
    let mut logs = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    logs.current_dir(dir).arg("logs").arg("-l").arg("10");
    logs.assert()
        .success()
        .stdout(contains("Hello World").or(contains("foo")));

    // Check specific service logs.
    let mut logs_py = Command::new(assert_cmd::cargo::cargo_bin!("sysg"));
    logs_py
        .current_dir(dir)
        .arg("logs")
        .arg("py")
        .arg("-l")
        .arg("10");
    logs_py
        .assert()
        .success()
        .stdout(contains("foo").and(contains("bar")));
}
