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

from pty_checks import wait_for_exit

metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True
))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
# roomkit-tui ignores answers until a card has been shown for 500 ms.
ARMING = 0.6
requests = []
# Model requests by index: the first turn asks three approvals, the second runs in Yolo.
calls = {
    0: ("write", {"path": "denied.txt", "content": "should not exist"}),
    1: ("edit", {"path": "source.txt", "old_text": "before", "new_text": "after"}),
    2: ("shell", {"command": "printf 'shell result' > shell.txt; printf 'shell passed'"}),
    4: ("shell", {"command": "printf 'yolo result' > yolo.txt; "
                  "printf '%s|%s|%s|%s|%s' \"$SSH_AUTH_SOCK\" \"$FIXTURE_EXTRA\" \"$FIXTURE_NIX_FLAGS\" "
                  "\"${FIXTURE_NIX_TOKEN-unset}\" \"${FIXTURE_API_KEY-unset}\" > env.txt; printf 'yolo passed'"}),
}
answers = {3: "Permission fixture complete.", 5: "Yolo fixture complete."}


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        index = len(requests)
        requests.append(request)
        if index in calls:
            name, arguments = calls[index]
            delta = {"tool_calls": [{"index": 0, "id": f"call-{index}", "type": "function",
                                    "function": {"name": name, "arguments": json.dumps(arguments)}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": answers[index]}, "stop"
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
    # Commands get the SSH agent and the added names, never another host
    # variable such as a key.
    env.update(SSH_AUTH_SOCK="/roomkit-fixture/agent.sock", FIXTURE_API_KEY="leaked",
               ROOMKIT_INHERIT_ENV="FIXTURE_EXTRA,FIXTURE_NIX_*",
               FIXTURE_EXTRA="extra", FIXTURE_NIX_FLAGS="nix", FIXTURE_NIX_TOKEN="leaked")
    process = subprocess.Popen(
        [str(binary), str(project), "--state-dir", str(state), "--base-url", url,
         "--model", "fixture", "--color", "always", "--no-animations"],
        stdin=slave, stdout=slave, stderr=slave, start_new_session=True, env=env,
    )
    captured = bytearray()

    def wait_for(marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:]):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}; terminal: {bytes(captured[-3500:])!r}")
            if select.select([master], [], [], 0.1)[0]:
                captured.extend(os.read(master, 65536))

    def pause(seconds):
        deadline = time.monotonic() + seconds
        while (left := deadline - time.monotonic()) > 0:
            if select.select([master], [], [], left)[0]:
                captured.extend(os.read(master, 65536))

    def shown(marker, after):
        return marker in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:])

    try:
        wait_for(b"[Ask]")
        os.write(master, b"Preserved draft")
        wait_for(b"Preserved draft")
        offset = len(captured)
        os.write(master, b"\x1b[Z")  # Shift+Tab
        wait_for(b"[Plan]", offset)
        os.write(master, b"\x1b[Z")
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
        time.sleep(ARMING)
        os.write(master, b"\x1b[B\r")  # Select Deny and explicitly confirm.
        wait_for(b"denied")
        wait_for(b"Allow edit?")
        wait_for(b"old_text")
        assert (project / "source.txt").read_text() == "before\n"
        # A decision on screen keeps the mode: Shift+Tab only works from the composer.
        offset = len(captured)
        os.write(master, b"\x1b[Z")
        pause(ARMING)
        assert not shown(b"[Plan]", offset), "Shift+Tab must not change the mode during a decision"
        assert len(requests) == 2 and (project / "source.txt").read_text() == "before\n"
        # Click the visible Allow once choice for this exact request.
        marker = captured.rfind(b"1. Allow once")
        positions = re.findall(rb"\x1b\[(\d+);(\d+)H", captured[:marker])
        assert marker >= 0 and positions, "Approval choice is not visible"
        row = positions[-1][0]
        os.write(master, b"\x1b[<0;5;" + row + b"M\x1b[<0;5;" + row + b"m")
        wait_for(b"approved")
        wait_for(b"Allow shell?")
        time.sleep(ARMING)
        os.write(master, b"\x19")  # Ctrl+Y
        wait_for(b"Permission fixture complete.")
        wait_for(b"Execution Completed")
        assert (project / "source.txt").read_text() == "after\n"
        assert (project / "shell.txt").read_text() == "shell result"
        assert not (project / "denied.txt").exists()
        assert len(requests) == 4
        assert b'"data":' not in captured, "Tools must use their product presentations"
        # Back in the composer, Yolo runs the next shell call without asking.
        offset = len(captured)
        os.write(master, b"\x1b[Z")
        wait_for(b"[Plan]", offset)
        os.write(master, b"\x1b[Z")
        wait_for(b"[Yolo]", offset)
        os.write(master, b"Run it in Yolo\r")
        wait_for(b"Yolo fixture complete.", offset)
        assert (project / "yolo.txt").read_text() == "yolo result"
        assert (project / "env.txt").read_text() == "/roomkit-fixture/agent.sock|extra|nix|unset|unset"
        assert not shown(b"Allow shell?", offset), "Yolo must not ask before a shell call"
        assert len(requests) == 6
        offset = len(captured)
        os.write(master, b"\x1b[Z")
        wait_for(b"[Ask]", offset)
        os.write(master, b"\x04")
        assert wait_for_exit(process, master, captured) == 0
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

print("PTY permissions passed: Shift+Tab, preserved draft, armed denial and approvals, mode kept during a decision, Yolo, actual file/shell effects and the shell environment.")
