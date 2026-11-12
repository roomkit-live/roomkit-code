"""Exercise hook review, invocation, output inspection and restart through a real PTY."""
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

class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": {"content": "Hook fixture complete."}}]}) + "\n\n"
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
            "--model", "fixture", "--session", "hooks", "--color", "never", "--no-animations", "--no-herdr", "--no-terminal-status"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True, env=env)
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

server = LoopbackHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix="roomkit-hooks-") as directory:
        root = Path(directory)
        project, storage = root / "project", root / "state"
        (project / ".roomkit").mkdir(parents=True)
        (project / ".roomkit" / "hooks.json").write_text(json.dumps({"hooks": {"TurnStart": [{"hooks": [{
            "type": "command", "command": "cat >> hook-events.jsonl; printf 'Checked fixture' >&2", "timeout": 2, "statusMessage": "Check fixture"
        }]}]}}))
        url = f"http://127.0.0.1:{server.server_port}/v1"
        app = App(project, storage, url)
        try:
            app.wait("[Ask]")
            app.wait("Ask about this project")
            offset = app.send("/hooks\r")
            app.wait("hooks discovered", offset)
            assert not (project / "hook-events.jsonl").exists()
            app.send(b"\x1b[B\r")  # Reload is first; choose the hook.
            app.wait("Enable this hook", offset)
            offset = app.send(b"\r")
            app.wait("State: enabled", offset)
            app.dismiss()
            offset = app.send("Check the fixture\r")
            app.wait("Hook fixture complete.", offset)
            app.wait("Execution Completed", offset)
            entries = (project / "hook-events.jsonl").read_text().splitlines()
            assert len(entries) == 1 and json.loads(entries[0])["hook_event_name"] == "TurnStart"
            offset = app.send("/hooks\r")
            app.wait("hooks discovered", offset)
            app.send(b"\x1b[B\r")
            app.wait("Checked fixture", offset)
            app.wait("completed", offset)
            if os.environ.get("ROOMKIT_HOOKS_CAPTURE"):
                Path(os.environ["ROOMKIT_HOOKS_CAPTURE"]).write_bytes(app.output)
            app.dismiss()
        finally:
            app.close()
        app = App(project, storage, url)
        try:
            app.wait("Hook fixture complete.")
            offset = app.send("/hooks\r")
            app.wait("1 enabled", offset)
            assert len((project / "hook-events.jsonl").read_text().splitlines()) == 1
            assert len(requests) == 1
            app.dismiss()
        finally:
            app.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
print("PTY hooks passed: explicit review, execution, output inspection, retained trust and no replay on restart.")
