#!/usr/bin/env bash
# USE CASE: a project added to a running supervisor shows its process tree.
#
# WHAT THIS TESTS
#   alpha boots the supervisor with the default summary snapshot mode. beta is
#   registered afterwards with `status.snapshot_mode: detailed`. Its `tree`
#   unit runs python3 -> sh -> sleep (under sysg's shell wrapper on Linux), and
#   its `burner` unit runs four nested /usr/bin/python3 children, one of them
#   spinning a CPU.
#
# HARD INVARIANTS
#   - cached `status -p beta` carries the tree (beta's own mode, not alpha's),
#   - beta's unit row has CPU/RSS metrics, like its children do,
#   - the text table draws sleep one level under sh,
#   - a busy child reports real CPU, not the 0% of a fresh process table,
#   - a child started by full path is named by its file name (`python3`),
#   - every row of a 4-deep tree still shows its name in a squeezed table,
#   - `--live` refuses with --format or without a TTY and points at --no-cache,
#   - `--no-cache` reads once, and a bare `--stream` keeps redrawing.
set -u
. /usecase/lib.sh

section "boot alpha (summary) then register beta (detailed)"
sysg start --config /usecase/alpha.yaml --daemonize
check "$?" "start -c alpha.yaml exits 0"
sleep 2
sysg start --config /usecase/beta.yaml --daemonize
check "$?" "start -c beta.yaml exits 0"
sleep 6

S="$(sysg status -p beta --format json 2>/dev/null)"
echo "$S" | python3 -c '
import json, sys
d = json.load(sys.stdin)
u = [u for u in d["units"] if u["name"] == "tree"][0]
def show(n, i):
    c = n["child"]
    print(" " * i + "%s %s %s" % (c["pid"], c["kind"], c["command"][:60]))
    for k in n.get("children", []):
        show(k, i + 2)
print("tree pid", (u.get("process") or {}).get("pid"), "metrics", u.get("metrics"))
for n in u.get("spawned_children", []):
    show(n, 2)
'

section "cached status uses beta's own detailed mode"
echo "$S" | python3 -c '
import json, sys
d = json.load(sys.stdin)
u = [u for u in d["units"] if u["name"] == "tree"][0]
def walk(nodes):
    for n in nodes:
        yield n
        yield from walk(n.get("children", []))
sh = [n for n in walk(u.get("spawned_children", [])) if n["child"]["command"].startswith("sh -c sleep")]
assert sh, "no sh child"
assert any(k["child"]["command"].startswith("sleep") for k in sh[0].get("children", [])), "sleep not under sh"
'
check "$?" "cached status nests sleep under sh in the unit tree"

section "the unit row has its own metrics"
echo "$S" | python3 -c '
import json, sys
d = json.load(sys.stdin)
u = [u for u in d["units"] if u["name"] == "tree"][0]
m = u.get("metrics") or {}
assert m.get("latest_rss_bytes", 0) > 0, m
'
check "$?" "beta's unit row reports RSS"

A="$(sysg status -p alpha --format json 2>/dev/null)"
echo "$A" | python3 -c '
import json, sys
d = json.load(sys.stdin)
u = [u for u in d["units"] if u["name"] == "alpha_svc"][0]
assert (u.get("metrics") or {}).get("latest_rss_bytes", 0) > 0
'
check "$?" "alpha's unit row still reports RSS"

section "text table keeps the nesting"
T="$(sysg status -p beta --no-color 2>/dev/null)"
echo "$T"
echo "$T" | grep -qE '[║│] {4}└─ '
check "$?" "grandchild row is indented under its parent"

section "child processes report real CPU and short names"
for flag in "" "--no-cache"; do
  sysg status -p beta $flag --format json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
u = [u for u in d["units"] if u["name"] == "burner"][0]
def walk(nodes):
    for n in nodes:
        yield n
        yield from walk(n.get("children", []))
kids = [n["child"] for n in walk(u.get("spawned_children", []))]
for c in kids:
    print("  %s name=%r cpu=%s :: %s" % (c["pid"], c["name"], c.get("cpu_percent"), c["command"][:40]))
busy = [c for c in kids if c["command"].startswith("/usr/bin/python3") and c["command"].endswith(" 1")]
assert busy and (busy[0].get("cpu_percent") or 0) > 20, busy
full = [c for c in kids if c["command"].startswith("/usr/bin/python3")]
assert len(full) == 3 and all(c["name"] == "python3" for c in full), full
'
  check "$?" "busy child shows real CPU and full-path children are named python3 (${flag:-cached})"
done

section "a 4-deep tree keeps every name in a squeezed table"
B="$(sysg status -p beta --no-color 2>/dev/null)"
echo "$B" | grep -E 'burner|python3'
[ "$(echo "$B" | grep -cE '└─ python3 ')" -ge 4 ]
check "$?" "all four python3 rows show their name"

section "--live refuses where it cannot redraw"
OUT="$(sysg status --live --format json 2>&1)"
RC=$?
echo "$OUT"
[ "$RC" -ne 0 ] && echo "$OUT" | grep -q -- '--no-cache'
check "$?" "status --live --format json refuses and points at --no-cache"
OUT="$(sysg status --live 2>&1 </dev/null)"
RC=$?
[ "$RC" -ne 0 ] && echo "$OUT" | grep -q 'not a terminal'
check "$?" "status --live without a TTY refuses"
OUT="$(sysg inspect -s tree -p beta --live 2>&1)"
RC=$?
[ "$RC" -ne 0 ] && echo "$OUT" | grep -q -- 'sysg inspect -s tree -p beta --no-cache'
check "$?" "inspect --live without a TTY refuses and points at --no-cache"

section "--no-cache reads once"
sysg status --no-cache --format json 2>/dev/null | python3 -c 'import json,sys; json.load(sys.stdin)'
check "$?" "status --no-cache --format json returns one document"

section "a bare --stream keeps redrawing every second"
timeout 4 sysg status -p beta --stream --no-color >/tmp/stream.out 2>&1
RC=$?
FRAMES="$(grep -c 'UNIT' /tmp/stream.out)"
echo "rc=$RC frames=$FRAMES"
[ "$RC" -eq 124 ] && [ "$FRAMES" -ge 2 ]
check "$?" "status --stream with no value keeps redrawing"
timeout 3 sysg inspect -s tree -p beta --stream >/dev/null 2>&1
[ "$?" -eq 124 ]
check "$?" "inspect --stream with no value is accepted"

sysg stop --supervisor >/dev/null 2>&1
finish
