"""Process and HTTP fixtures shared by the Redis executor checks."""

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler
import fcntl
import json
import os
import re
import pty
import select
import struct
import termios
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
        def event(frame):
            return ("data: " + json.dumps({"choices": [{"index": 0, **frame}]}) + "\n\n").encode()
        first = event({"delta": delta})
        end = event({"delta": {}, "finish_reason": reason}) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(first) + len(end)))
        try:
            self.end_headers()
            self.wfile.write(first)
            self.wfile.flush()
            self.after_delta(delta)
            self.wfile.write(end)
        except (BrokenPipeError, ConnectionResetError):
            pass  # A cancelled worker drops its model request.


    def after_delta(self, delta: dict) -> None:
        """Override to hold a stream open after the client receives its first fragment."""


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


class Terminal:
    def __init__(self, stack, args, env):
        self.master, slave = pty.openpty()
        stack.callback(os.close, self.master)
        stack.callback(os.close, slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 58, 200, 0, 0))
        self.proc = stack.enter_context(child(args, env, stdin=slave, stdout=slave,
                                              stderr=slave, start_new_session=True))
        self.output = bytearray()
        self.wait("[Plan]", 0)

    def wait(self, text, offset):
        markers = (text,) if isinstance(text, str) else text
        end = time.monotonic() + 35
        while time.monotonic() < end and self.proc.poll() is None:
            if select.select([self.master], [], [], .1)[0]:
                self.output.extend(os.read(self.master, 65536))
            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.output[offset:])
            if any(marker.encode() in plain for marker in markers):
                return
        raise AssertionError(f"Terminal did not show {text!r}: {self.output[-2800:]!r}")

    def send(self, message, expect):
        offset = len(self.output)
        os.write(self.master, message.encode() + b"\r")
        self.wait(expect, offset)
        return offset
