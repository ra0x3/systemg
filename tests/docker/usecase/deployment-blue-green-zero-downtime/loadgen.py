import http.client
import json
import os
import sys
import threading
import time

PORT = int(sys.argv[1])
OUT = sys.argv[2]
WORKERS = 4
PACE = 0.02

records = []
lock = threading.Lock()


def request(path):
    started = time.time()
    try:
        conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
        conn.request("GET", path, headers={"Connection": "close"})
        response = conn.getresponse()
        body = response.read().decode()
        conn.close()
        ok = response.status == 200
        pid = body.split()[0] if ok else None
        error = None if ok else f"status {response.status}"
    except Exception as err:
        ok, pid, error = False, None, repr(err)
    record = {
        "start": started,
        "end": time.time(),
        "path": path,
        "ok": ok,
        "pid": pid,
        "error": error,
    }
    with lock:
        records.append(record)
    if ok and not os.path.exists(OUT + ".ready"):
        open(OUT + ".ready", "w").close()


def worker(path, pace):
    while not os.path.exists(OUT + ".stop"):
        request(path)
        time.sleep(pace)


threads = [
    threading.Thread(target=worker, args=("/", PACE), daemon=True)
    for _ in range(WORKERS)
]
threads.append(
    threading.Thread(target=worker, args=("/slow?ms=1500", PACE), daemon=True)
)
for thread in threads:
    thread.start()
while not os.path.exists(OUT + ".stop"):
    time.sleep(0.1)
deadline = time.monotonic() + 10
for thread in threads:
    thread.join(timeout=max(0, deadline - time.monotonic()))
with lock:
    snapshot = list(records)
with open(OUT, "w", encoding="utf-8") as out:
    json.dump(snapshot, out)
sys.exit(1 if any(thread.is_alive() for thread in threads) else 0)
