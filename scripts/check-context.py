"""Exercise the context breakdown in the sidebar of a real terminal."""
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

from pty_checks import wait_for_exit

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
requests = []
# A turn whose request measured 40,000 tokens of a 131,072-token window, then
# a request the server refuses past that window, as vLLM does.
REFUSED = object()
replies = ["Context fixture complete.", REFUSED]
REFUSAL = {"error": {"object": "error", "type": "BadRequestError", "code": 400,
                     "message": "This model's maximum context length is 131072 tokens. However, you requested 131521 tokens."}}
usage = {"prompt_tokens": 40000, "completion_tokens": 40, "prompt_tokens_details": {"cached_tokens": 0}}


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        index = len(requests) - 1
        reply = replies[min(index, len(replies) - 1)]
        if reply is REFUSED:
            body = json.dumps(REFUSAL).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if isinstance(reply, tuple):
            name, args = reply
            delta = {"tool_calls": [{"index": 0, "id": f"call-{index}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": reply}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [], "usage": usage}) + "\n\ndata: [DONE]\n\n").encode()
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
            assert wait_for_exit(self.process, self.master, self.output) == 0, "Clean shutdown failed"
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
    with tempfile.TemporaryDirectory(prefix="roomkit-context-") as directory:
        root = Path(directory)
        project, storage = root / "project", root / "state"
        project.mkdir()
        storage.mkdir()
        url = f"http://127.0.0.1:{server.server_port}/v1"
        app = App([str(binary), str(project), "--state-dir", str(storage), "--base-url", url, "--model", "fixture",
                   "--mode", "yolo", "--context-window", "131072", "--color", "never", "--no-animations", "--no-herdr", "--no-terminal-status"], storage)
        app.wait("[Yolo]")
        offset = app.send("Show the context\r")
        app.wait("Context fixture complete.", offset)
        app.wait("Messages", offset)
        app.settle()
        rows = app.sidebar()
        section = rows[rows.index("Context") + 1:]
        section = section[:section.index("")] if "" in section else section
        legend = [row.split()[1] for row in section if row[:1] in "█▓▒░▄" and row[1:2] == " "]
        # Each part the host estimates, then the measured remainder.
        assert legend[:3] == ["System", "Instructions", "Tools"] and legend[-1] == "Messages", section
        assert any(row.startswith("█ System prompt  ≈") for row in section), section
        assert "40040 / 131072 tokens" in section, section
        # The next request is refused past the window, and the conversation is
        # too short for the kit to compact: the notice names the context limit,
        # and the sidebar says the request was refused above the last report.
        offset = app.send("Read everything\r")
        app.wait("Disconnect tools", offset)
        app.settle()
        assert "context limit" in app.screen(offset)
        assert "Engine execution failed" not in app.screen(offset)
        assert "Start a new session" not in app.screen(offset)
        rows = app.sidebar()
        section = rows[rows.index("Context") + 1:]
        assert section[:3] == ["Refused: over capacity", "Last report", "40040 / 131072 tokens"], section
        app.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY context passed: the sidebar splits the measured context into estimated parts and messages,"
      " and a request refused past the window says so above the last report.")
