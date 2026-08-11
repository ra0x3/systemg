#!/usr/bin/env bash
# PARITY: inspect must expose the same JSON field surface in user mode and
# system mode (--sys) for the same unit.
#
# HARD INVARIANTS
#   - inspect -s web parses as JSON in both lanes,
#   - the sorted top-level field set is identical across lanes,
#   - both lanes report a live pid for the unit.
set -u
. /usecase/lib.sh
. /usecase/parity-lib.sh

CONFIG=/usecase/stack.yaml

for MODE in user sys; do
  mode_begin "$MODE"

  msysg start --config "$CONFIG" --daemonize
  check "$?" "[$MODE] start exits 0"
  sleep 3

  msysg inspect --service web --format json --config "$CONFIG" > /tmp/inspect.$MODE.json 2>/tmp/inspect.err
  check "$?" "[$MODE] inspect -s web exits 0"

  python3 - /tmp/inspect.$MODE.json <<'PY' > /tmp/shape.$MODE
import json,sys
data=json.loads(open(sys.argv[1]).read())
node=data[0] if isinstance(data,list) and data else data
print("\n".join(sorted(node.keys())))
PY
  [ -s /tmp/shape.$MODE ]
  check "$?" "[$MODE] inspect JSON parsed with fields"

  mode_end
done

section "field surface matches across modes"
shapes_match
check "$?" "user and sys lanes exposed identical inspect fields"

finish
