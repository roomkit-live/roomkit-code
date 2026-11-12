"""Copy an agent reply longer than the screen with the mouse, in a real POSIX PTY.

The fixture provider answers with more paragraphs than the screen holds.
Pressing on the first, turning the wheel down with the button held and
releasing on the last copies every paragraph through OSC 52: the copy spans
about eighty lines on a thirty-row screen.
"""

import base64
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

from pty_checks import wait_for_exit

ROWS, COLS = 30, 120
PARAGRAPHS = 40
FIRST = b"Paragraph 01 of the long reply."
LAST = f"Paragraph {PARAGRAPHS} of the long reply.".encode()
COPY = re.compile(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07")
CONTROLS = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
PAGE_UP, PRESS, DRAG, WHEEL_DOWN = b"\x1b[5~", 0, 32, 65

if os.environ.get("ROOMKIT_TEST_BINARY"):
    binary = Path(os.environ["ROOMKIT_TEST_BINARY"])
else:
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True
    ))
    binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"


class Model(BaseHTTPRequestHandler):
    """Streams one long answer, a few paragraphs per chunk."""

    def log_message(self, *args):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        paragraphs = [f"Paragraph {n:02} of the long reply.\n\n" for n in range(1, PARAGRAPHS + 1)]
        chunks = ["".join(paragraphs[i:i + 5]) for i in range(0, PARAGRAPHS, 5)]
        events = [{"choices": [{"index": 0, "delta": {"content": chunk}}]} for chunk in chunks]
        events.append({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        body = "".join(f"data: {json.dumps(event)}\n\n" for event in events) + "data: [DONE]\n\n"
        data = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
with tempfile.TemporaryDirectory() as directory:
    project, state = Path(directory) / "project", Path(directory) / "state"
    project.mkdir()
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    original = termios.tcgetattr(slave)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    env["TERM"] = "xterm-256color"
    process = subprocess.Popen(
        [str(binary), str(project), "--state-dir", str(state),
         "--base-url", f"http://127.0.0.1:{server.server_port}/v1",
         "--model", "fixture", "--color", "never", "--no-animations"],
        stdin=slave, stdout=slave, stderr=slave, start_new_session=True, env=env,
    )
    captured = bytearray()

    def read(timeout):
        if select.select([master], [], [], timeout)[0]:
            captured.extend(os.read(master, 65536))

    def drawn(marker, after):
        return marker in CONTROLS.sub(b"", captured[after:])

    def wait_until(condition, what):
        deadline = time.monotonic() + 15
        while not condition():
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {what}; terminal: {bytes(captured[-4000:])!r}")
            read(0.1)

    def settle():
        """Read until the screen has been quiet for a moment."""
        while select.select([master], [], [], 0.2)[0]:
            captured.extend(os.read(master, 65536))

    def row_of(marker):
        """The row where `marker` was drawn last: the renderer positions each
        line it draws, then writes it."""
        positions = re.findall(rb"\x1b\[(\d+);(\d+)H", captured[:captured.rfind(marker)])
        assert positions, f"{marker!r} has no position"
        return int(positions[-1][0])

    def mouse(code, column, row, release=False):
        os.write(master, f"\x1b[<{code};{column};{row}{'m' if release else 'M'}".encode())

    def copied(after):
        match = COPY.search(captured, after)
        return base64.b64decode(match.group(1)).decode() if match else None

    try:
        wait_until(lambda: drawn(b"Start a conversation", 0), "the welcome screen")
        os.write(master, b"Write a long answer\r")
        wait_until(lambda: drawn(b"Execution Completed", 0), "the answer")
        settle()
        # Browse back to the first paragraph.
        for _ in range(20):
            offset = len(captured)
            os.write(master, PAGE_UP)
            settle()
            if drawn(FIRST, offset):
                break
        else:
            raise RuntimeError("Paging back never showed the first paragraph")
        start = row_of(FIRST)
        # Press on it, then turn the wheel down with the button held.
        mouse(PRESS, 1, start)
        offset = len(captured)
        for _ in range(40):
            mouse(WHEEL_DOWN, 1, start)
            settle()
        wait_until(lambda: drawn(LAST, offset), "the last paragraph after the wheel")
        end = row_of(LAST)
        mouse(DRAG, COLS - 1, end)
        settle()
        offset = len(captured)
        mouse(PRESS, COLS - 1, end, release=True)
        wait_until(lambda: copied(offset) is not None, "a clipboard write")
        text = copied(offset)
        lines = [line.strip() for line in text.splitlines() if line.strip()]
        assert FIRST.decode() in lines and LAST.decode() in lines, f"The copy lacks an end: {text!r}"
        paragraphs = sum(line.startswith("Paragraph ") for line in lines)
        assert paragraphs == PARAGRAPHS, f"The copy holds {paragraphs} paragraphs: {text!r}"
        assert len(text.splitlines()) > ROWS, "The copy should span more lines than the screen"
        os.write(master, b"\x04")
        assert wait_for_exit(process, master, captured) == 0
        assert termios.tcgetattr(slave) == original
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)
server.shutdown()
server.server_close()
thread.join(timeout=5)
print("Selection check passed: a reply longer than the screen, selected across a held wheel, copies whole.")
