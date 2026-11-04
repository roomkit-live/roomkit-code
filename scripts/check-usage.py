"""Exercise the context and the usage of a long turn in a real terminal (RMK-1333).

A fake server reports 1000, 2000, then 3000 tokens, one step at a time: the
counter follows each step without "(last report)", keeps the last measure after
Esc, on the first turn too, and after a resume, and the exit summary counts the
stopped turn.
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

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"]))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
LONG = {"command": "sleep 300", "_roomkit_intent": "I wait"}
READ = {"command": "true", "_roomkit_intent": "I read"}


class Script:
    """One reply per request; a step opens only once the test lets it."""

    def __init__(self, steps):
        self.steps = steps
        self.requests = 0
        self.lock = threading.Lock()
        self.gates = [threading.Event() for _ in steps]

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
        body = "".join(f"data: {json.dumps(chunk)}\n\n" for chunk in chunks) + "data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body.encode())))
        self.end_headers()
        try:
            self.wfile.write(body.encode())
        except BrokenPipeError:
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


def scenario(server, steps, ask, *, stop_at, expect_total, keeps_after_resume=True):
    """Runs the steps with a stop in the last, and returns the exit report."""
    global script
    script = Script(steps)
    with tempfile.TemporaryDirectory(prefix="roomkit-usage-") as directory:
        root = Path(directory)
        project, storage = root / "project", root / "state"
        project.mkdir()
        storage.mkdir()
        url = f"http://127.0.0.1:{server.server_port}/v1"
        argv = [str(binary), str(project), "--state-dir", str(storage), "--base-url", url, "--model", "fixture",
                "--mode", "yolo", "--context-window", "131072", "--color", "never", "--no-animations", "--no-herdr", "--no-terminal-status"]
        app = App(argv, storage)
        app.wait("[Yolo]")
        offset = app.send(ask + "\r")
        for index, (_, _, tokens) in enumerate(steps[:stop_at]):
            script.gates[index].set()
            app.wait(context(tokens + 10), offset)
            assert "(last report)" not in app.screen(offset), "A measure of this turn is no report of the last"
            offset = len(app.output)
        # A held generation stays in flight; its call runs otherwise.
        app.settle()
        offset = app.send("\x1b")
        app.wait("Interrupted", offset)
        app.settle()
        last = steps[stop_at - 1][2] + 10
        assert context(last) in app.screen(0)[-6000:], app.screen(0)[-3000:]
        assert "context ?" not in app.screen(offset), app.screen(offset)[-3000:]
        for gate in script.gates:
            gate.set()
        report = app.quit()
        total = next((line for line in report if line.startswith("Token usage:")), None)
        if expect_total is None:
            assert total is None, report
        else:
            assert total is not None and expect_total in total, report
        command = report[report.index("To continue this session, run:") + 1].split()

        app = App([str(binary)] + command[1:], storage)
        app.wait("[Yolo]")
        app.settle()
        if keeps_after_resume:
            assert context(last) in app.screen(0), app.screen(0)[-3000:]
        else:
            assert "context ?" in app.screen(0), app.screen(0)[-3000:]
        lines = app.quit()
        return total, lines


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    # A stop while the third step's call runs: every generation told, complete.
    total, _ = scenario(server, [
        ("One.", ("shell", READ), 1000),
        ("Two.", ("shell", READ), 2000),
        ("Three.", ("shell", LONG), 3000),
    ], "Review this", stop_at=3, expect_total="total=6030 input=6000 (+ 0 cached) output=30 · 1 turn")
    assert "(partial)" not in total, total

    # A first turn with no text at all, stopped while its call runs: the counter
    # holds. It wrote no message, so the store holds no record of it (RMK-1333,
    # HITL 5f58e3e3): after a resume, no measure, and no usage at exit.
    scenario(server, [
        ("", ("shell", READ), 1000),
        ("", ("shell", READ), 2000),
        ("", ("shell", LONG), 3000),
    ], "Review quietly", stop_at=3, expect_total=None, keeps_after_resume=False)

    # A stop with the third generation in flight: not counted, and partial.
    total, _ = scenario(server, [
        ("One.", ("shell", READ), 1000),
        ("Two.", ("shell", READ), 2000),
        ("Never.", None, 3000),
    ], "Review that", stop_at=2, expect_total="total=3020 input=3000 (+ 0 cached) output=20 · 1 turn (partial)")
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY usage passed: the counter follows each step, a stopped turn keeps its measure and its usage, after a resume too.")
