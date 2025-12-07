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
| `count_number`| Cron (10 s) | Appends "The number is *n*" to `lines.txt`, keeping a simple counter on disk. |
| `echo_lines`  | Worker      | Tails the newest line from `lines.txt` every 5 seconds and mirrors it into `echo.txt` while streaming friendly output. |

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

## Cleanup

The scripts tidy up their temporary markers automatically, but you can delete the generated `.count_state`, `.py_size_*`, `lines.txt`, and `echo.txt` files if you want a fresh run.
