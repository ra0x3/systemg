import os
import signal
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


port = int(os.environ.get("PORT", "18082"))
try:
    with open("/tmp/fail-slot", encoding="utf-8") as failure:
        if failure.read().strip() == str(port):
            time.sleep(0.4)
            print(f"candidate slot {port} failed", file=sys.stderr, flush=True)
            sys.exit(42)
except FileNotFoundError:
    pass


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(str(os.getpid()).encode())

    def log_message(self, format, *args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
with open(f"/tmp/pids-{port}", "a", encoding="utf-8") as output:
    output.write(f"{os.getpid()}\n")
    output.flush()
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
server.serve_forever()
