"""Exercise both status outputs with a real app, fake Herdr and local model."""

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
import sys
import tempfile
import termios
import threading
import time

from fixture_http import LoopbackHTTPServer

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
# roomkit-tui ignores answers until a card has been shown for 500 ms.
ARMING = 0.6
requests = []
send_tool, send_answer = threading.Event(), threading.Event()


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(request)
        index = len(requests)
        if index == 1:
            assert send_tool.wait(timeout=15)
            delta = {"tool_calls": [{"index": 0, "id": "write-1", "type": "function", "function": {
                "name": "write", "arguments": json.dumps({"path": "approved.txt", "content": "Approved change"})
            }}]}
            reason = "tool_calls"
        elif index == 2:
            assert send_answer.wait(timeout=15)
            delta, reason = {"content": "Herdr fixture complete."}, "stop"
        else:
            self.send_error(500, "PRIVATE PROVIDER BODY")
            return
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
                + "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def option(args, name):
    return args[args.index(name) + 1]


def reports(path):
    result = []
    if path.exists():
        for line in path.read_text().splitlines():
            try:
                result.append(json.loads(line))
            except json.JSONDecodeError:
                pass  # A concurrently appended record is checked on the next poll.
    return result


class App:
    def __init__(self, args, env, log):
        self.log, self.offset = log, len(reports(log))
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 130, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        self.process = subprocess.Popen(args, stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True, env=env)
        self.output = bytearray()

    def wait(self, condition):
        deadline = time.monotonic() + 15
        while not condition():
            if time.monotonic() > deadline or self.process.poll() is not None:
                raise AssertionError(f"App deadline/exit: {self.output[-2000:]!r}; reports={self.calls()[-3:]}")
            self.drain(0.02)

    def drain(self, timeout=0):
        if select.select([self.master], [], [], timeout)[0]:
            self.output.extend(os.read(self.master, 65536))

    def calls(self):
        return reports(self.log)[self.offset:]

    def text(self, text, after=0):
        marker = text.encode()
        self.wait(lambda: marker in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.output[after:]))

    def send(self, data):
        os.write(self.master, data.encode() if isinstance(data, str) else data)

    def report(self, predicate, after=0):
        self.wait(lambda: any(predicate(call["args"]) for call in self.calls()[after:]))
        return next(call["args"] for call in reversed(self.calls()[after:]) if predicate(call["args"]))

    def state(self, state, after=0):
        return self.report(lambda args: args[1] == "report-agent" and option(args, "--state") == state, after)

    def osc(self, state):
        marker = f"\x1b]7501;state={state}:app=roomkit-code\x1b\\".encode()
        self.wait(lambda: marker in self.output)

    def finish(self):
        self.send(b"\x04")
        deadline = time.monotonic() + 8
        while self.process.poll() is None and time.monotonic() < deadline:
            self.drain(0.02)
        assert self.process.poll() == 0, "Application shutdown did not finish"
        while select.select([self.master], [], [], 0)[0]:
            self.drain()
        assert termios.tcgetattr(self.slave) == self.original
        assert b"HERDR PRIVATE OUTPUT" not in self.output
        assert b"PRIVATE PROVIDER BODY" not in self.output

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        os.close(self.master)
        os.close(self.slave)


server = LoopbackHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix="roomkit-herdr-") as directory:
        root = Path(directory)
        project, storage = root / "project with spaces", root / "application state"
        project.mkdir()
        log, mode = root / "reports.jsonl", root / "reporter-mode"
        mode.write_text("ok")
        fake = root / "fake-herdr"
        fake.write_text(f'''#!{sys.executable}
import json, os, pathlib, sys, time
with open({str(log)!r}, "a") as log:
    log.write(json.dumps({{"args": sys.argv[1:], "socket": os.environ.get("HERDR_SOCKET_PATH"), "at": time.time_ns() // 1_000_000}}) + "\\n")
print("HERDR PRIVATE OUTPUT", file=sys.stderr)
mode = pathlib.Path({str(mode)!r}).read_text()
if mode == "stall": time.sleep(10)
if mode == "fail": sys.exit(1)
''')
        fake.chmod(0o700)
        env = {key: value for key, value in os.environ.items() if not key.startswith(("ROOMKIT_", "HERDR_"))}
        env.update({"HERDR_ENV": "1", "HERDR_BIN_PATH": str(fake), "HERDR_SOCKET_PATH": str(root / "herdr.sock"),
                    "HERDR_PANE_ID": "w1:p1", "XDG_DATA_HOME": str(root / "data"), "HOME": str(root / "home"),
                    "TERM": "xterm-256color", "NO_COLOR": "1", "PATH": str(binary.parent) + os.pathsep + env.get("PATH", "")})
        config = root / "appearance.toml"
        config.write_text("[ui]\nname = 'Integration Fixture'\n")
        args = [str(binary), str(project), "--state-dir", str(storage), "--config", str(config),
                "--base-url", f"http://127.0.0.1:{server.server_port}/v1", "--model", "fixture-model",
                "--color", "never", "--no-animations", "--no-logo"]

        app = App(args, env, log)
        try:
            app.text("fixture-model")
            initial = app.state("idle")
            initial_id = option(initial, "--agent-session-id")
            assert "--" in initial and initial[initial.index("--") + 1] == "roomkit-code"
            app.osc("idle")
            app.send("Change the fixture\r")
            app.state("working")
            app.osc("working")
            send_tool.set()
            app.state("blocked")
            app.osc("blocked:kind=permission")
            app.text("Allow write?")
            assert not (project / "approved.txt").exists()
            offset = len(app.calls())
            time.sleep(ARMING)
            app.send(b"\x19")
            app.state("working", offset)
            app.wait(lambda: (project / "approved.txt").exists())
            assert (project / "approved.txt").read_text() == "Approved change"
            app.send("/mode yolo\r")
            app.text("[Yolo]")
            app.report(lambda a: a[1] == "report-agent-session" and option(a, "--mode") == "yolo", offset)
            send_answer.set()
            app.text("Herdr fixture complete.")
            app.text("Execution Completed")
            app.osc("done")
            app.state("idle", offset)
            offset = len(app.calls())
            app.send("/resume missing-session\r")
            app.text("Session not found in this project")
            assert not any("missing-session" in call["args"] for call in app.calls()[offset:])
            app.send("/new\r")
            changed = app.report(lambda a: a[1] == "report-agent-session" and option(a, "--agent-session-id") != initial_id, offset)
            assert option(changed, "--mode") == "ask"
            app.send(f"/resume {initial_id}\r")
            resumed = app.report(lambda a: a[1] == "report-agent-session" and option(a, "--agent-session-id") == initial_id, offset)
            assert option(resumed, "--mode") == "yolo"
            resume = resumed[resumed.index("--") + 1:]
            assert resume[:4] == ["roomkit-code", str(project.resolve()), "--state-dir", str(storage)]
            assert option(resume, "--config") == str(config)
            app.send("Fail safely\r")
            app.text("Execution Failed")
            app.osc("error")
            # A fast failure may coalesce to idle; no stale blocked status survives.
            app.wait(lambda: next(option(c["args"], "--state") for c in reversed(app.calls()) if c["args"][1] == "report-agent") == "idle")
            app.finish()
            app.osc("clear")
            assert app.calls()[-2]["args"][1] == "report-metadata" and "--clear-title" in app.calls()[-2]["args"]
            assert app.calls()[-1]["args"][1] == "release-agent"
            stopped = [entry["timestamp_ms"] for path in storage.rglob("diagnostics.jsonl")
                       for entry in reports(path) if entry["event"] == "application_stopped"]
            assert stopped and max(stopped) <= app.calls()[-1]["at"], "Released before application cleanup"
        finally:
            send_tool.set(); send_answer.set(); app.close()

        previous_seq = int(option(reports(log)[-1]["args"], "--seq"))
        request_count = len(requests)
        # Use the actual published argv; the existing profile supplies the model.
        app = App(resume, env, log)
        try:
            app.text("fixture-model")
            app.text("[Yolo]")
            restored = app.state("idle")
            assert option(restored, "--agent-session-id") == initial_id
            assert int(option(restored, "--seq")) > previous_seq
            assert len(requests) == request_count, "Restoring a session reexecuted the model/tools"
            app.finish()
        finally:
            app.close()

        # All four compositions, plus an incomplete environment and TERM=dumb.
        for flags, herdr, osc_enabled, extra_env in [
            (["--no-herdr", "--no-terminal-status"], False, False, {}),
            (["--no-herdr"], False, True, {}),
            (["--no-terminal-status"], True, False, {}),
            ([], True, True, {}),
            ([], False, True, {"HERDR_PANE_ID": ""}),
            ([], True, False, {"TERM": "dumb"}),
        ]:
            app = App([*args, "--session", "named", *flags], {**env, **extra_env}, log)
            try:
                app.text("fixture-model")
                if herdr:
                    assert option(app.state("idle"), "--agent-session-id") == "named"
                if osc_enabled: app.osc("idle")
                app.finish()
                assert bool(app.calls()) == herdr
                assert (b"\x1b]7501;" in app.output) == osc_enabled
            finally:
                app.close()

        # Reporter failure cannot prevent normal terminal input or bounded exit.
        for failure in ("fail", "stall"):
            mode.write_text(failure)
            app = App(args, env, log)
            try:
                app.text("fixture-model")
                app.wait(lambda: bool(app.calls()))
                app.send("Local draft")
                app.text("Local draft")
                app.finish()
            finally:
                app.close()
        mode.write_text("ok")

        for flag in ["--help", "--version", "--check-config", None]:
            before = len(reports(log))
            result = subprocess.run([*args, *([flag] if flag else [])], input=b"", capture_output=True, env=env, timeout=10)
            assert result.returncode == (0 if flag else 1)
            assert b"\x1b]7501;" not in result.stdout + result.stderr
            assert len(reports(log)) == before
        sequences = [int(option(call["args"], "--seq")) for call in reports(log)]
        assert all(left < right for left, right in zip(sequences, sequences[1:]))
        assert all(call["socket"] == env["HERDR_SOCKET_PATH"] for call in reports(log))
        assert all(option(call["args"], "--source") in ("roomkit-code", "roomkit-code-title") for call in reports(log))
finally:
    send_tool.set(); send_answer.set()
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("Herdr checks passed: independent outputs, permissions, mode/session changes, persisted resume, failures, monotonic reports and cleanup.")
