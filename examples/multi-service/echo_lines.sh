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
