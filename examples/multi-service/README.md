# Multi-Service Example

This example demonstrates systemg's capability to manage multiple services simultaneously with different behaviors:

- **py_size**: A continuously running Python service that monitors directory sizes
- **count_number**: A cron-scheduled task that increments a counter every 10 seconds
- **echo_lines**: A service that depends on count_number and echoes lines periodically

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

## Services

### py_size
- **Type**: Long-running service
- **Command**: `python3 py_size.py`
- **Restart Policy**: Always restart on failure
- **Backoff**: 5 seconds between restart attempts
- **Hooks**: Sends HTTP POST to a test endpoint on restart errors

### count_number
- **Type**: Cron job (every 10 seconds)
- **Command**: `sh count_number.sh`
- **Schedule**: Every 10 seconds (`*/10 * * * * *`)
- **Restart Policy**: Never restart
- **Function**: Increments and saves a counter to `.count_state`

### echo_lines
- **Type**: Dependent service
- **Command**: `sh echo_lines.sh`
- **Depends On**: count_number
- **Restart Policy**: Never restart
- **Function**: Echoes lines from a file with delays

## Usage

### Start all services in the foreground:
```bash
sysg start
```

### Start all services as daemon:
```bash
sysg start --daemonize
```

### Check service status:
```bash
sysg status
```

### Expected Status Output:
```
Service statuses:
● [cron] count_number - Exited successfully (exit code 0)
  Cron history (UTC) for count_number:
    - 2025-12-07 19:57:40 UTC | exit 0
    - 2025-12-07 19:57:30 UTC | exit 0
    - 2025-12-07 19:57:20 UTC | exit 0

● echo_lines Running
   Active: active (running) since 00:39; 39 secs ago
 Main PID: 36275
    Tasks: 0 (limit: N/A)
   Memory: 2.7M
      CPU: 0.020s
 Process Group: 36275
     |-36275 sh echo_lines.sh
       ├─37305 sleep 5

● py_size Running
   Active: active (running) since 00:39; 39 secs ago
 Main PID: 36260
    Tasks: 0 (limit: N/A)
   Memory: 13.1M
      CPU: 0.030s
 Process Group: 36260
     |-36260 /opt/homebrew/Cellar/python@3.13/.../Python py_size.py
```

### View supervisor and service logs:
```bash
sysg logs
```

### Expected Logs Output:
```
+---------------------------------+
|           Supervisor            |
+---------------------------------+

2025-12-07T19:57:20.522297Z  INFO systemg::supervisor: Running cron job 'count_number'
2025-12-07T19:57:20.522762Z DEBUG systemg::daemon: Initializing daemon...
2025-12-07T19:57:20.522815Z  INFO systemg::daemon: Starting service: count_number
2025-12-07T19:57:20.527214Z DEBUG systemg::daemon: Service 'count_number' started with PID: 35763
2025-12-07T19:57:20.580630Z  INFO systemg::supervisor: Cron job 'count_number' completed successfully

2025-12-07T19:57:30.563509Z  INFO systemg::supervisor: Running cron job 'count_number'
2025-12-07T19:57:30.566047Z DEBUG systemg::daemon: Service 'count_number' started with PID: 35824
2025-12-07T19:57:30.621800Z  INFO systemg::supervisor: Cron job 'count_number' completed successfully
```

### Stop all services:
```bash
sysg stop
```

## Files Generated

- `.count_state`: Stores the current counter value
- `.count_start`: Stores the initial counter value
- `echo.txt`: Output from echo_lines service
- `lines.txt`: Input data for echo_lines service
