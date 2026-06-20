# Zombie / restart-leak reproduction

Reproduces and guards against the production bug where a service wrapper shell
exits while its real worker keeps running (reparented to PID 1 but retaining the
original process group), causing:

- `sysg status` to report a healthy long-running service as `Zombie`/`Failing`
  because it read the recorded wrapper PID.
- Restarts to spawn a fresh worker alongside the surviving orphan, leaking
  duplicate workers on every restart.

## Manual reproduction

```sh
docker build -f tests/docker/zombie-repro/Dockerfile -t systemg-zombie-repro .
docker run -d --name zr systemg-zombie-repro
# Kill the wrapper shells, then observe: no <defunct> children, exactly one
# worker per service after restart, and `sysg status` stays HEALTHY.
docker exec zr bash -c 'for p in $(ps -eo pid,comm | awk "\$2==\"sh\"{print \$1}"); do kill -TERM $p; done'
```

## Regression test

```sh
docker build -f tests/docker/zombie-repro/Dockerfile.test -t systemg-zombie-test .
docker run --rm systemg-zombie-test
```

Runs `restart_reaps_orphaned_group_member_linux`, which asserts the previous
process group has zero live members after a restart (no leaked orphan).
