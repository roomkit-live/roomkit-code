"""Write to the agent while it works (RMK-1230), on a real pseudo-terminal.

The local Chat Completions fixture holds the agent's first answer, a call to
read a file, open while the person types:

- Enter: the message goes to the running turn, the terminal says so, and the
  model reads it at its next step, as it reads a mode changed meanwhile, even
  sent before the model's first word (RMK-1317);
- Ctrl+S, or Ctrl+Enter where the terminal reports it: the turn is cut, and the
  message leaves once it stopped, in a turn of its own. On an empty composer, it
  cuts the turn for the message already handed to it (RMK-1311).
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
CTRL_S = b"\x13"
CTRL_ENTER = b"\x1b[13;5u"
requests = []
release = threading.Event()
silent = threading.Event()


class Model(BaseHTTPRequestHandler):
    """The first answer waits for `release` before its call; the others answer."""

    def log_message(self, *args):
        pass

    def chunk(self, delta, finish=None):
        choice = {"index": 0, "delta": delta, "finish_reason": finish}
        self.wfile.write(b"data: " + json.dumps({"choices": [choice]}).encode() + b"\n\n")
        self.wfile.flush()

    def answer(self, first):
        if first:
            if silent.is_set():
                release.wait(30)
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

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        first = len(requests) == 1
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        try:
            self.answer(first)
        except (BrokenPipeError, ConnectionResetError):
            pass  # An interrupted turn stopped reading its answer.


def said(request, text):
    """Whether the model was asked with `text` in any of its messages."""
    return any(text in json.dumps(message) for message in request["messages"])


class Terminal:
    """The application on a pseudo-terminal, against the fixture, its first
    turn holding its answer."""

    def __init__(self, root, url):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        project = root / "project"
        project.mkdir()
        (project / "record.txt").write_text("Record 42: ledger 2\n")
        self.process = subprocess.Popen(
            [str(binary), str(project), "--state-dir", str(root / "state"), "--base-url", url,
             "--model", "fixture", "--color", "always", "--no-animations"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True, env=env,
        )
        self.captured = bytearray()

    def plain(self, after=0):
        return re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.captured[after:])

    def wait_for(self, marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in self.plain(after):
            if time.monotonic() >= deadline or self.process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}; terminal: {bytes(self.captured[-3500:])!r}")
            if select.select([self.master], [], [], 0.1)[0]:
                self.captured.extend(os.read(self.master, 65536))

    def send(self, data):
        os.write(self.master, data)

    def working(self):
        self.wait_for(b"[Ask]")
        self.send(b"Find the record\r")
        self.wait_for(b"Checking the record.")
        return len(self.captured)

    def exits(self):
        self.send(b"\x04")
        deadline = time.monotonic() + 10
        while self.process.poll() is None:
            assert time.monotonic() < deadline, "The application did not exit"
            if select.select([self.master], [], [], 0.05)[0]:
                self.captured.extend(os.read(self.master, 65536))
        assert self.process.returncode == 0
        assert termios.tcgetattr(self.slave) == self.original

    def close(self):
        release.set()
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        os.close(self.master)
        os.close(self.slave)


def handed_to_the_running_turn(terminal):
    offset = terminal.working()
    # A mode changed now reaches the turn too. The message waits for the mode
    # command to return, so once it is handed over, the turn has been told.
    terminal.send(b"\x1b[Z")  # Shift+Tab
    terminal.wait_for(b"[Plan]", offset)
    terminal.send(STEERED.encode() + b"\r")
    terminal.wait_for("↳ sent to the running turn".encode(), offset)
    release.set()
    terminal.wait_for(b"The record is in the second ledger.", offset)
    terminal.wait_for(b"Execution Completed", offset)
    assert len(requests) == 2, f"{len(requests)} model requests, not 2: the message took a turn of its own"
    assert said(requests[1], STEERED), "The turn's next step did not read the message"
    assert said(requests[1], "ledger 2"), "The turn's call did not run"
    assert said(requests[1], "Application mode: Plan"), "The turn did not read the new mode"
    assert b"A request is still executing" not in terminal.plain(), "The message was refused"


def read_at_the_next_stop(terminal):
    """RMK-1317: sent before the model's first word, the message is not in what
    it was already asked; it reads it after its round of calls, and the line
    says so."""
    silent.set()
    terminal.wait_for(b"[Ask]")
    terminal.send(b"Find the record\r")
    terminal.wait_for(b"Working")
    offset = len(terminal.captured)
    terminal.send(STEERED.encode() + b"\r")
    terminal.wait_for("↳ sent to the running turn".encode(), offset)
    release.set()
    terminal.wait_for(b"The record is in the second ledger.", offset)
    terminal.wait_for("↳ read by the turn".encode(), offset)
    terminal.wait_for(b"Execution Completed", offset)
    assert len(requests) == 2, f"{len(requests)} model requests, not 2: the message took a turn of its own"
    assert not said(requests[0], STEERED), "The first request knew the message"
    messages = requests[1]["messages"]
    roles = [message["role"] for message in messages]
    read = [i for i, message in enumerate(messages) if STEERED in json.dumps(message)]
    assert len(read) == 1, f"The model read the message {len(read)} times"
    assert messages[read[0]]["role"] == "user", messages[read[0]]
    assert read[0] > len(roles) - 1 - roles[::-1].index("tool"), f"Not after the call's result: {roles}"


def cut_for_a_message_already_handed(terminal):
    offset = terminal.working()
    terminal.send(STEERED.encode() + b"\r")
    terminal.wait_for("sent to the running turn · Ctrl+S to cut it now".encode(), offset)
    terminal.send(CTRL_S)
    terminal.wait_for(b"Execution Interrupted", offset)
    terminal.wait_for(b"The record is in the second ledger.", offset)
    terminal.wait_for("answered in its own turn".encode(), offset)
    terminal.wait_for(b"Execution Completed", offset)
    assert len(requests) == 2, f"{len(requests)} model requests, not 2"
    read = sum(STEERED == message.get("content") for message in requests[1]["messages"])
    assert read == 1, f"The model read the message {read} times"
    assert said(requests[1], "Application mode: Ask"), "The turn did not read its mode"
    assert b"cut the turn" not in terminal.plain(), "The instruction showed"


def sent_now(key):
    def scenario(terminal):
        offset = terminal.working()
        terminal.send(STEERED.encode() + key)
        terminal.wait_for(b"Execution Interrupted", offset)
        terminal.wait_for(b"The record is in the second ledger.", offset)
        terminal.wait_for(b"Execution Completed", offset)
        assert len(requests) == 2, f"{len(requests)} model requests, not 2"
        assert said(requests[1], STEERED), "The message did not reach the model"
        assert said(requests[1], "Find the record"), "The cut turn's message left the conversation"
        assert "↳ sent to the running turn".encode() not in terminal.plain(offset), \
            "The message was handed to the turn it should have cut"
    return scenario


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    for check in (
        handed_to_the_running_turn,
        read_at_the_next_stop,
        sent_now(CTRL_S),
        sent_now(CTRL_ENTER),
        cut_for_a_message_already_handed,
    ):
        requests.clear()
        release.clear()
        silent.clear()
        with tempfile.TemporaryDirectory() as directory:
            terminal = Terminal(Path(directory), f"http://127.0.0.1:{server.server_port}/v1")
            try:
                check(terminal)
                terminal.exits()
            finally:
                terminal.close()
finally:
    release.set()
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY steering passed: a message typed during the agent's turn goes to that turn, which reads it "
      "at its next step with a mode changed meanwhile; Ctrl+S and Ctrl+Enter cut the turn and send it "
      "once it stopped; on an empty composer, Ctrl+S cuts it to answer the message already handed.")
