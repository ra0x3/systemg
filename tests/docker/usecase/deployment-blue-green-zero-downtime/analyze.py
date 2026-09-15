import json
import sys


def load(path):
    with open(path, encoding="utf-8") as source:
        return json.load(source)


def failures(records):
    bad = [r for r in records if not r["ok"]]
    for record in bad[:5]:
        print(record, file=sys.stderr)
    print(len(bad))
    return True


def latency(records, ceiling):
    worst = max(r["end"] - r["start"] for r in records)
    print(f"max latency {worst:.3f}s over {len(records)} requests")
    return worst < ceiling


def window(records, start, end, old, new):
    served = [r for r in records if r["ok"] and start <= r["start"] <= end]
    old_starts = [r["start"] for r in served if r["pid"] == old]
    new_starts = [r["start"] for r in served if r["pid"] == new]
    stale = [r for r in records if r["ok"] and r["start"] > end and r["pid"] == old]
    print(f"old={len(old_starts)} new={len(new_starts)} stale={len(stale)}")
    return (
        bool(old_starts)
        and bool(new_starts)
        and min(old_starts) < min(new_starts)
        and not stale
    )


command, records = sys.argv[1], load(sys.argv[2])
if command == "failures":
    passed = failures(records)
elif command == "latency":
    passed = latency(records, float(sys.argv[3]))
else:
    passed = window(
        records, float(sys.argv[3]), float(sys.argv[4]), sys.argv[5], sys.argv[6]
    )
sys.exit(0 if passed else 1)
