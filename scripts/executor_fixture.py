"""Process and HTTP fixtures shared by the Redis executor checks."""

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler
import json
import socket
import subprocess
import time


def tool(name, call_id, **arguments):
    return {"tool_calls": [{"index": 0, "id": call_id, "type": "function",
                            "function": {"name": name, "arguments": json.dumps(arguments)}}]}


class StreamingModel(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        delta = self.reply(request)
        reason = "tool_calls" if "tool_calls" in delta else "stop"
        frames = [{"delta": delta}, {"delta": {}, "finish_reason": reason}]
        body = "".join("data: " + json.dumps({"choices": [{"index": 0, **frame}]})
                       + "\n\n" for frame in frames) + "data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body.encode())))
        try:
            self.end_headers()
            self.wfile.write(body.encode())
        except (BrokenPipeError, ConnectionResetError):
            pass  # A cancelled worker drops its model request.


def wait_for_redis(port):
    """Docker can publish its port before Redis accepts commands through it."""
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1) as connection:
                connection.sendall(b"PING\r\n")
                with connection.makefile("rb") as response:
                    if response.readline(32) == b"+PONG\r\n":
                        return
        except OSError:
            pass
        time.sleep(.1)
    raise RuntimeError("The test Redis did not answer PING on its published port")


@contextmanager
def redis():
    container = subprocess.check_output(["docker", "run", "--rm", "-d", "-p",
                                         "127.0.0.1::6379", "redis:7-alpine"], text=True).strip()
    try:
        port = subprocess.check_output(["docker", "port", container, "6379/tcp"],
                                       text=True).strip().rsplit(":", 1)[1]
        wait_for_redis(int(port))
        yield f"redis://127.0.0.1:{port}"
    finally:
        subprocess.run(["docker", "stop", "-t", "1", container], check=True,
                       stdout=subprocess.DEVNULL)


@contextmanager
def child(args, env, **kwargs):
    proc = subprocess.Popen(args, env=env, **kwargs)
    try:
        yield proc
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
