#!/usr/bin/env python3
"""The shop web tier: a stdlib HTTP API over the real Postgres + Redis services.

No Python DB drivers are assumed — the base image ships `psql` and `redis-cli`,
so this talks to the datastores by shelling out to them. That keeps the app a
faithful client of the services sysg supervises without pulling pip packages
into the image.
"""
import os
import subprocess
from http.server import BaseHTTPRequestHandler, HTTPServer

GREETING = os.environ.get("GREETING", "hello")
PORT = int(os.environ.get("WEB_PORT", "8080"))
PG_PORT = os.environ.get("PG_PORT", "5433")
REDIS_PORT = os.environ.get("REDIS_PORT", "6380")


def db_now():
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-p", PG_PORT, "-U", "postgres",
         "-tAc", "select 1"],
        capture_output=True, text=True, timeout=5,
        env={**os.environ, "PGPASSWORD": "postgres"},
    )
    return out.returncode == 0 and out.stdout.strip() == "1"


def cache_ping():
    out = subprocess.run(
        ["redis-cli", "-p", REDIS_PORT, "ping"],
        capture_output=True, text=True, timeout=5,
    )
    return "PONG" in out.stdout


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body):
        self.send_response(code)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        self.wfile.write(body.encode())

    def do_GET(self):
        if self.path == "/health":
            ok = db_now() and cache_ping()
            self._send(200 if ok else 503, "ok" if ok else "unhealthy")
        elif self.path == "/":
            self._send(200, f"{GREETING} from shop (db+cache up)")
        else:
            self._send(404, "not found")

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
