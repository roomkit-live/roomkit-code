"""Exercise the context and the usage of a long turn in a real terminal (RMK-1333).

A fake server reports 1000, 2000, then 3000 tokens, one step at a time: the
counter follows each step without "(last report)", keeps the last measure after
Esc, on the first turn too, and after a resume, and the turn's totals and the exit
summary count the stopped turn.
"""
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

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"]))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
LONG = {"command": "sleep 300", "_roomkit_intent": "I wait"}
READ = {"command": "true", "_roomkit_intent": "I read"}


class Script:
    """One reply per request; a step opens only once the test lets it."""

    def __init__(self, steps, holds_after_text=()):
        self.steps = steps
        self.requests = 0
        self.lock = threading.Lock()
        self.gates = [threading.Event() for _ in steps]
        # A step that writes its text, then says nothing until the test ends.
        self.holds_after_text = set(holds_after_text)
        self.released = threading.Event()

    def take(self):
        with self.lock:
            index = min(self.requests, len(self.steps) - 1)
            self.requests += 1
        return index


script = None


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        index = script.take()
        if not script.gates[index].wait(timeout=60):
            return
        text, call, tokens = script.steps[index]
        chunks = []
        if text:
            chunks.append({"choices": [{"index": 0, "delta": {"content": text}}]})
        if call:
            name, args = call
            tool = {"index": 0, "id": f"call-{index}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}
            chunks.append({"choices": [{"index": 0, "delta": {"tool_calls": [tool]}}]})
        chunks.append({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if call else "stop"}]})
        chunks.append({"choices": [], "usage": {"prompt_tokens": tokens, "completion_tokens": 10}})
        frames = [f"data: {json.dumps(chunk)}\n\n" for chunk in chunks] + ["data: [DONE]\n\n"]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            for position, frame in enumerate(frames):
                self.wfile.write(frame.encode())
                self.wfile.flush()
                if position == 0 and index in script.holds_after_text:
                    script.released.wait(timeout=60)
        except (BrokenPipeError, ConnectionResetError):
            pass  # The stop closed the request this reply answers.


class App:
    def __init__(self, argv, storage):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 100, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env.update({"TERM": "xterm-256color", "HOME": str(storage / "home"), "XDG_DATA_HOME": str(storage / "data"), "OPENAI_API_KEY": "x"})
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
                raise AssertionError(f"Missing {text!r}: {self.screen(after)[-3000:]!r}")
            if select.select([self.master], [], [], 0.02)[0]:
                self.output.extend(os.read(self.master, 65536))

    def settle(self):
        while select.select([self.master], [], [], 0.3)[0]:
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
            report = bytes(self.output[offset:]).rsplit(b"\x1b[?1049l", 1)[-1]
            text = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", report).decode(errors="replace")
            return text.replace("\r", "").splitlines()
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            os.close(self.master)
            os.close(self.slave)


def context(tokens):
    return f"context {tokens}/131072"


def turn(tokens_in, tokens_out):
    return f"turn {tokens_in} in / {tokens_out} out"


def until_asked(count):
    """The server was asked `count` times: what follows the last step started."""
    deadline = time.monotonic() + 15
    while script.requests < count:
        assert time.monotonic() < deadline, f"{script.requests} requests, {count} expected"
        time.sleep(0.01)


class Run:
    """One conversation in a fresh directory; a failure leaves no process behind."""

    def __enter__(self):
        self.directory = tempfile.TemporaryDirectory(prefix="roomkit-usage-")
        root = Path(self.directory.name)
        self.project, self.storage = root / "project", root / "state"
        self.project.mkdir()
        self.storage.mkdir()
        self.app = None
        return self

    def open(self, argv=None):
        url = f"http://127.0.0.1:{server.server_port}/v1"
        argv = argv or [str(binary), str(self.project), "--state-dir", str(self.storage), "--base-url", url, "--model", "fixture",
                        "--mode", "yolo", "--context-window", "131072", "--color", "never", "--no-animations", "--no-herdr", "--no-terminal-status"]
        self.app = App(argv, self.storage)
        self.app.wait("[Yolo]")
        return self.app

    def __exit__(self, *_):
        if self.app is not None and self.app.process.poll() is None:
            self.app.process.kill()
            self.app.process.wait(timeout=5)
        self.directory.cleanup()


