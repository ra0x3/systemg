#!/usr/bin/env python3
"""Runs a foreground `sysg start` under a PTY and records WHEN it exits.

Writes 'EXITED <code>' to <marker> the moment the foreground start returns (the
terminal yields), or leaves the marker absent if it wedges. Args:
<config> <max_seconds> <marker>. Used to prove the foreground detaches when its
project is stopped from another terminal instead of blocking forever.
"""
import os, sys, pty, time, select

config, secs, marker = sys.argv[1], float(sys.argv[2]), sys.argv[3]
try:
    os.remove(marker)
except FileNotFoundError:
    pass

pid, fd = pty.fork()
if pid == 0:
    os.execvp("sysg", ["sysg", "start", "--config", config])

end = time.time() + secs
exited = None
while time.time() < end:
    r, _, _ = select.select([fd], [], [], 0.3)
    if r:
        try:
            if not os.read(fd, 4096):
                pass
        except OSError:
            pass
    done, status = os.waitpid(pid, os.WNOHANG)
    if done:
        exited = os.waitstatus_to_exitcode(status)
        break

if exited is not None:
    with open(marker, "w") as f:
        f.write(f"EXITED {exited}\n")
