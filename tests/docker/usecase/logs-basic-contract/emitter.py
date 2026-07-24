import time
from pathlib import Path

root = Path("/usecase")

if (root / "current").exists():
    print("CURRENT_A", flush=True)
    print(f"CURRENT_LONG {'y' * 240}", flush=True)
    print("CURRENT_C", flush=True)
else:
    payload = "x" * 2048
    for index in range(1, 151):
        print(f"OLD_{index:03} {payload}", flush=True)

while not (root / "emit").exists():
    time.sleep(0.05)

print("FOLLOW_ONE", flush=True)
print("FOLLOW_TWO", flush=True)
time.sleep(3000)
