import os
import signal
import sys
import time


with open("/tmp/mode", encoding="utf-8") as mode:
    if mode.read().strip() == "fail":
        time.sleep(0.4)
        print("candidate failed without a port collision", file=sys.stderr, flush=True)
        sys.exit(42)

print("historical Address already in use output", file=sys.stderr, flush=True)
with open("/tmp/server-pids", "a", encoding="utf-8") as output:
    output.write(f"{os.getpid()}\n")
    output.flush()
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
while True:
    time.sleep(1)
