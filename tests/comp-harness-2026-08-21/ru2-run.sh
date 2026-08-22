#!/bin/bash
# Host-side driver for ru2.sh. One container per (mode, N); results appended to
# raw/ru2-<mode>-<N>.txt. Memory is measured per container, so parallel runs do
# not contaminate each other's PSS.
H="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$H/raw"
DURATION="${DURATION:-600}"
for N in 1 10 40; do
  for MODE in bare sysg supervisor; do
    OUT="$H/raw/ru2-$MODE-$N.txt"
    docker run --rm -v "$H:/h" sysg-bench bash -c "
      set -e
      if [ '$MODE' = sysg ]; then
        curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev | sh >/dev/null 2>&1
      elif [ '$MODE' = supervisor ]; then
        pip install --quiet --break-system-packages supervisor >/dev/null 2>&1
      fi
      bash /h/ru2.sh $MODE $N $DURATION
    " > "$OUT" 2>"$OUT.err" &
  done
done
wait
echo "ru2 complete"
cat "$H"/raw/ru2-*.txt
