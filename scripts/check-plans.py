"""Exercise persistent plans, live UI and mode cycling with a local model and PTY."""

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

metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
# roomkit-tui ignores answers until a card has been shown for 500 ms.
ARMING = 0.6
requests = []
plan_id = None
steps = [
    {"step_id": "inspect", "description": "Inspect the project", "status": "completed"},
    {"step_id": "implement", "description": "Implement the change", "status": "pending"},
    {"step_id": "test", "description": "Run the checks", "status": "pending"},
]


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        global plan_id
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(request)
        index = len(requests)
        call = None
        if index == 1:
            assert "Application mode: Plan" in request["messages"][-1]["content"]
            call = ("plan_create", {"title": "A checked project change", "objective": "Improve the project and verify it.", "steps": steps})
        elif index == 2:
            # The model reads the plan as lines, its ID first.
            plan_id = request["messages"][-1]["content"].split()[0]
            answer = "Plan prepared. Switch mode and request implementation when ready."
        elif index == 3:
            assert any("Application mode: Ask" in str(message.get("content")) for message in request["messages"]), request["messages"]
            active = [dict(step) for step in steps]
            active[1]["status"] = "in_progress"
            call = ("plan_update", {"plan_id": plan_id, "expected_revision": 1, "steps": active})
        elif index == 4:
            call = ("write", {"path": "approved.txt", "content": "Checked fixture change"})
        elif index == 5:
            done = [dict(step, status="completed") for step in steps]
            call = ("plan_update", {"plan_id": plan_id, "expected_revision": 2, "steps": done})
        else:
            answer = "Plan fixture complete."
        if call:
            name, args = call
            args["_roomkit_intent"] = "I update the project plan" if name.startswith("plan_") else "I apply the approved change"
            delta = {"tool_calls": [{"index": 0, "id": f"call-{index}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": answer}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
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
        env = {key: value for key, value in os.environ.items() if not key.startswith(("ROOMKIT_", "HERDR_"))}
        env.update({"TERM": "xterm-256color", "HOME": str(storage / "home"), "XDG_DATA_HOME": str(storage / "data")})
        self.process = subprocess.Popen(
            [str(binary), str(project), "--state-dir", str(storage), "--base-url", url,
             "--model", "fixture", "--session", "planning", "--color", "never", "--no-animations",
             "--no-herdr", "--no-terminal-status"], stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True, env=env)
        self.output = bytearray()

    def send(self, text):
        os.write(self.master, text.encode() if isinstance(text, str) else text)

    def wait(self, text, after=0):
        deadline = time.monotonic() + 15
        while text.encode() not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.output[after:]):
            if time.monotonic() > deadline or self.process.poll() is not None:
                raise AssertionError(f"Missing {text!r}: {self.output[-5000:]!r}")
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


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix="roomkit-plans-") as directory:
        root = Path(directory)
        project, storage = root / "project", root / "state"
        project.mkdir()
        url = f"http://127.0.0.1:{server.server_port}/v1"
        app = App(project, storage, url)
        try:
            app.wait("[Ask]")
            app.send("Prepare a project plan")
            app.wait("Prepare a project plan")
            app.send(b"\x1b[Z")
            app.wait("[Plan]")
            assert not requests, "Changing mode must not execute a turn"
            app.send(b"\r")
            app.wait("Plan prepared.")
            app.wait("1/3 completed")
            assert requests[0]["messages"][-2]["content"] == "Prepare a project plan", "Mode cycling must preserve the draft"
            offset = len(app.output)
            app.send("/plans\r")
            app.wait("Choose a plan to inspect", offset)
            app.send(b"\r")
            app.wait("Progress is reported by the agent", offset)
            app.wait("Run the checks", offset)
            offset = len(app.output)
            app.send(b"\x1b")
            app.wait("[Plan]", offset)
            offset = len(app.output)
            app.send("!touch denied-manual\r")
            app.wait("Shell commands are disabled in Plan mode", offset)
            assert not (project / "denied-manual").exists()
            # Clear a refused command if the editor retained it.
            app.send(b"\x15")
            app.send(b"\x1b[Z")
            app.wait("[Yolo]", offset)
            app.send(b"\x1b[Z")
            app.wait("[Ask]", offset)
            assert len(requests) == 2
            app.send("Implement the plan\r")
            app.wait("Allow write?", offset)
            app.wait("Implement the change", offset)
            assert not (project / "approved.txt").exists()
            time.sleep(ARMING)
            app.send(b"\x19")  # Ctrl+Y: approve this exact write.
            app.wait("Plan fixture complete.", offset)
            app.wait("3/3 completed", offset)
            assert (project / "approved.txt").read_text() == "Checked fixture change"
        finally:
            app.close()
        count = len(requests)
        app = App(project, storage, url)
        try:
            app.wait("3/3 completed")
            app.send("/plans\r")
            app.wait("Choose a plan to inspect")
            app.send(b"\r")
            app.wait("Progress is reported by the agent")
            app.wait("3/3 completed")
            assert len(requests) == count, "Resuming and browsing must not reexecute tools"
            offset = len(app.output)
            app.send(b"\x1b")
            app.wait("[Ask]", offset)
        finally:
            app.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY plans passed: mode cycle, preserved draft, live progress, detail view, approvals and restart.")
