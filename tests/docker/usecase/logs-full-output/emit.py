#!/usr/bin/env python3
"""Emits N numbered lines (LINE_<i>) then idles, so the log has a known count."""
import sys, time
n = int(sys.argv[1]) if len(sys.argv) > 1 else 200
for i in range(1, n + 1):
    print(f"LINE_{i}", flush=True)
time.sleep(3000)
