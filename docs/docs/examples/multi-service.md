---
sidebar_position: 30
title: Multi-Service Playground
---

# Multi-Service Playground

This walkthrough wires three cooperative services together to highlight `systemg`'s cron scheduling, restart supervision, and lifecycle hooks. Clone the repo, `cd examples/multi-service`, and run:

```bash
sysg start --log-level debug --config systemg.yaml
```

`systemg` will manage the following services for ~2 minutes before everything shuts down cleanly.

| Service       | Type        | Purpose |
|---------------|-------------|---------|
| `py_size`     | Long-lived  | Prints the size of companion files every 10 seconds. Intentionally crashes once (after 60 s), triggering a hook that posts a JSON payload and proving the restart flow before exiting successfully at 120 s. |
| `count_number`| Cron        | Appends "The number is *n*" to `lines.txt` every 10 seconds, keeping a simple counter on disk. |
| `echo_lines`  | Worker      | Tails the newest line from `lines.txt` every 5 seconds and mirrors it into `echo.txt` while streaming friendly output. |

## Configuration

### systemg.yaml

The `systemg.yaml` configuration file defines three services:

```yaml
version: "1"
services:
  py_size:
    command: "python3 py_size.py"
    working_dir: "."
    restart_policy: "always"
    backoff: "5s"
    hooks:
      on_restart:
        error:
          command: "curl -s -X POST https://jsonplaceholder.typicode.com/posts -H 'Content-Type: application/json' -d '{\"message\":\"I goof\\'d\"}'"
          timeout: "10s"
  count_number:
    command: "sh count_number.sh"
    working_dir: "."
    cron:
      expression: "*/10 * * * * *"
    restart_policy: "never"
  echo_lines:
    command: "sh echo_lines.sh"
    working_dir: "."
    restart_policy: "never"
    depends_on:
      - "count_number"
```

### count_number.sh

Increments a counter every 10 seconds and writes to `lines.txt`:

```bash
#!/usr/bin/env sh
set -eu

ROOT_DIR="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
STATE_FILE="$ROOT_DIR/.count_state"
START_FILE="$ROOT_DIR/.count_start"
LINES_FILE="$ROOT_DIR/lines.txt"

if [ ! -f "$START_FILE" ]; then
  date +%s > "$START_FILE"
fi

start_epoch="$(cat "$START_FILE")"
now_epoch="$(date +%s)"
elapsed=$((now_epoch - start_epoch))

if [ "$elapsed" -ge 120 ]; then
  printf 'count_number has completed its 120 second window; exiting gracefully.\n'
  exit 0
fi

if [ -f "$STATE_FILE" ]; then
  current="$(cat "$STATE_FILE")"
else
  current=0
fi

current=$((current + 1))
printf '%s\n' "$current" > "$STATE_FILE.tmp"
mv "$STATE_FILE.tmp" "$STATE_FILE"

printf 'The number is %s\n' "$current" >> "$LINES_FILE"
```

### echo_lines.sh

Reads the latest line from `lines.txt` every 5 seconds and writes to `echo.txt`:

```bash
#!/usr/bin/env sh
set -eu

ROOT_DIR="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
LINES_FILE="$ROOT_DIR/lines.txt"
ECHO_FILE="$ROOT_DIR/echo.txt"
START_TIME=$(date +%s)
END_TIME=$((START_TIME + 120))

mkdir -p "$ROOT_DIR"
: > "$ECHO_FILE"

while [ "$(date +%s)" -lt "$END_TIME" ]; do
  if [ -f "$LINES_FILE" ]; then
    last_line=$(tail -n 1 "$LINES_FILE" || true)
    if [ -z "$last_line" ]; then
      last_line="(no lines yet)"
    fi
  else
    last_line="(no lines yet)"
  fi

  printf 'The last line is: %s\n' "$last_line" | tee -a "$ECHO_FILE"
  sleep 5
done

printf 'echo_lines exiting after 120 seconds.\n'
```

### py_size.py

Monitors file sizes every 10 seconds, crashes after 60s to demonstrate restart hooks:

