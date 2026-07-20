#!/usr/bin/env python3
"""Runs `sysg start` (foreground) under a PTY, holds it, then Ctrl-C's it.
Args: <config> <hold_seconds>."""
import os, sys, pty, time, select, signal
config, hold = sys.argv[1], float(sys.argv[2])
pid, fd = pty.fork()
if pid == 0:
    os.execvp("sysg", ["sysg", "start", "--config", config])
end = time.time() + hold
while time.time() < end:
    r, _, _ = select.select([fd], [], [], 0.3)
    if r:
        try:
            if not os.read(fd, 4096): break
        except OSError: break
os.kill(pid, signal.SIGINT)   # Ctrl-C — the "sysg stop" the user did
time.sleep(4)