def scenario(steps, ask, *, stop_at, after_esc, expect_total, in_flight=False, before=None, holds_after_text=(), resumed=None):
    """Asks a turn (after `before`, one that finishes), lets it go `stop_at`
    steps, then stops it with Esc; returns the exit report's usage line."""
    global script
    script = Script(([before] if before else []) + steps, holds_after_text)
    with Run() as run:
        app = run.open()
        first = 0
        if before:
            first = 1
            script.gates[0].set()
            offset = app.send("First\r")
            app.wait(before[0], offset)
            app.wait("[Yolo]", offset)
            app.settle()
        offset = app.send(ask + "\r")
        if before:
            app.wait(f"context (last report) {before[2] + 10}/131072", offset)
        for index, (_, _, tokens) in enumerate(steps[:stop_at]):
            script.gates[first + index].set()
            app.wait(context(tokens + 10), offset)
            assert "(last report)" not in app.screen(len(app.output)), "A measure of this turn is no report of the last"
            offset = len(app.output)
        until_asked(first + stop_at + (1 if in_flight else 0))
        if holds_after_text:
            script.gates[first + stop_at].set()
            app.wait(steps[stop_at][0], offset)
        app.settle()
        offset = app.send("\x1b")
        app.wait("Interrupted", offset)
        app.settle()
        script.released.set()
        for gate in script.gates:
            gate.set()
        screen = app.screen(offset)
        for expected in after_esc:
            assert expected in screen, f"{expected!r} after Esc: {screen[-2000:]!r}"
        assert "context ?" not in screen, screen[-2000:]
        report = app.quit()
        total = next((line for line in report if line.startswith("Token usage:")), None)
        assert (total is None) == (expect_total is None) and (total is None or expect_total in total), report
        command = shlex.split(report[report.index("To continue this session, run:") + 1])

        app = run.open([str(binary)] + command[1:])
        app.settle()
        assert resumed in app.screen(0), f"{resumed!r} after a resume: {app.screen(0)[-2000:]!r}"
        app.quit()
    return total


def exit_during_a_turn():
    """Ctrl+D while a call runs: the turn is cut after the report is read, so the
    report says its total is partial."""
    global script
    script = Script([("Done.", None, 1000), ("Working.", ("shell", LONG), 2000)])
    with Run() as run:
        app = run.open()
        script.gates[0].set()
        offset = app.send("First\r")
        app.wait("Done.", offset)
        app.wait("[Yolo]", offset)
        app.settle()
        script.gates[1].set()
        offset = app.send("Second\r")
        app.wait(context(2010), offset)
        app.settle()
        lines = app.quit()
        total = next(line for line in lines if line.startswith("Token usage:"))
        assert "(partial)" in total, lines


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    # The first turn, a stop while the third step's call runs: every generation
    # told, so its usage is complete.
    total = scenario([
        ("One.", ("shell", READ), 1000),
        ("Two.", ("shell", READ), 2000),
        ("Three.", ("shell", LONG), 3000),
    ], "Review this", stop_at=3, after_esc=[context(3010), turn(6000, 30)],
        expect_total="total=6030 input=6000 (+ 0 cached) output=30 · 1 turn", resumed=context(3010))
    assert "(partial)" not in total, total

    # After a turn that finished: its report shows until this turn tells, then
    # this turn's own measure, and both count at exit.
    scenario([
        ("One.", ("shell", READ), 2000),
        ("Two.", ("shell", LONG), 3000),
    ], "Review more", before=("Done.", None, 1000), stop_at=2, after_esc=[context(3010)],
        expect_total="total=6030 input=6000 (+ 0 cached) output=30 · 2 turns", resumed=context(3010))

    # A first turn with no text at all, stopped while its call runs: the counter
    # holds: nothing redraws it after Esc, "context ?" would. It wrote no message,
    # so the store holds no record of it (RMK-1333, HITL 5f58e3e3): after a resume,
    # no measure, and no usage at exit.
    scenario([
        ("", ("shell", READ), 1000),
        ("", ("shell", READ), 2000),
        ("", ("shell", LONG), 3000),
    ], "Review quietly", stop_at=3, after_esc=[], expect_total=None, resumed="context ?")

    # A stop with the third generation in flight: not counted, and partial.
    scenario([
        ("One.", ("shell", READ), 1000),
        ("Two.", ("shell", READ), 2000),
        ("Never.", None, 3000),
    ], "Review that", stop_at=2, in_flight=True, after_esc=[context(2010), turn(3000, 20), "(partial)"],
        expect_total="total=3020 input=3000 (+ 0 cached) output=20 · 1 turn (partial)", resumed=context(2010))

    # A stop in a turn's first generation, its text kept: nothing told it, the
    # total says it is partial, the last turn's measure stays a report.
    scenario([
        ("Partly.", None, 3000),
    ], "Review once more", before=("Done.", None, 1000), stop_at=0, holds_after_text=[1], in_flight=True,
        after_esc=[],
        expect_total="total=1010 input=1000 (+ 0 cached) output=10 · 1 turn (partial)", resumed="context ?")

    exit_during_a_turn()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY usage passed: the counter follows each step, a stopped turn keeps its measure and its usage, after a resume too.")
