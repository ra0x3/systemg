#!/usr/bin/env python3
"""Runs one `sysg start` (foreground) under a PTY, holding it up for a window.

Foreground starts own the terminal, so each runs under its own PTY. This backs
one terminal; the harness launches two of these to model term1 + term2. Args:
<config> <seconds>. Exits after the window (leaving services up — the harness
asserts on them — then reaps by killing the process group).
"""
import os, sys, pty, time, select, signal

config, secs = sys.argv[1], float(sys.argv[2])
pid, fd = pty.fork()
if pid == 0:
    os.execvp("sysg", ["sysg", "start", "--config", config])
end = time.time() + secs
while time.time() < end:
    r, _, _ = select.select([fd], [], [], 0.3)
    if r:
        try:
            if not os.read(fd, 4096):
                break
        except OSError:
            break
# leave it running for the assertions; the container teardown reaps it
