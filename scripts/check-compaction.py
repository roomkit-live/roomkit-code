"""Exercise /compact through an isolated PTY: it starts at once, or once the turn under way ends, the status line says
so while the summary is written, its line stays in the conversation, comes back on resume, and the agent never reads it;
the context gauge shows what it left, as an estimate."""
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
# Cleared, the summaries or the turns wait at the server until set again.
summaries_free, turns_free = threading.Event(), threading.Event()
summaries_free.set()
turns_free.set()
SUMMARY = "SUMMARY_OF_THE_EARLIER_TURNS"

class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(request)
        if request.get("stream") is not True:
            summaries_free.wait(30)
            body = json.dumps({"choices": [{"index": 0, "message": {"role": "assistant", "content": SUMMARY},
                                            "finish_reason": "stop"}]}).encode()
            content = "application/json"
        else:
            turns_free.wait(30)
            # Long enough for the status line to draw the turn running, then its end.
            time.sleep(0.3)
            body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": {"content": "Compaction fixture answer."}}]})
                    + "\n\ndata: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
                    + "\n\ndata: [DONE]\n\n").encode()
            content = "text/event-stream"
        self.send_response(200)
        self.send_header("Content-Type", content)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

def streamed():
    return [request for request in requests if request.get("stream") is True]

def summaries():
    return [request for request in requests if request.get("stream") is not True]

class App:
    def __init__(self, project, storage, url):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 160, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env.update({"TERM": "xterm-256color", "HOME": str(storage / "home"), "XDG_DATA_HOME": str(storage / "data")})
        self.process = subprocess.Popen([str(binary), str(project), "--state-dir", str(storage), "--base-url", url,
            "--model", "fixture", "--context-window", "200000", "--session", "compaction", "--color", "never",
            "--no-animations", "--no-herdr", "--no-terminal-status"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True, env=env)
        self.output = bytearray()

    def send(self, data):
        offset = len(self.output)
        os.write(self.master, data.encode() if isinstance(data, str) else data)
        return offset

    def text(self, after=0):
        return re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.output[after:])

    def wait(self, text, after=0):
        deadline = time.monotonic() + 15
        while text.encode() not in self.text(after):
            if time.monotonic() > deadline or self.process.poll() is not None:
                raise AssertionError(f"Missing {text!r}: {self.output[-6000:]!r}")
            if select.select([self.master], [], [], 0.02)[0]:
                self.output.extend(os.read(self.master, 65536))

    def rows(self, text, after=0):
        """The screen rows `text` was written at after `after`: the terminal moves the cursor before each row it writes."""
        raw, rows = bytes(self.output), []
        at = raw.find(text.encode(), after)
        while at >= 0:
            moves = re.findall(rb"\x1b\[(\d+);\d+H", raw[max(0, at - 400):at])
            if moves:
                rows.append(int(moves[-1]))
            at = raw.find(text.encode(), at + 1)
        return rows

    def wait_below(self, text, row, after):
        """Wait until `text` is written after `after` on a screen row below `row`."""
        deadline = time.monotonic() + 15
        while not any(written > row for written in self.rows(text, after)):
            if time.monotonic() > deadline or self.process.poll() is not None:
                raise AssertionError(f"Missing {text!r} below row {row}: {self.output[-6000:]!r}")
            if select.select([self.master], [], [], 0.02)[0]:
                self.output.extend(os.read(self.master, 65536))

    def turn(self, text):
        offset = self.send(text + "\r")
        self.wait("Working", offset)
        self.wait("Compaction fixture answer.", offset)
        self.wait("Execution Completed", offset)

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

server = LoopbackHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix="roomkit-compaction-") as directory:
        root = Path(directory)
        project, storage = root / "project", root / "state"
        project.mkdir()
        storage.mkdir()
        url = f"http://127.0.0.1:{server.server_port}/v1"
        app = App(project, storage, url)
        try:
            app.wait("[Ask]")
            for question in ("First question", "Second question", "Third question"):
                app.turn(question)
            assert not summaries()
            # Idle: the summary is asked at once, and the status line says so while it is written.
            summaries_free.clear()
            offset = app.send("/compact\r")
            app.wait("Compacting the conversation…", offset)
            deadline = time.monotonic() + 15
            while not summaries():
                assert time.monotonic() < deadline, f"No summary was asked: {app.output[-6000:]!r}"
                if select.select([app.master], [], [], 0.02)[0]:
                    app.output.extend(os.read(app.master, 65536))
            assert len(streamed()) == 3, "No message was needed"
            summaries_free.set()
            app.wait("● roomkit-code", offset)
            # The first turn's: four messages at least are kept whole.
            line = "Conversation compacted · 2 messages summarized · ≈"
            app.wait(line, offset)
            first = max(app.rows(line, offset))
            # The gauge shows what the compaction left, as an estimate: no request measured it.
            app.wait(" / 200000 tokens", offset)
            assert re.search(rb"~\d+ / 200000 tokens", app.text(offset)), app.text(offset)[-3000:]
            # During a turn: queued, then run as the turn ends.
            turns_free.clear()
            offset = app.send("Fourth question\r")
            deadline = time.monotonic() + 15
            # A pending redraw must finish before the app can read Enter.
            while len(streamed()) < 4:
                assert time.monotonic() < deadline, f"The fourth turn sent nothing: {app.output[-6000:]!r}"
                if select.select([app.master], [], [], 0.02)[0]:
                    app.output.extend(os.read(app.master, 65536))
            app.wait("Working", offset)
            app.send("/compact\r")
            app.wait("Compaction queued: it runs when the agent's turn ends", offset)
            assert len(summaries()) == 1
            turns_free.set()
            app.wait("Compaction fixture answer.", offset)
            # The second turn's, the first being in the earlier summary. It stands below the first compaction's line,
            # which the terminal writes again whenever it repaints its row: the sidebar beside it changes as a turn
            # starts.
            app.wait_below(line, first, offset)
            assert len(summaries()) == 2 and len(streamed()) == 4
        finally:
            app.close()
        # Resumed: both lines in their place; the agent reads the summary, never a line.
        app = App(project, storage, url)
        try:
            app.wait("[Ask]")
            restored = app.text().decode(errors="replace")
            first = restored.find("Conversation compacted · 2 messages")
            second = restored.find("Conversation compacted · 2 messages", first + 1)
            order = [restored.find("Third question"), first, restored.find("Fourth question"), second]
            assert 0 <= order[0] and order == sorted(order), (order, restored[-4000:])
            app.turn("Fifth question")
            last = json.dumps(streamed()[-1])
            assert SUMMARY in last and "Conversation compacted" not in last
            assert len(summaries()) == 2
        finally:
            app.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
print("PTY compaction passed: /compact at once and during a turn, the status while it runs, the gauge's estimate, its lasting line on resume.")
