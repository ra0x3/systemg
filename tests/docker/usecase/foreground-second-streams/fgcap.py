#!/usr/bin/env python3
"""Runs one foreground `sysg start` under a PTY and CAPTURES its streamed output.

Unlike fg2.py (which discards output), this writes everything the foreground
start prints to <outfile>, so the harness can assert that a foreground terminal
actually streamed ITS project's logs. Args: <config> <seconds> <outfile>.
Leaves services running for the assertions; container teardown reaps them.
"""
import os, sys, pty, time, select

config, secs, outfile = sys.argv[1], float(sys.argv[2]), sys.argv[3]
pid, fd = pty.fork()
if pid == 0:
    os.execvp("sysg", ["sysg", "start", "--config", config])

end = time.time() + secs
with open(outfile, "wb") as out:
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.3)
        if r:
            try:
                data = os.read(fd, 4096)
            except OSError:
                break
            if not data:
                break
            out.write(data)
            out.flush()
