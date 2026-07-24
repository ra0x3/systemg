import os
import pty
import select
import sys
import time
from pathlib import Path

mode = sys.argv[1]
command = sys.argv[2:]
pid, fd = pty.fork()

if pid == 0:
    os.execvp(command[0], command)

captured = bytearray()
started = time.monotonic()
emitted = False

while time.monotonic() - started < 6:
    if mode in ("follow", "service") and not emitted and time.monotonic() - started > 1:
        Path("/usecase/emit").touch()
        emitted = True
    ready, _, _ = select.select([fd], [], [], 0.1)
    if ready:
        try:
            captured.extend(os.read(fd, 65536))
        except OSError:
            break
    if mode in ("follow", "service") and b"FOLLOW_TWO" in captured:
        break
    if mode == "stream" and captured.count(b"\x1b[2J\x1b[H") >= 3:
        break

try:
    os.write(fd, b"\x03")
except OSError:
    pass

deadline = time.monotonic() + 2
while time.monotonic() < deadline:
    waited, _ = os.waitpid(pid, os.WNOHANG)
    if waited == pid:
        break
    time.sleep(0.05)
else:
    os.kill(pid, 9)
    os.waitpid(pid, 0)

if mode in ("follow", "service"):
    valid = b"FOLLOW_TWO" in captured and b"\r\nxxxxxxxx" not in captured
    for marker in (b"FOLLOW_ONE", b"FOLLOW_TWO"):
        offset = captured.find(marker)
        end = captured.find(b"\n", offset)
        valid = valid and offset >= 0 and end > offset and captured[end - 1] == 13
    if mode == "service":
        valid = valid and captured.count(b"OLD_") == 150
else:
    valid = (
        b"CURRENT_C" in captured
        and captured.count(b"\x1b[2J\x1b[H") >= 2
    )

sys.stdout.buffer.write(captured)
sys.exit(0 if valid else 1)
