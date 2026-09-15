import sys
import urllib.request

tag = sys.argv[1]
try:
    with urllib.request.urlopen(
        f"http://127.0.0.1:18090/slow?ms=1500&tag={tag}", timeout=10
    ) as response:
        result = f"{response.status} {response.read().decode().split()[0]}"
except Exception as err:
    result = f"error {err!r}"
with open(f"/tmp/pinned-{tag}", "w", encoding="utf-8") as out:
    out.write(result)