```python
#!/usr/bin/env python3
from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Iterable

BASE_DIR = Path(__file__).resolve().parent
CRASH_MARKER = BASE_DIR / ".py_size_crash_once"
START_FILE = BASE_DIR / ".py_size_start.json"
TARGET_GLOB = "*.txt"
PRINT_INTERVAL_SECONDS = 10
FAILURE_AFTER_SECONDS = 60
EXIT_AFTER_SECONDS = 120


def load_start_epoch() -> float:
    if START_FILE.exists():
        try:
            payload = json.loads(START_FILE.read_text())
            return float(payload.get("started_at", time.monotonic()))
        except (ValueError, TypeError):
            pass
    epoch = time.monotonic()
    START_FILE.write_text(json.dumps({"started_at": epoch}))
    return epoch


def iter_target_files() -> Iterable[Path]:
    yield from sorted(BASE_DIR.glob(TARGET_GLOB))


def report_file_sizes() -> None:
    paths = list(iter_target_files())
    for path in paths:
        try:
            size = path.stat().st_size
            print(f"py_size: {path.name} -> {size} bytes")
        except FileNotFoundError:
            print(f"py_size: {path.name} -> (missing)")
    if not paths:
        print("py_size: no tracked files yet")


def main() -> None:
    start = load_start_epoch()
    while True:
        elapsed = time.monotonic() - start
        report_file_sizes()

        if elapsed >= EXIT_AFTER_SECONDS:
            if CRASH_MARKER.exists():
                CRASH_MARKER.unlink(missing_ok=True)
            if START_FILE.exists():
                START_FILE.unlink(missing_ok=True)
            print("py_size: completed monitoring window; exiting cleanly")
            return

        if not CRASH_MARKER.exists() and elapsed >= FAILURE_AFTER_SECONDS:
            CRASH_MARKER.write_text("triggered\n")
            raise RuntimeError("py_size simulated failure after 60 seconds")

        time.sleep(PRINT_INTERVAL_SECONDS)


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as exc:
        print(f"py_size: {exc}")
        raise
```

## Files

```
examples/multi-service/
├── count_number.sh
├── echo_lines.sh
├── py_size.py
└── systemg.yaml
```

All scripts assume a POSIX shell and rely only on tools that ship with macOS/Linux (plus `python3`).

## How it works

1. **`py_size`** starts immediately. It watches `*.txt` files in the directory, prints their sizes, and after the first minute raises a `RuntimeError`. `restart_policy: "always"` restarts it, the `on_restart.error` hook posts `{"message":"I goof'd"}` to `https://jsonplaceholder.typicode.com/posts`, and the second run exits with code `0` once the 120‑second window closes.
2. **`count_number`** is a cron service that runs every 10 seconds. It persists an incrementing counter so each invocation safely picks up where the previous one left off, even across restarts.
3. **`echo_lines`** depends on `count_number`. It runs for 120 seconds, asks for the latest line in `lines.txt` (falling back to a friendly placeholder), appends its own summary to `echo.txt`, and exits successfully.

Because the cron job writes `lines.txt`, the echo worker always has something to replay, and `py_size` demonstrates how a supervised process can fail, fire hooks, restart, and still terminate cleanly.

## Example Output

Here's what you'll see when running these services:

### Starting the services

```bash
$ sysg start --config systemg.yaml
Supervisor started
```

### Checking service status

```bash
$ sysg status
Service statuses:
● echo_lines Running
   Active: active (running) since 00:16; 16 secs ago
 Main PID: 67705
    Tasks: 0 (limit: N/A)
   Memory: 2.0M
      CPU: 0.010s
 Process Group: 67705
     |-67705 sh echo_lines.sh
       ├─67943 sleep 5

● [cron] count_number - Exited successfully (exit code 0)
  Cron history (local (-08:00-08:00)) for count_number:
    - 2025-12-31 15:47:10 -08:00 | exit 0
    - 2025-12-31 15:47:00 -08:00 | exit 0
    - 2025-12-31 15:41:10 -08:00 | exit 0
    - 2025-12-31 15:41:00 -08:00 | exit 0
    - 2025-12-31 15:40:50 -08:00 | exit 0

● py_size Running
   Active: active (running) since 00:17; 17 secs ago
 Main PID: 67704
    Tasks: 0 (limit: N/A)
   Memory: 8.2M
      CPU: 0.030s
 Process Group: 67704
     |-67704 python3 py_size.py
```

### Viewing logs

```bash
$ sysg logs py_size
+---------------------------------+
|         py_size (running)       |
+---------------------------------+

==> /Users/rashad/.local/share/systemg/logs/py_size_stdout.log <==
py_size: echo.txt -> 595 bytes
py_size: lines.txt -> 195 bytes
py_size: echo.txt -> 665 bytes
py_size: lines.txt -> 212 bytes
py_size: echo.txt -> 735 bytes
py_size: lines.txt -> 229 bytes
...
```

```bash
$ sysg logs echo_lines
+---------------------------------+
|      echo_lines (running)       |
+---------------------------------+

==> /Users/rashad/.local/share/systemg/logs/echo_lines_stdout.log <==
The last line is: (no lines yet)
The last line is: The number is 1
The last line is: The number is 2
The last line is: The number is 2
The last line is: The number is 3
The last line is: The number is 3
...
```

### Stopping the services

```bash
$ sysg stop
Supervisor shutting down
```

## Cleanup

The scripts tidy up their temporary markers automatically, but you can delete the generated `.count_state`, `.py_size_*`, `lines.txt`, and `echo.txt` files if you want a fresh run.
