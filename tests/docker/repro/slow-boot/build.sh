#!/bin/sh
# Stand-in for a real `build.sh`. Timestamps each invocation so the harness can
# prove the build runs twice: once as the `slow_build` barrier, once again as
# `api`'s pre_start.
echo "build start $(date +%s.%N)" >> /tmp/build.trace
sleep 6
echo "build done  $(date +%s.%N)" >> /tmp/build.trace
