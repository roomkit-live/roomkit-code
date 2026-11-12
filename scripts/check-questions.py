"""An agent asks the person questions in a real terminal: the card shows, keys
answer it once it can have been read, and the model reads the answer; a second
question is closed with Esc, and the model reads that it should go on."""

import fcntl
from http.server import BaseHTTPRequestHandler
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

from fixture_http import LoopbackHTTPServer

metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True
))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
# roomkit-tui ignores answers until a card has been shown for 500 ms.
ARMING = 0.6
requests = []
ASK = {"questions": [
    {"header": "Database", "question": "Which database holds the sessions?",
     "options": [{"label": "SQLite (Recommended)", "description": "One file, nothing to install"},
                 {"label": "Postgres", "description": "Shared between machines"}]},
    {"header": "Checks", "question": "Which checks run before a push?", "multi_select": True,
     "options": [{"label": "Tests"}, {"label": "Lint"}, {"label": "Audit"}]},
]}
ASK_AGAIN = {"questions": [{"question": "Which port should the server use?"}]}
# Model requests by index: two questions answered, then one closed.
calls = {0: ("ask_user", ASK), 2: ("ask_user", ASK_AGAIN)}
answers = {1: "Questions answered.", 3: "Going on without the port."}


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


def tool_results(index):
    return [m["content"] for m in requests[index]["messages"] if m.get("role") == "tool"]


def scenario(project, state, url):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    process = subprocess.Popen(
        [str(binary), str(project), "--state-dir", str(state), "--base-url", url,
         "--model", "fixture", "--color", "always", "--no-animations", "--no-herdr"],
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

    try:
        wait_for(b"[Ask]")
        os.write(master, b"Set the project up\r")
        wait_for(b"Which database holds the sessions?")
        wait_for(b"1. SQLite (Recommended)")
        wait_for(b"3. Other")
        # A key pressed as the card appears answers nothing.
        os.write(master, b"2")
        time.sleep(ARMING)
        offset = len(captured)
        os.write(master, b"\x1b[B")  # Down to Postgres,
        os.write(master, b"\r")  # Enter: picked, then the next question.
        wait_for(b"Which checks run before a push?", offset)
        os.write(master, b"1")  # Tests
        os.write(master, b"3")  # Audit
        offset = len(captured)
        os.write(master, b"4")  # Own words for the second question.
        wait_for(b"Type your answer", offset)
        os.write(master, b"clippy too\r")
        wait_for(b"Questions answered.")
        read = " ".join(tool_results(1))
        assert "Database: Postgres" in read, read
        assert "Checks: Tests, Audit; in their own words: clippy too" in read, read
        # No approval was asked: asking changes nothing.
        assert b"Allow ask_user" not in captured

        offset = len(captured)
        os.write(master, b"Start the server\r")
        wait_for(b"Which port should the server use?", offset)
        time.sleep(ARMING)
        os.write(master, b"\x1b")  # Esc: closed unanswered.
        wait_for(b"Going on without the port.", offset)
        read = " ".join(tool_results(3))
        assert "closed the question without answering" in read, read
        os.write(master, b"\x04")
        assert wait_for_exit(process, master, captured) == 0
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)


server = LoopbackHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        project = root / "project"
        project.mkdir()
        scenario(project, root / "state", f"http://127.0.0.1:{server.server_port}/v1")
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY questions passed: the card shows after its arming delay, arrows and Enter choose, digits pick, own words answer, the model reads the answers, Esc closes unanswered.")
