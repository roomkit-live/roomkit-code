"""Loopback Streamable HTTP fixture; no real credentials or external services."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
from socketserver import TCPServer
import sys


class LoopbackHTTPServer(ThreadingHTTPServer):
    def server_bind(self):
        # HTTPServer normally resolves the host's name here. This fixture only
        # serves loopback and must start independently of the runner's DNS.
        TCPServer.server_bind(self)
        self.server_name = "localhost"
        self.server_port = self.server_address[1]


def start(portfile, trace, token_variable="MCP_FIXTURE_TOKEN"):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def respond(self, status, body=b"", content_type="application/json", session=False):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            if session:
                self.send_header("Mcp-Session-Id", "fixture-session")
            self.end_headers()
            self.wfile.write(body)

        def authorized(self):
            if self.headers.get("Authorization") != "Bearer " + os.environ[token_variable]:
                self.respond(401, b"fixture-private-error")
                return False
            return True

        def do_GET(self):
            self.respond(405)

        def do_DELETE(self):
            if self.authorized():
                with trace.open("a") as out:
                    out.write("delete\n")
                self.respond(204)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if not self.authorized():
                return
            method = request.get("method", "")
            if method.startswith("notifications/"):
                self.respond(202)
                return
            if method == "server/discover":
                self.respond(400)
                return
            if method == "initialize":
                result = {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}},
                          "serverInfo": {"name": "fixture", "version": "1"}}
            elif method == "tools/list":
                assert self.headers.get("Mcp-Session-Id") == "fixture-session"
                result = {"tools": [{"name": "read", "description": "Read fixture",
                                     "inputSchema": {"type": "object", "properties": {}}}]}
            elif method == "tools/call":
                assert self.headers.get("Mcp-Session-Id") == "fixture-session"
                with trace.open("a") as out:
                    out.write(request["params"]["name"] + "\n")
                result = {"content": [{"type": "text", "text": "HTTP fixture read"}]}
            else:
                result = {}
            body = json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}).encode()
            if method == "tools/call":
                self.respond(200, b"event: message\ndata: " + body + b"\n\n", "text/event-stream")
            else:
                self.respond(200, body, session=method == "initialize")

    server = LoopbackHTTPServer(("127.0.0.1", 0), Handler)
    ready = portfile.with_suffix(".ready")
    ready.write_text(str(server.server_port))
    ready.replace(portfile)
    server.serve_forever()


if __name__ == "__main__":
    start(Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3] if len(sys.argv) > 3 else "MCP_FIXTURE_TOKEN")
