"""A click on the model's reasoning, in a real POSIX PTY (RMK-1332).

The fixture provider reasons, calls a tool, then answers. In the activity view,
this application's default, a click on `∴ Thought for…` opens that block alone:
the calls below it stay as they were, grouped. A second click closes it. F2 still
opens every detail.
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

ROWS, COLS = 40, 120
REASONING = b"Weighing the record first."
ANSWER = b"Record 42 is in the second ledger."
THOUGHT = b"Thought for"
CALL_CARD = b"completed"
CONTROLS = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
PRESS, F2 = 0, b"\x1bOQ"

if os.environ.get("ROOMKIT_TEST_BINARY"):
    binary = Path(os.environ["ROOMKIT_TEST_BINARY"])
else:
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True
    ))
    binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
requests = []


class Model(BaseHTTPRequestHandler):
    """Reasons and calls `read` first, then answers."""

    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        if len(requests) == 1:
            call = {"index": 0, "id": "read-1", "type": "function",
                    "function": {"name": "read", "arguments": json.dumps({"path": "record.txt"})}}
            deltas = [({"reasoning_content": REASONING.decode()}, None), ({"tool_calls": [call]}, None),
                      ({}, "tool_calls")]
        else:
            deltas = [({"content": ANSWER.decode()}, None), ({}, "stop")]
        events = [{"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
                  for delta, finish in deltas]
        body = "".join(f"data: {json.dumps(event)}\n\n" for event in events) + "data: [DONE]\n\n"
        data = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
threading.Thread(target=server.serve_forever, daemon=True).start()
with tempfile.TemporaryDirectory() as directory:
    project, state = Path(directory) / "project", Path(directory) / "state"
    project.mkdir()
    (project / "record.txt").write_text("Record 42: ledger 2\n")
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    env["TERM"] = "xterm-256color"
    process = subprocess.Popen(
        [str(binary), str(project), "--state-dir", str(state),
         "--base-url", f"http://127.0.0.1:{server.server_port}/v1",
         "--model", "fixture", "--color", "never", "--no-animations"],
        stdin=slave, stdout=slave, stderr=slave, start_new_session=True, env=env,
    )
    captured = bytearray()

    def drawn(marker, after=0):
        return marker in CONTROLS.sub(b"", captured[after:])

    def wait_until(condition, what):
        deadline = time.monotonic() + 15
        while not condition():
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {what}; terminal: {bytes(captured[-3000:])!r}")
            if select.select([master], [], [], 0.1)[0]:
                captured.extend(os.read(master, 65536))

    def settle():
        """Read until the screen has been quiet for a moment."""
        while select.select([master], [], [], 0.3)[0]:
            captured.extend(os.read(master, 65536))

    def click_thought():
        """Click the block's line, where it was drawn last, and read the frame."""
        positions = re.findall(rb"\x1b\[(\d+);(\d+)H", captured[:captured.rfind(THOUGHT)])
        assert positions, "The block has no position"
        row = int(positions[-1][0])
        offset = len(captured)
        for release in (False, True):
            os.write(master, f"\x1b[<{PRESS};10;{row}{'m' if release else 'M'}".encode())
        settle()
        return offset

    try:
        wait_until(lambda: drawn(b"Start a conversation"), "the welcome screen")
        os.write(master, b"Find the record\r")
        wait_until(lambda: drawn(b"Execution Completed"), "the answer")
        settle()
        assert not drawn(CALL_CARD), "A call showed as a card before any click"

        offset = click_thought()
        assert drawn(REASONING, offset), "The click did not open the block"
        assert not drawn(CALL_CARD, offset), "The click opened the calls below the block too"

        offset = len(captured)
        offset = click_thought()
        assert not drawn(REASONING, offset), "The second click did not close the block"

        offset = len(captured)
        os.write(master, F2)
        settle()
        assert drawn(CALL_CARD, offset), "F2 did not open every detail"
        assert drawn(REASONING, offset), "F2 did not open the reasoning"
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)
server.shutdown()
print("Thinking click check passed: a click opens the reasoning block alone, a second closes it, "
      "and F2 still opens every detail.")
