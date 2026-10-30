"""Run the real application in a POSIX PTY with a local model fixture."""

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
requests = []
continue_response = threading.Event()


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(request)
        if len(requests) == 3 and not continue_response.wait(timeout=15):
            self.send_error(504, "Fixture response was not released")
            return
        if len(requests) == 1:
            delta = {"tool_calls": [{"index": 0, "id": "read-1", "type": "function", "function": {
                "name": "read", "arguments": json.dumps({"path": "fixture.txt"})
            }}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": '\n\n## Inspection\n\nFixture **inspection** complete.\n\n- Read `fixture.txt`\n- Read-only\n\n```json\n{"ok": true, "count": 1}\n```\n\nLatest answer marker.\n'}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
                + "\n\ndata: " + json.dumps({"choices":[], "usage": {
                    "prompt_tokens":1000 if len(requests)==1 else 2000,
                    "completion_tokens":20 if len(requests)==1 else 50,
                    "prompt_tokens_details":{"cached_tokens":500}}})
                + "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def scenario(project, state, base_url, resume=False, color="never"):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
    original = termios.tcgetattr(slave)
    env = {k: v for k, v in os.environ.items() if not k.startswith("ROOMKIT_")}
    env["NO_COLOR"] = "1" # Explicit always must override this; auto/never must not.
    args = [str(binary), str(project), "--state-dir", str(state), "--base-url", base_url,
            "--model", "fixture-model", "--context-window", "32768", "--color", color, "--no-animations"]
    if resume:
        args += ["--native-selection", "--no-logo"]
    else:
        args += ["--session", "fixture"]
    process = subprocess.Popen(args, stdin=slave, stdout=slave, stderr=slave, env=env)
    captured = bytearray()

    def wait_for(marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:]):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}; terminal output: {bytes(captured[-3000:])!r}")
            if select.select([master], [], [], min(0.2, max(0, deadline - time.monotonic())))[0]:
                captured.extend(os.read(master, 65536))

    try:
        wait_for(b"fixture-model")
        wait_for(b"RoomKit Code")
        wait_for("› Ask about this project…".encode())
        wait_for(b"Start a conversation")
        assert b"Fixture inspection complete." not in captured, "Launch unexpectedly resumed the previous session"
        if not resume:
            offset = len(captured)
            os.write(master, b"/tools\r")
            wait_for(b"Tools available to code", offset)
            wait_for(b"19 registered", offset)
            os.write(master, b"shell")
            wait_for(b"Filter: shell", offset)
            os.write(master, b"\r")
            wait_for(b"shell is registered", offset)
            assert not requests, "Tool inspection must not contact the model"
            os.write(master, b"Read fixture.txt\r")
        else:
            offset = len(captured)
            os.write(master, b"/resume\r")
            wait_for(b"Resume session", offset)
            assert b"Empty conversation" not in captured[offset:], "Unused drafts leaked into the picker"
            # Filtering is local; an unmatched query and Escape never call the model.
            os.write(master, b"no-such-session")
            wait_for(b"No matching choices", offset)
            offset = len(captured)
            os.write(master, b"\x1b")
            wait_for(b"Enter send", offset)
            os.write(master, b"/resume\r")
            wait_for(b"Resume session", offset)
            os.write(master, b"fixture")
            wait_for(b"Filter: fixture", offset)
            os.write(master, b"\r")
        wait_for(b"Fixture inspection complete.")
        wait_for(b"Read-only")
        # The longer answer can push the compact tool's summary off screen.
        # Inspect it through actual history navigation, not transient streaming.
        os.write(master, b"\x1b[5~")
        # Transcript/footer height varies; focus the retained tool so its
        # expanded body is visible instead of assuming a fixed page position.
        os.write(master, b"\x1b[1;3A")
        wait_for('fixture.txt · lines 1–4'.encode())
        os.write(master, b"\x1b[1;5F")
        assert b'"data":' not in captured, "The file card leaked its raw JSON envelope"
        assert b"**inspection**" not in captured, "Markdown was displayed as raw syntax"
        if not resume:
            wait_for(b"Execution Completed")
            assert b"Expanded fixture line" not in captured
            offset = len(captured)
            # Focus before expanding: Ctrl+End above deliberately returned to
            # the latest answer, which can leave the tool outside the viewport.
            os.write(master, b"\x1b[1;3A\x1bOQ")
            wait_for(b"F2 collapse", offset)
            wait_for(b"Expanded fixture line", offset)
        # A partially entered draft survives returning from history. Ctrl+End
        # works even when native selection leaves mouse events to the terminal.
        os.write(master, b"Unsent draft\x1b[1;5F")
        wait_for(b"Latest answer marker.")
        wait_for(b"context 2050/32768")
        offset = len(captured)
        os.write(master, b"\x1b[5~") # PageUp
        wait_for(b"Back to bottom", offset)
        offset = len(captured)
        os.write(master, b"\x1b[1;5F") # Ctrl+End
        wait_for(b"Latest answer marker.", offset)
        if not resume:
            offset = len(captured)
            os.write(master, b"\x1b[<64;30;10M") # Wheel up
            wait_for(b"Back to bottom", offset)
            marker = captured.rfind(b"Back to bottom")
            positions = re.findall(rb"\x1b\[(\d+);(\d+)H", captured[:marker])
            assert positions, "Return-to-bottom control was not positioned"
            row = positions[-1][0]
            offset = len(captured)
            # The badge reaches the right edge of the 120-column frame.
            os.write(master, b"\x1b[<0;115;" + row + b"M\x1b[<0;115;" + row + b"m")
            wait_for(b"Latest answer marker.", offset)
            assert b"\x1b]52;c;" not in captured, "Navigation unexpectedly requested a clipboard write"
        # Trigger an editor redraw so a lost or submitted draft is detectable.
        offset = len(captured)
        os.write(master, b"!")
        wait_for(b"Unsent draft!", offset)
        if not resume:
            offset = len(captured)
            os.write(master, b"\x7f" * len("Unsent draft!") + b"/new\r")
            wait_for(b"Start a conversation", offset)
            os.write(master, b"/resume\r")
            wait_for(b"Resume session", offset)
            os.write(master, b"fixture")
            wait_for(b"Filter: fixture", offset)
            offset = len(captured)
            os.write(master, b"\r")
            wait_for(b"Latest answer marker.", offset)
            # Hold the next provider response until the prior context is
            # visibly labeled. It must not disappear during a working turn.
            offset = len(captured)
            os.write(master, b"Check the context again.\r")
            wait_for(b"context (last report) 2050/32768", offset)
            offset = len(captured)
            continue_response.set()
            wait_for(b"Execution Completed", offset)
            wait_for(b"context 2050/32768", offset)
        os.write(master, b"\x04")
        code = process.wait(timeout=10)
        while select.select([master], [], [], 0)[0]:
            captured.extend(os.read(master, 65536))
        assert code == 0, bytes(captured[-3000:])
        assert termios.tcgetattr(slave) == original, "Terminal modes not restored"
        assert b"\x1b[?1049l" in captured, "Alternate screen not restored"
        assert b"\x1b[?2004l" in captured, "Bracketed paste not restored"
        has_colors = re.search(rb"\x1b\[(?:38|48);", captured) is not None
        assert has_colors == (color == "always"), "Color policy was not respected"
        if resume:
            assert b"\x1b[?1006h" not in captured
        else:
            assert b"\x1b[?1006l" in captured, "Mouse mode not restored"
    finally:
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
        project, state = root / "project", root / "state"
        project.mkdir()
        (project / "fixture.txt").write_text("A fictional project fixture.\nSecond line.\nExpanded fixture line.\nFinal line.\n")
        base_url = f"http://127.0.0.1:{server.server_port}/v1"
        scenario(project, state, base_url)
        scenario(project, state, base_url, resume=True)
        scenario(project, state, base_url, resume=True, color="always")
        scenario(project, state, base_url, resume=True, color="auto")
        assert len(requests) == 3, "Resume reexecuted a model turn"
        assert any(m.get("role") == "tool" and "fictional project fixture" in m.get("content", "")
                   for m in requests[-1]["messages"]), "The model did not receive the actual file"
        assert sorted(p.name for p in project.iterdir()) == ["fixture.txt"], "Workspace was modified"
finally:
    continue_response.set()
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY checks passed: tools, Markdown, composer, history navigation, fresh launch, new/resume/filter/cancel, empty-draft filtering, working context report, preserved draft, colors, restart and terminal restoration.")
