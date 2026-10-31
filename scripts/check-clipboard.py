"""Optional Linux clipboard + PTY check on a private Xvfb display.

Requires Xvfb and xclip. Never accesses the developer's desktop clipboard.
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
import shutil
import struct
import subprocess
import tempfile
import termios
import threading
import time
import zlib

for command in ("Xvfb", "xclip"):
    if not shutil.which(command):
        raise SystemExit(f"Install {command} for the optional isolated clipboard check")
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
requests = []


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": {"content": "Image received."}}]})
                + '\n\ndata: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n').encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(bytes([0, 255, 0, 0, 255]))) + chunk(b"IEND", b""))
read_fd, write_fd = os.pipe()
xvfb = subprocess.Popen(["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "640x480x24", "-nolisten", "tcp"], pass_fds=(write_fd,), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
os.close(write_fd)
owner = process = None
server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
master = slave = None
try:
    if not select.select([read_fd], [], [], 10)[0]:
        raise RuntimeError("Private X server did not start")
    display = os.read(read_fd, 32).strip().decode()
    assert display.isdigit()
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_")) and k not in {"DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY"}}
    env["DISPLAY"] = ":" + display
    env["TERM"] = "xterm-256color"
    owner = subprocess.Popen(["xclip", "-quiet", "-selection", "clipboard", "-t", "image/png", "-i"], env=env, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    owner.stdin.write(png)
    owner.stdin.close()
    deadline = time.monotonic() + 10
    while True:
        pasted = subprocess.run(["xclip", "-selection", "clipboard", "-t", "image/png", "-o"], env=env, capture_output=True, timeout=2)
        if pasted.returncode == 0 and pasted.stdout == png:
            break
        if time.monotonic() >= deadline:
            raise RuntimeError("Private image clipboard did not become ready")
        time.sleep(0.05)
    with tempfile.TemporaryDirectory() as directory:
        project, state = Path(directory) / "project", Path(directory) / "state"
        project.mkdir()
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 35, 130, 0, 0))
        original = termios.tcgetattr(slave)
        process = subprocess.Popen([str(binary), str(project), "--state-dir", str(state), "--base-url", f"http://127.0.0.1:{server.server_port}/v1", "--model", "vision-fixture", "--color", "never", "--no-animations"], env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        captured = bytearray()

        def wait_for(marker, after=0):
            deadline = time.monotonic() + 15
            while marker not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:]):
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"Missing {marker!r}: {bytes(captured[-3000:])!r}")
                if select.select([master], [], [], 0.1)[0]:
                    captured.extend(os.read(master, 65536))

        wait_for(b"vision-fixture")
        os.write(master, b"/agents edit:code\r")
        wait_for(b"Agent profile")
        offset = len(captured)
        os.write(master, b"\t" * 8 + b"\x1b[C\x13")
        wait_for(b"Tab commands", offset)
        offset = len(captured)
        os.write(master, b"\x16")
        wait_for(b"Image 1", offset)
        assert not requests, "Pasting submitted to the model"
        offset = len(captured)
        os.write(master, b"\x1b\x7f")  # Alt+Backspace removes it.
        os.write(master, b"\x16")
        wait_for(b"Image 1", offset)
        os.write(master, b"Describe this image.\r")
        wait_for(b"Image received.")
        wait_for(b"Execution Completed")
        assert len(requests) == 1
        parts = [p for m in requests[0]["messages"] if isinstance(m["content"], list) for p in m["content"] if p["type"] == "image_url"]
        assert len(parts) == 1, "Removed image remained attached"
        encoded = parts[0]["image_url"]["url"].split(",", 1)[1]
        assert base64.b64decode(encoded).startswith(b"\x89PNG\r\n\x1a\n")
        assert encoded.encode() not in captured
        assert list(project.iterdir()) == [], "Attachment entered the workspace"
        files = list(state.glob("projects/*/attachments/*/*.png"))
        assert len(files) == 1 and files[0].stat().st_mode & 0o777 == 0o600
        os.write(master, b"\x04")
        assert process.wait(timeout=10) == 0
        assert termios.tcgetattr(slave) == original
finally:
    for child in (process, owner, xvfb):
        if child is not None and child.poll() is None:
            child.kill()
            child.wait(timeout=5)
    for fd in (read_fd, master, slave):
        if fd is not None:
            os.close(fd)
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
print("Isolated clipboard check passed: agent opt-in, Ctrl+V, removal, PNG upload, provider input, private storage and terminal restoration.")
