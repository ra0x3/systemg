#!/usr/bin/env python3
"""Run a foreground `sysg start` under a PTY, capture its output, and allow a
REAL terminal Ctrl-C to be delivered on demand.

A keyboard Ctrl-C makes the tty driver send SIGINT to the whole foreground
process group. Signalling a single pid with `kill -INT` does NOT reproduce that,
and will make a correct foreground teardown look broken. Writing the control
file (<pidfile>.ctl) makes this helper write 0x03 into the pty master, which is
exactly what a keypress does.

Args: <binary> <config> <outfile> <pidfile> [max_seconds]
"""
import os
import select
import signal
import sys
import time

import pty

binary, cfg, out, pidf = sys.argv[1:5]
max_seconds = float(sys.argv[5]) if len(sys.argv) > 5 else 600.0
if not os.path.isabs(binary) or not os.access(binary, os.X_OK):
    raise SystemExit(f"binary must be an absolute executable path: {binary}")
ctl = pidf + ".ctl"
try:
    os.remove(ctl)
except FileNotFoundError:
    pass

pid, fd = pty.fork()
if pid == 0:
    os.execv(binary, [binary, "start", "--config", cfg])

with open(pidf, "w") as f:
    f.write(str(pid))

end = time.time() + max_seconds
sent = False
status = None
try:
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
            done, child_status = os.waitpid(pid, os.WNOHANG)
            if done:
                status = child_status
                break
finally:
    if status is None:
        try:
            os.write(fd, b"\x03")
        except OSError:
            pass
        deadline = time.time() + 10
        while time.time() < deadline:
            done, child_status = os.waitpid(pid, os.WNOHANG)
            if done:
                status = child_status
                break
            time.sleep(0.1)
    if status is None:
        os.kill(pid, signal.SIGTERM)
        deadline = time.time() + 5
        while time.time() < deadline:
            done, child_status = os.waitpid(pid, os.WNOHANG)
            if done:
                status = child_status
                break
            time.sleep(0.1)
    if status is None:
        os.kill(pid, signal.SIGKILL)
        _, status = os.waitpid(pid, 0)

if os.WIFEXITED(status):
    raise SystemExit(os.WEXITSTATUS(status))
raise SystemExit(128 + os.WTERMSIG(status))
