# boot-abort repro

Reproduces (and guards against) the bug where a **single service failing to start
aborts the entire supervisor during boot**, and the daemonized supervisor dies
silently — `sysg start --daemonize` returns `0` while `sysg status` reports
`No running supervisor`.

## The bug

- `Supervisor::run_internal` propagated a per-service `start_service(...)?` error
  out of the boot loop, aborting before the control socket was published.
- `daemonize_systemg` redirected the daemon's stderr to `/dev/null` and the parent
  exited `0` before the child could fail, so the failure was invisible.

A stale process holding a service's port (e.g. an orphan from a prior `restart`)
was enough to make a whole multi-service stack look permanently dead and silent.

## The fix

- Boot loop logs and continues on a per-service start failure; the monitor/restart
  loop then handles the unit per its `restart_policy`.
- `start_supervisor_daemon` forks a detached child and the parent waits for the
  control socket (`wait_for_supervisor_ready`), returning a non-zero error that
  points at `supervisor.log` when boot fails.

## Run it

```sh
# from the repo root (build context is the repo)
docker build -f tests/docker/repro/boot-abort/Dockerfile -t sysg-boot-abort .
docker run --rm sysg-boot-abort
```

Exit `0` = GREEN (fixed). Exit `1` = RED (reproduced / still broken).

To prove the harness, build with the fix reverted first — the run should exit `1`.
