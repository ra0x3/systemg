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
