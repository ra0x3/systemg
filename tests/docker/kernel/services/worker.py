#!/usr/bin/env python3
"""
Sample worker service for kernel mode testing
Simulates a real production worker that connects to Redis and PostgreSQL
"""

import os
import time
import signal
import sys


def signal_handler(sig, frame):
    print(
        f"Worker {os.environ.get('WORKER_ID', 'unknown')}: Received signal {sig}, shutting down gracefully..."
    )
    sys.exit(0)


signal.signal(signal.SIGTERM, signal_handler)
signal.signal(signal.SIGINT, signal_handler)

print(f"Worker started - ID: {os.environ.get('WORKER_ID', 'unknown')}")
print(f"Redis URL: {os.environ.get('REDIS_URL', 'not set')}")

# Simulate work
while True:
    time.sleep(5)
    print(
        f"Worker {os.environ.get('WORKER_ID', 'unknown')}: Processing task at {time.time()}"
    )
