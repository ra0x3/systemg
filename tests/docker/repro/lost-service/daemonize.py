#!/usr/bin/env python3
"""Double-forking server, the way a real `serve` daemon backgrounds itself.

The process sysg exec's forks a grandchild that binds the port and lives on,
then the parent exits 0 immediately. sysg's Child handle therefore refers to a
process that has already exited, while the actual worker is a live, re-parented
grandchild that the daemon holds no handle to.
"""

import os
import socket
import sys
import time

port = int(sys.argv[1])

pid = os.fork()
if pid > 0:
    # Parent: exit at once. This is the process sysg spawned and waits on.
    os._exit(0)

# Grandchild: detach and become the real, long-lived worker.
os.setsid()

sock = socket.socket()
sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
sock.bind(("127.0.0.1", port))
sock.listen()

# Advertise the true worker PID so the harness can kill exactly this process.
with open("/tmp/worker.pid", "w") as fh:
    fh.write(str(os.getpid()))

while True:
    time.sleep(1)
