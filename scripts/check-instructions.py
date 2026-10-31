"""Exercise scoped instruction inspection and prompt delivery through an isolated PTY."""
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
requests = []

class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": {"content": "Instruction fixture complete."}}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
                + "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

class App:
    def __init__(self, project, storage, url):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 160, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env.update({"TERM": "xterm-256color", "HOME": str(storage / "home"), "XDG_DATA_HOME": str(storage / "data")})
        self.process = subprocess.Popen([str(binary), str(project), "--state-dir", str(storage), "--base-url", url,
            "--model", "fixture", "--session", "instructions", "--color", "never", "--no-animations", "--no-herdr", "--no-terminal-status"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env)
        self.output = bytearray()

    def send(self, data):
        offset = len(self.output)
        os.write(self.master, data.encode() if isinstance(data, str) else data)
        return offset

    def wait(self, text, after=0):
        deadline = time.monotonic() + 15
        while text.encode() not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.output[after:]):
            if time.monotonic() > deadline or self.process.poll() is not None:
                raise AssertionError(f"Missing {text!r}: {self.output[-6000:]!r}")
            if select.select([self.master], [], [], 0.02)[0]:
                self.output.extend(os.read(self.master, 65536))

    def close(self):
        try:
            self.send(b"\x04")
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

    def dismiss(self):
        offset = self.send(b"\x1b")
        # Wait for standalone Esc to be decoded before sending more bytes;
        # otherwise terminal input correctly treats Esc + a character as Alt.
        self.wait("Ask about this project", offset)

server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix="roomkit-instructions-") as directory:
        root = Path(directory)
        project, storage = root / "project", root / "state"
        (project / "src").mkdir(parents=True)
        storage.mkdir()
        (storage / "AGENTS.md").write_text("GLOBAL_INSTRUCTIONS")
        (project / "AGENTS.md").write_text("ROOT_INSTRUCTIONS")
        (project / "src/AGENTS.md").write_text("SCOPED_INSTRUCTIONS")
        url = f"http://127.0.0.1:{server.server_port}/v1"
        app = App(project, storage, url)
        try:
            app.wait("[Ask]")
            offset = app.send("/instructions\r")
            app.wait("GLOBAL_INSTRUCTIONS", offset)
            app.wait("ROOT_INSTRUCTIONS", offset)
            assert b"SCOPED_INSTRUCTIONS" not in app.output[offset:]
            assert not requests
            app.dismiss()
            offset = app.send("/instructions src/new.rs\r")
            app.wait("SCOPED_INSTRUCTIONS", offset)
            if os.environ.get("ROOMKIT_INSTRUCTIONS_CAPTURE"):
                Path(os.environ["ROOMKIT_INSTRUCTIONS_CAPTURE"]).write_bytes(app.output)
            app.dismiss()
            offset = app.send("Inspect this project\r")
            app.wait("Instruction fixture complete.", offset)
            app.wait("Execution Completed", offset)
            assert len(requests) == 1
            sent = json.dumps(requests[0])
            assert "GLOBAL_INSTRUCTIONS" in sent and "ROOT_INSTRUCTIONS" in sent
            assert "SCOPED_INSTRUCTIONS" not in sent
            offset = app.send("/instructions ../outside\r")
            app.wait("Instruction targets must be relative", offset)
            app.dismiss()
            assert len(requests) == 1
        finally:
            app.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
print("PTY instructions passed: scoped source inspection, first-turn context, invalid paths and terminal restoration.")
