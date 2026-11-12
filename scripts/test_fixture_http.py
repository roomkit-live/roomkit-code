"""The local HTTP fixtures must work without any external name service."""

from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler
import threading
import unittest
from unittest.mock import patch

from fixture_http import LoopbackHTTPServer


class FixtureHttpTests(unittest.TestCase):
    def test_local_server_serves_requests_without_reverse_dns(self):
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                self.send_response(200)
                self.send_header("Content-Length", "7")
                self.end_headers()
                self.wfile.write(b"fixture")

        with patch("socket.getfqdn", side_effect=AssertionError("Unexpected DNS lookup")):
            with LoopbackHTTPServer(("127.0.0.1", 0), Handler) as server:
                self.assertEqual(server.server_name, "localhost")
                self.assertGreater(server.server_port, 0)
                thread = threading.Thread(target=server.serve_forever, daemon=True)
                thread.start()
                client = HTTPConnection("127.0.0.1", server.server_port, timeout=5)
                try:
                    client.request("GET", "/")
                    response = client.getresponse()
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.read(), b"fixture")
                finally:
                    client.close()
                    server.shutdown()
                    thread.join(timeout=5)
                self.assertFalse(thread.is_alive())


if __name__ == "__main__":
    unittest.main()
