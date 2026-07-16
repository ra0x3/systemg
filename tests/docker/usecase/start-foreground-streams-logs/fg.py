#!/usr/bin/env python3
"""Runs `sysg start` (foreground) under a real PTY and captures its console.

Foreground streaming is TTY-gated — sysg tails each unit's merged stdout+stderr
to the terminal only when stdout is a tty. A plain pipe/redirect suppresses it,
so the harness must allocate a PTY. Args: <config> <out-file> <seconds>. After
the window, SIGTERM the foreground process (as Ctrl-C would) and write whatever
the console emitted.
"""
import os
import sys
import pty
import time
import select
import signal

config, out_path, secs = sys.argv[1], sys.argv[2], float(sys.argv[3])

pid, fd = pty.fork()
if pid == 0:
    os.execvp("sysg", ["sysg", "start", "--config", config])

buf = b""
deadline = time.time() + secs
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.3)
    if r:
        try:
            data = os.read(fd, 4096)
        except OSError:
            break
        if not data:
            break
        buf += data

# Ctrl-C is how a user exits a foreground start; sysg attaches a SIGINT handler
# that stops the project and shuts the supervisor down.
os.kill(pid, signal.SIGINT)
time.sleep(3)
try:
    os.waitpid(pid, os.WNOHANG)
except OSError:
    pass

with open(out_path, "wb") as f:
    f.write(buf)
