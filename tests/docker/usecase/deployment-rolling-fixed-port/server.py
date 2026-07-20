import os
import signal
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(str(os.getpid()).encode())

    def log_message(self, format, *args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler)
with open(sys.argv[2], "a", encoding="utf-8") as output:
    output.write(f"{os.getpid()}\n")
    output.flush()
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
server.serve_forever()
