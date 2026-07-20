#!/usr/bin/env python3
"""A trivial HTTP server that binds a port and answers /health with 200.

Stands in for a real `serve` process. Writes its own PID to a file so the repro
can tell an orphan (survivor of a killed supervisor) apart from a fresh unit.
"""
import http.server
import os
import socketserver
import sys

port = int(sys.argv[1])
tag = sys.argv[2] if len(sys.argv) > 2 else "server"

pid_file = f"/tmp/{tag}.pid"
with open(pid_file, "w") as handle:
    handle.write(str(os.getpid()))


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *_args):
        pass


class Server(socketserver.TCPServer):
    allow_reuse_address = True


with Server(("127.0.0.1", port), Handler) as httpd:
    httpd.serve_forever()
