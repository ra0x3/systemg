# resident-abort repro

Reproduces (and guards against) the resident-supervisor sibling of the
boot-abort bug: with a supervisor already running, **one failing unit aborts
the whole project load/restart**.

## The bug

ca79691 made the *cold boot* loop (`Supervisor::run_internal`) log and continue
when a unit fails to start. But the resident paths never got that fix:

- `sysg start -c b.yaml` against a running supervisor goes through
  `add_project_config` → `start_project_services`, where
  `daemon.start_service(...)?` propagates the first failure. Every unit ordered
  after the bad one is never started, and the CLI gets
  `error[SG0001]: Failed to start service '<bad>': process exited with status 1`.
- `sysg restart -p <project>` goes through `replace_extra_project_runtime` →
  the same `start_project_services`, so a restart aborts the same way.
- Worse: `replace_extra_project_runtime` called `Daemon::stop_services()`, which
  iterates the pid file **shared by every project** — so restarting one project
  stopped every other project's units too (they show up as Stopped/Lost/Zombie).
- It also `remove()`d the project from the registry before work that could
  fail, so a failure mid-restart left the project unregistered and every later
  command answered `project '<id>' is not managed by this supervisor`.

A single broken unit (e.g. a binary that exits 1 at boot) makes the healthy
rest of its project look dead, on every load and every restart.

## The fix

- `start_project_services` logs and continues on a per-unit start failure —
  mirroring the cold-boot behavior — and the project daemon's monitor retries
  the unit per its `restart_policy`.
- `replace_extra_project_runtime` stops only the project's own units (from its
  config, not the shared pid file) and never removes the registry entry — the
  new runtime overwrites it in place.

## Run it

```sh
# from the repo root (build context is the repo)
docker build -f tests/docker/repro/resident-abort/Dockerfile -t sysg-resident-abort .
docker run --rm sysg-resident-abort
```

Exit `0` = GREEN (fixed). Exit `1` = RED (reproduced / still broken).

To prove the harness, build with the fix reverted first — the run should exit `1`.
