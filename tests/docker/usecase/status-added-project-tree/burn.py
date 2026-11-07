import subprocess
import sys
import time

depth = int(sys.argv[1])
if depth < 3:
    subprocess.Popen(["/usr/bin/python3", __file__, str(depth + 1)])
if depth == 1:
    while True:
        pass
time.sleep(3000)
