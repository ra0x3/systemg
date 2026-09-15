import os
import signal
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PORT = int(os.environ["PORT"])


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/slow":
            query = parse_qs(url.query)
            tag = query.get("tag", [""])[0]
            if tag:
                with open(f"/tmp/accepted-{tag}", "w", encoding="utf-8") as marker:
                    marker.write(str(os.getpid()))
            time.sleep(int(query.get("ms", ["1500"])[0]) / 1000)
        body = f"{os.getpid()} {PORT}".encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass


class Server(ThreadingHTTPServer):
    daemon_threads = False


time.sleep(1.5)
server = Server(("127.0.0.1", PORT), Handler)
signal.signal(signal.SIGTERM, lambda *_: threading.Thread(target=server.shutdown).start())
server.serve_forever()
server.server_close()
