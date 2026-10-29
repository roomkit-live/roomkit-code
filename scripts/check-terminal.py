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


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(request)
        if len(requests) == 1:
            delta = {"tool_calls": [{"index": 0, "id": "read-1", "type": "function", "function": {
                "name": "read", "arguments": json.dumps({"path": "fixture.txt"})
            }}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": '\n\n## Inspection\n\nFixture **inspection** complete.\n\n- Read `fixture.txt`\n- Read-only\n\n```json\n{"ok": true, "count": 1}\n```\n\n'}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
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
            "--model", "fixture-model", "--color", color, "--no-animations"]
    if resume:
        args += ["--native-selection", "--no-logo"]
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
        if not resume:
            os.write(master, b"Read fixture.txt\r")
        wait_for(b"Fixture inspection complete.")
        wait_for(b"Read-only")
        wait_for('fixture.txt · lines 1–4'.encode())
        assert b'"data":' not in captured, "The file card leaked its raw JSON envelope"
        assert b"**inspection**" not in captured, "Markdown was displayed as raw syntax"
        if not resume:
            wait_for(b"Execution Completed")
            assert b"Expanded fixture line" not in captured
            offset = len(captured)
            os.write(master, b"\x1bOQ")  # F2: the generic UI expands the real tool.
            wait_for(b"F2 collapse", offset)
            wait_for(b"Expanded fixture line", offset)
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
        assert len(requests) == 2, "Resume reexecuted a model turn"
        assert any(m.get("role") == "tool" and "fictional project fixture" in m.get("content", "")
                   for m in requests[-1]["messages"]), "The model did not receive the actual file"
        assert sorted(p.name for p in project.iterdir()) == ["fixture.txt"], "Workspace was modified"
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY checks passed: formatted file tool, Markdown, color/monochrome, details, restart and terminal restoration.")
