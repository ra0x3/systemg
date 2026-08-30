#!/usr/bin/env bash
# Spawns dynamic children through the supervisor, then stays alive.
set -u
# A real orchestrator waits for work before it spawns; the pause also keeps
# the case honest about what it is testing (lifecycle, not boot timing).
sleep 5
for n in 1 2 3; do
  sysg start --parent-pid "$$" --name "worker_$n" -- sleep 3000 >>/tmp/spawn.log 2>&1
  echo "spawn worker_$n rc=$?" >> /tmp/spawn.log
done
touch /tmp/spawned.done
exec sleep 3000
