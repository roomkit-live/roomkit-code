"""Exercise real permission keys and tool side effects in a POSIX terminal."""

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
calls = [
    ("write", {"path": "denied.txt", "content": "should not exist"}),
    ("edit", {"path": "source.txt", "old_text": "before", "new_text": "after"}),
    ("shell", {"command": "printf 'shell result' > shell.txt; printf 'shell passed'"}),
]


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        index = len(requests)
        requests.append(request)
        if index < len(calls):
            name, arguments = calls[index]
            delta = {"tool_calls": [{"index": 0, "id": f"call-{index}", "type": "function",
                                    "function": {"name": name, "arguments": json.dumps(arguments)}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": "Permission fixture complete."}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
                + "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def scenario(project, state, url):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
    original = termios.tcgetattr(slave)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    process = subprocess.Popen(
        [str(binary), str(project), "--state-dir", str(state), "--base-url", url,
         "--model", "fixture", "--color", "always", "--no-animations"],
        stdin=slave, stdout=slave, stderr=slave, env=env,
    )
    captured = bytearray()

    def wait_for(marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:]):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}; terminal: {bytes(captured[-3500:])!r}")
            if select.select([master], [], [], 0.1)[0]:
                captured.extend(os.read(master, 65536))

    try:
        wait_for(b"[Ask]")
        os.write(master, b"Preserved draft")
        wait_for(b"Preserved draft")
        offset = len(captured)
        os.write(master, b"\x1b[Z")  # Shift+Tab
        wait_for(b"[Yolo]", offset)
        os.write(master, b"!")
        wait_for(b"Preserved draft!", offset)
        os.write(master, b"\x7f")
        offset = len(captured)
        os.write(master, b"\x1b[Z")
        wait_for(b"[Ask]", offset)
        assert not requests, "Changing modes must not call the model"
        os.write(master, b"\x7f" * len("Preserved draft") + b"Change the fixture\r")
        wait_for(b"Allow write?")
        wait_for(b"denied.txt")
        assert not (project / "denied.txt").exists()
        os.write(master, b"\x0e")  # Ctrl+N denies the actual call.
        wait_for(b"Allow edit?")
        wait_for(b"old_text")
        assert (project / "source.txt").read_text() == "before\n"
        offset = len(captured)
        os.write(master, b"\x1b[Z")
        wait_for(b"[Yolo]", offset)
        wait_for(b"Allow edit?")
        assert len(requests) == 2, "Yolo must not settle the existing approval"
        assert (project / "source.txt").read_text() == "before\n"
        os.write(master, b"\x19")  # Ctrl+Y allows exactly this edit.
        wait_for(b"Permission fixture complete.")
        wait_for(b"Execution Completed")
        assert (project / "source.txt").read_text() == "after\n"
        assert (project / "shell.txt").read_text() == "shell result"
        assert not (project / "denied.txt").exists()
        assert len(requests) == 4
        assert b"Allow shell?" not in captured
        assert b'"data":' not in captured, "Tools must use their product presentations"
        offset = len(captured)
        os.write(master, b"\x1b[Z")
        wait_for(b"[Ask]", offset)
        os.write(master, b"\x04")
        assert process.wait(timeout=10) == 0
        assert termios.tcgetattr(slave) == original
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
        project = root / "project"
        project.mkdir()
        (project / "source.txt").write_text("before\n")
        scenario(project, root / "state", f"http://127.0.0.1:{server.server_port}/v1")
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY permissions passed: Shift+Tab, preserved draft, denial, approval, Yolo and actual file/shell effects.")
