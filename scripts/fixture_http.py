"""Loopback HTTP fixtures that start independently of the host's DNS."""

from http.server import ThreadingHTTPServer
from socketserver import TCPServer


class LoopbackHTTPServer(ThreadingHTTPServer):
    """Serve local test traffic without HTTPServer's reverse hostname lookup."""

    def server_bind(self):
        TCPServer.server_bind(self)
        self.server_name = "localhost"
        self.server_port = self.server_address[1]
