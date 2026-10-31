"""Exercise the Files sidebar section and /files in a real terminal."""
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
import unicodedata

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
requests = []
# One model reply per request: a file tool, a shell command, then the answer.
replies = [
    ("write", {"path": "notes.md", "content": "# Notes\n", "_roomkit_intent": "I write the notes"}),
    ("shell", {"command": "printf made > made.txt", "_roomkit_intent": "I create a file from the shell"}),
    "Files fixture complete.",
]


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        index = len(requests) - 1
        reply = replies[min(index, len(replies) - 1)]
        if isinstance(reply, tuple):
            name, args = reply
            delta = {"tool_calls": [{"index": 0, "id": f"call-{index}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": reply}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}) + "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class App:
    def __init__(self, argv, storage):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 160, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env.update({"TERM": "xterm-256color", "HOME": str(storage / "home"), "XDG_DATA_HOME": str(storage / "data")})
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
                raise AssertionError(f"Missing {text!r}: {self.output[-6000:]!r}")
            if select.select([self.master], [], [], 0.02)[0]:
                self.output.extend(os.read(self.master, 65536))

    def settle(self):
        """Read until the terminal has been quiet for a moment: a whole frame."""
        while select.select([self.master], [], [], 0.3)[0]:
            self.output.extend(os.read(self.master, 65536))

    def sidebar(self):
        """The sidebar column of the last drawn screen, row by row."""
        rows, cols = 48, 160
        grid = [[" "] * cols for _ in range(rows)]
        row = col = 0
        data = self.output.decode(errors="replace")
        index = 0
        while index < len(data):
            osc = re.match(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", data[index:])
            if osc:
                index += len(osc.group(0))
                continue
            csi = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", data[index:])
            if csi:
                args, command = csi.groups()
                if command == "H":
                    position = [int(x) for x in args.split(";") if x.isdigit()] or [1, 1]
                    row, col = position[0] - 1, (position[1] - 1 if len(position) > 1 else 0)
                elif command == "K" and args == "2":
                    grid[row] = [" "] * cols
                index += len(csi.group(0))
                continue
            char = data[index]
            index += 1
            if char == "\x1b" or char in "\r\n" or not (0 <= row < rows and 0 <= col < cols):
                continue
            grid[row][col] = char
            col += 2 if unicodedata.east_asian_width(char) in "WF" else 1
        return ["".join(line).rsplit("│", 1)[-1].strip() for line in grid if "│" in "".join(line)]

    def close(self):
        try:
            self.send("\x04")
            assert self.process.wait(timeout=10) == 0, "Clean shutdown failed"
            assert termios.tcgetattr(self.slave) == self.original
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            os.close(self.master)
            os.close(self.slave)


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix="roomkit-files-") as directory:
        root = Path(directory)
        project, storage = root / "project", root / "state"
        project.mkdir()
        storage.mkdir()
        url = f"http://127.0.0.1:{server.server_port}/v1"
        app = App([str(binary), str(project), "--state-dir", str(storage), "--base-url", url, "--model", "fixture",
                   "--mode", "yolo", "--color", "never", "--no-animations", "--no-herdr", "--no-terminal-status"], storage)
        app.wait("[Yolo]")
        offset = app.send("Change the files\r")
        app.wait("Files fixture complete.", offset)
        assert (project / "made.txt").read_text() == "made"
        # The sidebar names each file with how it changed, the shell's included.
        # The transcript shows these paths too: read the sidebar column itself.
        app.wait("Created · shell", offset)
        app.settle()
        rows = app.sidebar()
        files = rows[rows.index("› Files") + 1:][:4]
        assert files == ["› made.txt", "Created · shell", "› notes.md", "Created"], rows
        offset = app.send("/files\r")
        app.wait("Files · this conversation", offset)
        app.wait("Changed (2)", offset)
        offset = app.send("\x1b")
        app.wait("[Yolo]", offset)
        app.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY files passed: the sidebar lists tool and shell changes, /files opens the full list.")
