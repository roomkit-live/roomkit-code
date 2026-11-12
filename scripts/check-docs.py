"""Exercise the embedded guide through the documentation tools in an isolated PTY."""
import fcntl
from http.server import BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import threading
import time

from fixture_http import LoopbackHTTPServer

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
requests = []
# One lookup per turn: the transcript shows only the current call of
# consecutive tool calls, so each result stays on screen after its turn.
replies = [
    ("docs_search", {"query": "command hooks", "_roomkit_intent": "I search the RoomKit Code guide"}),
    "Guide search complete.",
    ("docs_read", {"document_id": "command-hooks.md", "limit": 5, "_roomkit_intent": "I read the hooks section"}),
    "Docs fixture complete.",
]


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        index = len(requests) - 1
        reply = replies[index]
        if isinstance(reply, tuple):
            name, args = reply
            delta = {"tool_calls": [{"index": 0, "id": f"call-{index}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": reply}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
                + "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class App:
    def __init__(self, project, storage, url, mode):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 160, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env.update({"TERM": "xterm-256color", "HOME": str(storage / "home"), "XDG_DATA_HOME": str(storage / "data")})
        self.process = subprocess.Popen([str(binary), str(project), "--state-dir", str(storage), "--base-url", url,
            "--model", "fixture", "--session", "docs", "--mode", mode, "--color", "never", "--no-animations",
            "--no-herdr", "--no-terminal-status"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True, env=env)
        self.output = bytearray()

    def send(self, data):
        offset = len(self.output)
        os.write(self.master, data.encode())
        return offset

    def screen(self, after=0):
        return re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.output[after:])

    def wait(self, text, after=0):
        deadline = time.monotonic() + 15
        while text.encode() not in self.screen(after):
            if time.monotonic() > deadline or self.process.poll() is not None:
                raise AssertionError(f"Missing {text!r}: {self.output[-6000:]!r}")
            if select.select([self.master], [], [], 0.02)[0]:
                self.output.extend(os.read(self.master, 65536))

    def close(self):
        try:
            self.send("\x04")
            deadline = time.monotonic() + 10
            while self.process.poll() is None and time.monotonic() < deadline:
                if select.select([self.master], [], [], 0.02)[0]:
                    self.output.extend(os.read(self.master, 65536))
            assert self.process.poll() == 0, "Clean shutdown failed"
            assert termios.tcgetattr(self.slave) == self.original
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            os.close(self.master)
            os.close(self.slave)


server = LoopbackHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    for mode, label in [("ask", "[Ask]"), ("plan", "[Plan]")]:
        requests.clear()
        with tempfile.TemporaryDirectory(prefix="roomkit-docs-") as directory:
            root = Path(directory)
            project, storage = root / "project", root / "state"
            project.mkdir()
            storage.mkdir()
            app = App(project, storage, f"http://127.0.0.1:{server.server_port}/v1", mode)
            try:
                app.wait(label)
                start = app.send("How do I configure hooks?\r")
                app.wait("guide documents match", start)
                app.wait("Guide search complete.", start)
                app.wait("Execution Completed", start)
                offset = app.send("Show me that section\r")
                app.wait("Command hooks · command-hooks.md · lines 1–5 of", offset)
                # Unchanged cells are not redrawn: wait for this turn's own answer.
                app.wait("Docs fixture complete.", offset)
                screen = app.screen(start)
                if os.environ.get("ROOMKIT_DOCS_CAPTURE"):
                    Path(os.environ["ROOMKIT_DOCS_CAPTURE"]).write_bytes(screen)
                assert b"Allow docs_" not in screen, "Guide lookups must not ask for approval"
                assert b'"data":' not in screen, "Tools must use their product presentations"
                assert len(requests) == 4
                # The model reads the results as lines: the first hit's quoted
                # ID, then the page's lines numbered as read numbers them.
                found = requests[1]["messages"][-1]["content"]
                assert '\n1. "command-hooks.md" ' in found, found
                page = requests[3]["messages"][-1]["content"]
                assert "\n1: # Command hooks\n" in page, page
                assert len(list((storage / "docs").glob("*/command-hooks.md"))) == 1
            finally:
                app.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
print("PTY docs passed: embedded guide published, searched and read without approval in Ask and Plan.")
