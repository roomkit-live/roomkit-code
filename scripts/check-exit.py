"""Exercise the exit summary: resume command, session-wide usage and stopped processes."""
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import struct
import subprocess
import tempfile
import termios
import threading
import time
import uuid

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
marker = f"roomkit-exit-{uuid.uuid4().hex}"
requests = []
# One model reply per request: the first turn starts a process, then answers.
replies = [
    ("process_start", {"command": f"sleep 300 # {marker}", "_roomkit_intent": "I start the server"}),
    "Exit fixture first.",
    "Exit fixture second.",
]
usage = [
    {"prompt_tokens": 200, "completion_tokens": 20, "prompt_tokens_details": {"cached_tokens": 100}},
    {"prompt_tokens": 300, "completion_tokens": 30, "prompt_tokens_details": {"cached_tokens": 200}},
    {"prompt_tokens": 400, "completion_tokens": 40, "prompt_tokens_details": {"cached_tokens": 300}},
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
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [], "usage": usage[index]}) + "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class App:
    def __init__(self, argv, storage):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 160, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env.update({"TERM": "xterm-256color", "HOME": str(storage / "home"), "XDG_DATA_HOME": str(storage / "data")})
        self.process = subprocess.Popen(argv, stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True, env=env)
        self.output = bytearray()

    def send(self, data):
        offset = len(self.output)
        os.write(self.master, data.encode())
        return offset

    def screen(self, after=0):
        return re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.output[after:]).decode(errors="replace")

    def wait(self, text, after=0):
        deadline = time.monotonic() + 15
        while text not in self.screen(after):
            if time.monotonic() > deadline or self.process.poll() is not None:
                raise AssertionError(f"Missing {text!r}: {self.output[-6000:]!r}")
            if select.select([self.master], [], [], 0.02)[0]:
                self.output.extend(os.read(self.master, 65536))

    def quit(self):
        """Detach with Ctrl+D and return the lines printed after the view closed."""
        try:
            offset = self.send("\x04")
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if select.select([self.master], [], [], 0.02)[0]:
                    self.output.extend(os.read(self.master, 65536))
                elif self.process.poll() is not None:
                    break
            assert self.process.poll() == 0, "Clean shutdown failed"
            assert termios.tcgetattr(self.slave) == self.original
            # The report follows the terminal's restoration of the main screen.
            report = bytes(self.output[offset:]).rsplit(b"\x1b[?1049l", 1)[-1]
            text = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", report).decode(errors="replace")
            return text.replace("\r", "").splitlines()
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            os.close(self.master)
            os.close(self.slave)


def after(lines, heading):
    return lines[lines.index(heading) + 1].strip()


def alive():
    return subprocess.run(["pgrep", "-f", marker], capture_output=True).returncode == 0


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix="roomkit-exit-") as directory:
        root = Path(directory)
        project, storage = root / "project", root / "state"
        project.mkdir()
        storage.mkdir()
        url = f"http://127.0.0.1:{server.server_port}/v1"
        app = App([str(binary), str(project), "--state-dir", str(storage), "--base-url", url, "--model", "fixture",
                   "--mode", "yolo", "--color", "never", "--no-animations", "--no-herdr", "--no-terminal-status"], storage)
        app.wait("[Yolo]")
        offset = app.send("Start the server\r")
        app.wait("Exit fixture first.", offset)
        assert alive(), "The fixture process must run before exit"
        # The badge follows the exit warning: once it shows, Ctrl+D must ask first.
        app.wait("Processes: 1 running")
        # With a running process, the first Ctrl+D only states what exiting stops.
        offset = app.send("\x04")
        app.wait("Press Ctrl+D again to exit · 1 running process will be stopped", offset)
        assert app.process.poll() is None and alive(), "The first Ctrl+D must only warn"
        # F6 disarms the exit and opens the Processes panel, which states the warning too.
        offset = app.send("\x1b[17~")
        app.wait("Background processes", offset)
        offset = app.send("\x04")
        app.wait("Press Ctrl+D again to exit · 1 running process will be stopped", offset)
        assert app.process.poll() is None and alive(), "Ctrl+D over a panel must only warn"
        lines = app.quit()
        session = next(line for line in lines if line.startswith("Session: "))[len("Session: "):]
        command = shlex.split(after(lines, "To continue this session, run:"))
        assert command[0] == "roomkit-code" and command[command.index("--session") + 1] == session, lines
        assert "Token usage: total=250 input=200 (+ 300 cached) output=50 · 1 turn" in lines, lines
        assert after(lines, "Stopped 1 background process:").split(" · ")[1].startswith("sleep 300"), lines
        assert not alive(), "Closing must stop the background process"

        # The printed command reopens the same conversation; usage spans both launches.
        app = App([str(binary)] + command[1:], storage)
        app.wait("Start the server")
        offset = app.send("Continue\r")
        app.wait("Exit fixture second.", offset)
        lines = app.quit()
        assert f"Session: {session}" in lines, lines
        assert "Token usage: total=390 input=300 (+ 600 cached) output=90 · 2 turns" in lines, lines
        # Without running processes, a single Ctrl+D detaches at once.
        assert not any("background process" in line for line in lines), lines
        assert len(requests) == 3
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
print("PTY exit passed: Ctrl+D warns about running processes in the conversation and over a panel, resume command reopens the session, usage spans launches, background process stopped.")
