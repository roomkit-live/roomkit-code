"""Write to the agent while it works (RMK-1230), on a real pseudo-terminal.

The local Chat Completions fixture holds the agent's first answer, a call to
read a file, open while the person types: the message goes to the running
turn, the terminal says so, and the model reads it at its next step.
"""

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
    ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True
))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
STEERED = "Look in the second ledger"
requests = []
release = threading.Event()


class Model(BaseHTTPRequestHandler):
    """The first answer waits for `release` before its call; the second answers."""

    def log_message(self, *args):
        pass

    def chunk(self, delta, finish=None):
        choice = {"index": 0, "delta": delta, "finish_reason": finish}
        self.wfile.write(b"data: " + json.dumps({"choices": [choice]}).encode() + b"\n\n")
        self.wfile.flush()

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        first = len(requests) == 1
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        if first:
            self.chunk({"content": "Checking the record. "})
            release.wait(30)
            call = {"index": 0, "id": "read-1", "type": "function",
                    "function": {"name": "read", "arguments": json.dumps({"path": "record.txt"})}}
            self.chunk({"tool_calls": [call]})
            self.chunk({}, "tool_calls")
        else:
            self.chunk({"content": "The record is in the second ledger."})
            self.chunk({}, "stop")
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


def said(request, text):
    """Whether the model was asked with `text` in any of its messages."""
    return any(text in json.dumps(message) for message in request["messages"])


def scenario(project, state, url):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
    original = termios.tcgetattr(slave)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    process = subprocess.Popen(
        [str(binary), str(project), "--state-dir", str(state), "--base-url", url,
         "--model", "fixture", "--color", "always", "--no-animations"],
        stdin=slave, stdout=slave, stderr=slave, start_new_session=True, env=env,
    )
    captured = bytearray()

    def plain(after):
        return re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:])

    def wait_for(marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in plain(after):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}; terminal: {bytes(captured[-3500:])!r}")
            if select.select([master], [], [], 0.1)[0]:
                captured.extend(os.read(master, 65536))

    try:
        wait_for(b"[Ask]")
        os.write(master, b"Find the record\r")
        wait_for(b"Checking the record.")
        offset = len(captured)
        os.write(master, STEERED.encode() + b"\r")
        wait_for("↳ sent to the running turn".encode(), offset)
        release.set()
        wait_for(b"The record is in the second ledger.", offset)
        wait_for(b"Execution Completed", offset)
        assert len(requests) == 2, f"{len(requests)} model requests, not 2: the message took a turn of its own"
        assert said(requests[1], STEERED), "The turn's next step did not read the message"
        assert said(requests[1], "ledger 2"), "The turn's call did not run"
        assert b"A request is still executing" not in plain(0), "The message was refused"
        os.write(master, b"\x04")
        assert process.wait(timeout=10) == 0
        assert termios.tcgetattr(slave) == original
    finally:
        release.set()
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        project = root / "project"
        project.mkdir()
        (project / "record.txt").write_text("Record 42: ledger 2\n")
        scenario(project, root / "state", f"http://127.0.0.1:{server.server_port}/v1")
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY steering passed: a message typed during the agent's turn goes to that turn, which reads it at its next step.")
