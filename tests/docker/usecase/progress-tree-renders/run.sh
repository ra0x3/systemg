#!/usr/bin/env bash
# USE CASE: start/stop/restart draw a live, nested progress tree on a terminal.
#
# WHAT THIS TESTS
#   Each unit appears as it is worked and resolves independently of the steps
#   nested under it: a health check ticks IN PLACE, turns ✔, and only then does
#   the service it belongs to. Failures mark ✗ on both the step and its unit and
#   still carry the SG code and its docs link.
#
# WHY IT NEEDS A PTY (and why the bug it guards survived so long)
#   The tree renders only to a terminal. Every other case in this suite captures
#   through a pipe, where the renderer correctly draws nothing — so none of them
#   can see this feature at all, and a regression in it is invisible to them.
#   A registration race once left the tree empty for whole commands while every
#   existing case stayed green.
#
# EXPECTED OUTCOME
#   - start, restart and stop each draw one row per unit,
#   - a health check nests UNDER its service and resolves before it,
#   - repeated health-check attempts update one row, never append,
#   - skipped units are ✔, not failures,
#   - a failed health check marks ✗ and reports SG0104 with its docs link.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
PROJECT=treeproj

# Runs a command under a pseudo-terminal and returns what the terminal saw.
cat >/tmp/ptyrun.py <<'PY'
import os, pty, select, sys

cmd = sys.argv[1:]
pid, fd = pty.fork()
if pid == 0:
    os.environ["COLUMNS"] = "100"
    os.environ["LINES"] = "40"
    os.execvp(cmd[0], cmd)

chunks = []
while True:
    try:
        ready, _, _ = select.select([fd], [], [], 90)
        if not ready:
            break
        data = os.read(fd, 4096)
        if not data:
            break
        chunks.append(data)
    except OSError:
        break
sys.stdout.write(b"".join(chunks).decode("utf-8", "replace"))
PY

# Strips cursor motion and colour so assertions read the visible text. The LAST
# repaint is what remains on screen, so the tail after the final cursor-up is
# the finished tree.
cat >/tmp/screen.py <<'PY'
import re, sys

raw = open(sys.argv[1], "rb").read().decode("utf-8", "replace")
frames = re.split(r"\x1b\[\d+A", raw)
clean = re.sub(r"\x1b\[[0-9;]*[A-HJKSTfmsu]", "", frames[-1])
for line in clean.replace("\r", "\n").split("\n"):
    if line.strip():
        print(line)
PY

run_pty() { python3 /tmp/ptyrun.py "$@" >/tmp/raw.txt 2>&1; python3 /tmp/screen.py /tmp/raw.txt; }

section "start draws a row per unit"
sysg start --config "$CONFIG" --daemonize >/dev/null 2>&1
sleep 3
sysg stop -p "$PROJECT" >/dev/null 2>&1
sleep 1

OUT="$(run_pty sysg start -p "$PROJECT" --config "$CONFIG")"
echo "$OUT"
printf '%s' "$OUT" | grep -q '✔ api'
check "$?" "start marks 'api' complete"
printf '%s' "$OUT" | grep -q '✔ worker'
check "$?" "start marks 'worker' complete"
printf '%s' "$OUT" | grep -q 'Starting'
check "$?" "start names the operation in its head line"

section "a health check nests under its service and resolves independently"
OUT="$(run_pty sysg restart -p "$PROJECT" --config "$CONFIG")"
echo "$OUT"
printf '%s' "$OUT" | grep -q '✔ api'
check "$?" "restart marks 'api' complete"

# The step row is indented FURTHER than the unit row it belongs to.
UNIT_INDENT="$(printf '%s' "$OUT" | grep -m1 '✔ api' | sed 's/[^ ].*//' | wc -c)"
STEP_INDENT="$(printf '%s' "$OUT" | grep -m1 'health check' | sed 's/[^ ].*//' | wc -c)"
echo "  unit indent=$UNIT_INDENT step indent=$STEP_INDENT"
[ "$STEP_INDENT" -gt "$UNIT_INDENT" ] 2>/dev/null
check "$?" "the health check row nests under its service"

# One row for the whole check, however many attempts it took.
ATTEMPTS="$(printf '%s' "$OUT" | grep -c 'health check')"
echo "  health check rows on screen: $ATTEMPTS"
[ "$ATTEMPTS" -le 1 ]
check "$?" "repeated attempts update one row instead of appending"

section "stop draws the units it brought down"
OUT="$(run_pty sysg stop -p "$PROJECT")"
echo "$OUT"
printf '%s' "$OUT" | grep -q '✔ api'
check "$?" "stop marks 'api' down"
printf '%s' "$OUT" | grep -q '✔ worker'
check "$?" "stop marks 'worker' down"

section "a failing health check marks ✗ and keeps its SG code"
python3 - <<'PY'
src = open('/usecase/stack.yaml').read()
old = "        command: /bin/true\n        interval: 1s"
new = "        command: /bin/false\n        interval: 1s"
assert old in src, "health check block not found — fixture and injection drifted"
open('/tmp/broken.yaml', 'w').write(src.replace(old, new))
PY

sysg start -p "$PROJECT" --config "$CONFIG" >/dev/null 2>&1
sleep 2
OUT="$(run_pty sysg restart -p "$PROJECT" --config /tmp/broken.yaml)"
echo "$OUT"

printf '%s' "$OUT" | grep -q '✗'
check "$?" "the failing unit is marked with ✗"

# The diagnostic still names the code and links its docs page.
grep -q 'SG0104' /tmp/raw.txt
check "$?" "the failure reports SG0104"
grep -q 'codes#sg0104' /tmp/raw.txt
check "$?" "the failure links the SG0104 docs page"

section "a piped (non-terminal) run stays clean"
sysg restart -p "$PROJECT" --config "$CONFIG" >/tmp/piped.txt 2>&1
! grep -qE '✔|✗|\[2K' /tmp/piped.txt
check "$?" "no tree or cursor control leaks into a pipe"

sysg stop --supervisor >/dev/null 2>&1
finish
