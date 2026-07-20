#!/usr/bin/env python3
"""Run a foreground `sysg start` under a PTY, capture its output, and allow a
REAL terminal Ctrl-C to be delivered on demand.

A keyboard Ctrl-C makes the tty driver send SIGINT to the whole foreground
process group. Signalling a single pid with `kill -INT` does NOT reproduce that,
and will make a correct foreground teardown look broken. Writing the control
file (<pidfile>.ctl) makes this helper write 0x03 into the pty master, which is
exactly what a keypress does.

Args: <config> <outfile> <pidfile> [max_seconds]
"""
import os
import select
import sys
import time

import pty

cfg, out, pidf = sys.argv[1], sys.argv[2], sys.argv[3]
max_seconds = float(sys.argv[4]) if len(sys.argv) > 4 else 600.0
ctl = pidf + ".ctl"
try:
    os.remove(ctl)
except FileNotFoundError:
    pass

pid, fd = pty.fork()
if pid == 0:
    os.execvp("sysg", ["sysg", "start", "--config", cfg])

with open(pidf, "w") as f:
    f.write(str(pid))

end = time.time() + max_seconds
sent = False
with open(out, "wb") as f:
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.3)
        if ready:
            try:
                data = os.read(fd, 4096)
            except OSError:
                break
            if not data:
                break
            f.write(data)
            f.flush()
        if not sent and os.path.exists(ctl):
            os.write(fd, b"\x03")
            sent = True
        done, _ = os.waitpid(pid, os.WNOHANG)
        if done:
            break
