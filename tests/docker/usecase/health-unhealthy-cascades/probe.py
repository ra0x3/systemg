import http.server, os, sys

FLAG = "/tmp/display.ok"

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        code = 200 if os.path.exists(FLAG) else 503
        self.send_response(code)
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *args):
        pass

open(FLAG, "w").close()
http.server.HTTPServer(("127.0.0.1", 18080), H).serve_forever()
