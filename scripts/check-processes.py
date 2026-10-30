"""Exercise live process controls while a provider is busy, in a real POSIX PTY."""

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
provider_waiting = threading.Event()
release_provider = threading.Event()


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        index = len(requests)
        if index == 3:
            provider_waiting.set()
            if not release_provider.wait(20):
                self.send_error(504)
                return
        if index == 1:
            delta = {"tool_calls": [{"index": 0, "id": "background-job", "type": "function", "function": {
                "name": "process_start", "arguments": json.dumps({
                    "command": "printf 'worker ready\n'; printf 'diagnostic stream\n' >&2; cat",
                })
            }}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": "Background fixture started." if index == 2 else "Provider work finished."}, "stop"
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
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
    original = termios.tcgetattr(slave)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    env["TERM"] = "xterm-256color"
    process = subprocess.Popen(
        [str(binary), str(project), "--state-dir", str(state), "--base-url", url,
         "--model", "fixture", "--mode", "yolo", "--color", "always", "--no-animations"],
        stdin=slave, stdout=slave, stderr=slave, env=env,
    )
    captured = bytearray()

    def wait_for(marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:]):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}; terminal: {bytes(captured[-4000:])!r}")
            if select.select([master], [], [], 0.1)[0]:
                captured.extend(os.read(master, 65536))

    def click(label, column):
        marker = captured.rfind(label)
        positions = re.findall(rb"\x1b\[(\d+);(\d+)H", captured[:marker])
        assert positions, "Control has no visible position"
        row = int(positions[-1][0])
        os.write(master, f"\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m".encode())

    try:
        wait_for(b"Start a conversation")
        os.write(master, b"Start a background worker\r")
        wait_for(b"Background fixture started.")
        wait_for(b"Processes: 1 running")
        offset = len(captured)
        wait_for("› Processes".encode())
        click("› Processes".encode(), 110)
        wait_for(b"Background processes", offset)
        wait_for(b"owner code", offset)
        os.write(master, b"\x1b")
        wait_for(b"Enter send", offset)
        offset = len(captured)
        os.write(master, b"Continue working\r")
        assert provider_waiting.wait(5), "Provider fixture did not receive the second turn"
        os.write(master, b"Preserved draft")
        wait_for(b"Preserved draft", offset)
        offset = len(captured)
        os.write(master, b"\x1b[17~")  # F6 while the provider has not answered.
        wait_for(b"Background processes", offset)
        os.write(master, b"\r")
        wait_for(b"Following latest output", offset)
        wait_for(b"stdout", offset)
        wait_for(b"stderr", offset)
        wait_for(b"worker ready", offset)
        wait_for(b"diagnostic stream", offset)
        # Resize cannot remove the action controls or touch the hidden draft.
        offset = len(captured)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 72, 0, 0))
        wait_for(b"Stop process", offset)
        offset = len(captured)
        click(b"Stop process", 5)
        wait_for(b"stopped", offset)
        assert not release_provider.is_set(), "Stop waited for the provider"
        assert len(requests) == 3, "Human process controls triggered a model call"
        os.write(master, b"\x1b")
        wait_for(b"Enter send", offset)
        offset = len(captured)
        os.write(master, b"!")
        wait_for(b"Preserved draft!", offset)
        release_provider.set()
        wait_for(b"Provider work finished.", offset)
        offset = len(captured)
        os.write(master, b"\x7f" * len("Preserved draft!") + b"/processes\r")
        wait_for(b"Background processes", offset)
        wait_for(b"No running processes", offset)
        assert b"Processes: 1 finished" not in captured[offset:]
        if os.environ.get("ROOMKIT_PROCESS_CAPTURE"):
            Path(os.environ["ROOMKIT_PROCESS_CAPTURE"]).write_bytes(captured)
        os.write(master, b"\x1b\x04")
        assert process.wait(5) == 0
        assert termios.tcgetattr(slave) == original, "Terminal state not restored"
        assert b"\x1b]52;c;" not in captured, "Clicks unexpectedly copied text"
    finally:
        release_provider.set()
        if process.poll() is None:
            process.kill()
            process.wait(5)
        os.close(master)
        os.close(slave)


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory() as directory:
        project = Path(directory) / "project"
        project.mkdir()
        scenario(project, Path(directory) / "state", f"http://127.0.0.1:{server.server_port}/v1")
finally:
    server.shutdown()
    server.server_close()
print("Process PTY: lifecycle, click/F6/command, busy stop, resize, draft and cleanup passed.")
