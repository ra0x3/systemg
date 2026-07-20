#!/usr/bin/env sh
n=$(cat /tmp/pre-count 2>/dev/null || echo 0)
echo $((n + 1)) > /tmp/pre-count
