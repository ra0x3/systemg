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
