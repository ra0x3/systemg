#!/usr/bin/env python3
"""Runs a foreground `sysg start` under a PTY, captures its output to <outfile>,
writes the child sysg PID to <pidfile>, and records exit to <markerfile>.

The harness sends SIGINT (Ctrl-C) to the PID in <pidfile> to simulate pressing
Ctrl-C in THAT terminal only, then asserts this foreground yields while a sibling
foreground keeps running. Args: <config> <max_seconds> <outfile> <pidfile> <marker>.
"""
import os, sys, pty, time, select, signal

config, secs = sys.argv[1], float(sys.argv[2])
outfile, pidfile, marker = sys.argv[3], sys.argv[4], sys.argv[5]
for f in (marker,):
    try:
        os.remove(f)
    except FileNotFoundError:
        pass

pid, fd = pty.fork()
if pid == 0:
    # Ctrl-C is delivered as SIGINT to this child (the sysg start process).
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    os.execvp("sysg", ["sysg", "start", "--config", config])

with open(pidfile, "w") as f:
    f.write(str(pid))

end = time.time() + secs
exited = None
with open(outfile, "wb") as out:
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.3)
        if r:
            try:
                data = os.read(fd, 4096)
            except OSError:
                data = b""
            if data:
                out.write(data)
                out.flush()
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            exited = os.waitstatus_to_exitcode(status)
            break

if exited is not None:
    with open(marker, "w") as f:
        f.write(f"EXITED {exited}\n")
