"""Exercise provider/agent forms and secret handling in a real isolated PTY."""

import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time
import unicodedata

if os.environ.get("ROOMKIT_TEST_BINARY"):
    binary = Path(os.environ["ROOMKIT_TEST_BINARY"])
else:
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True
    ))
    binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"

requests = []
secret = "fixture-private-key"


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        assert self.path == "/v1/models"
        assert self.headers.get("Authorization") == f"Bearer {secret}"
        data = json.dumps({"data": [{"id": "fixture-discovered"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        assert self.path == "/v1/chat/completions"
        assert self.headers.get("Authorization") == f"Bearer {secret}"
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(request)
        model = request["model"]
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": {"content": f"Answer from {model}."}}]}) +
                '\n\ndata: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n').encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()

with tempfile.TemporaryDirectory(prefix="roomkit-profiles-") as directory:
    root = Path(directory)
    workspace = root / "workspace"
    workspace.mkdir()
    state = root / "state"
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 110, 0, 0))
    original = termios.tcgetattr(slave)
    env = {key: value for key, value in os.environ.items() if not key.startswith(("ROOMKIT_", "HERDR_"))}
    env["TERM"] = "xterm-256color"
    process = subprocess.Popen([str(binary), str(workspace), "--state-dir", str(state), "--no-animations"], stdin=slave, stdout=slave, stderr=slave, start_new_session=True, env=env)
    captured = bytearray()

    def pump(duration=0.05):
        if select.select([master], [], [], duration)[0]:
            captured.extend(os.read(master, 65536))

    def wait_for(marker, after=0):
        end = time.monotonic() + 12
        while marker not in re.sub(rb"\x1b\[[0-9;:]*[A-Za-z]", b"", captured[after:]):
            if time.monotonic() >= end or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}: {bytes(captured[-2500:])!r}")
            pump()

    def send(data):
        offset = len(captured)
        os.write(master, data.encode() if isinstance(data, str) else data)
        return offset

    def command(text, marker):
        offset = send(text + "\r")
        wait_for(marker, offset)

    def after_turn(text, marker):
        """Send command `text` after a turn. The conversation stays busy a moment
        after its end is shown, and refuses a command sent in that window: send
        it again then, and only then, as anything else would type into the view."""
        for _ in range(100):
            offset = send(text + "\r")
            end = time.monotonic() + 12
            while True:
                shown = re.sub(rb"\x1b\[[0-9;:]*[A-Za-z]", b"", captured[offset:])
                if marker in shown:
                    return
                if b"still executing" in shown:
                    break
                if time.monotonic() >= end or process.poll() is not None:
                    raise RuntimeError(f"Missing {marker!r}: {bytes(captured[-2500:])!r}")
                pump()
        raise RuntimeError(f"The conversation stayed busy before {text!r}")

    def answered(text, count, answer):
        """Send `text`, then wait until the model received request `count` and
        its answer shows below the message: a repaint can redraw an earlier,
        identical answer before this one exists."""
        send(text + "\r")
        end = time.monotonic() + 12
        while True:
            drawn = screen()
            below = drawn.rsplit(text, 1)[1] if text in drawn else ""
            if len(requests) >= count and answer in below and "Working" not in below:
                return
            if time.monotonic() >= end or process.poll() is not None:
                raise RuntimeError(f"No answer to {text!r}: {drawn}")
            pump()

    def screen(rows=28, cols=110):
        """The screen as drawn so far, one string per row."""
        cells = [[" "] * cols for _ in range(rows)]
        row = col = 0
        data = captured.decode(errors="replace")
        index = 0
        while index < len(data):
            osc = re.match(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", data[index:])
            if osc:
                index += len(osc.group(0))
                continue
            csi = re.match(r"\x1b\[([0-9;?<]*)([A-Za-z])", data[index:])
            if csi:
                args, command = csi.groups()
                if command == "H":
                    position = [int(x) for x in args.split(";") if x.isdigit()] or [1, 1]
                    row, col = position[0] - 1, (position[1] - 1 if len(position) > 1 else 0)
                elif command == "K" and args == "2":
                    cells[row] = [" "] * cols
                index += len(csi.group(0))
                continue
            char = data[index]
            index += 1
            if char == "\x1b" or char in "\r\n" or not (0 <= row < rows and 0 <= col < cols):
                continue
            cells[row][col] = char
            col += 2 if unicodedata.east_asian_width(char) in "WF" else 1
        return "\n".join("".join(line) for line in cells)

    def fields(values):
        for index, value in enumerate(values):
            if value is None:
                pass  # Preserve a choice or existing value.
            elif value == "<right>":
                send(b"\x1b[C")
            else:
                send(b"\x15" + value.encode())  # Ctrl+U: replace this field only.
            if index + 1 < len(values):
                send(b"\t")
        pump()

    def save():
        offset = send(b"\x13")  # Ctrl+S, with terminal flow control disabled.
        wait_for(b"Tab commands", offset)

    def add_agent(identifier, label, model):
        after_turn("/agents", "Agents · global".encode())
        offset = send(b"\r")
        wait_for(b"Agent profile", offset)
        fields([identifier, label, None, model, "32768", None, None, "Fixture instructions", "read"])
        save()
        wait_for(model.encode())

    try:
        wait_for(b"RoomKit Code")
        # Without an agent, the setup notice stays on the welcome screen.
        wait_for(b"Configure a connection")
        pump(0.3)
        drawn = screen()
        assert "RoomKit Code · Start a conversation" in drawn and "Configure a connection" in drawn, drawn
        command("/providers", b"Providers")
        # Account discovery is a local menu, not an implicit network login.
        send("Sign in with ChatGPT")
        offset = send(b"\r")
        wait_for(b"ChatGPT accounts", offset)
        wait_for(b"Connect a ChatGPT account", offset)
        assert not (state / "auth" / "accounts.json").exists()
        offset = send(b"\x1b")
        wait_for(b"Tab commands", offset)
        command("/providers", b"Providers")
        offset = send(b"\r")
        wait_for(b"Provider connection", offset)
        fields(["fixture", "Fixture provider", f"http://127.0.0.1:{server.server_port}/v1", "<right>", secret, "", "30"])
        wait_for(b"Timeout (seconds) *: 30")
        assert secret.encode() not in captured, "Secret appeared in terminal output"
        # Saving an invisible form is disabled until the terminal is large enough.
        offset = len(captured)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 10, 20, 0, 0))
        os.kill(process.pid, signal.SIGWINCH)
        wait_for(b"Enlarge terminal", offset)
        send(b"\x13")
        for _ in range(5): pump()
        assert not (state / "connections.json").exists()
        offset = len(captured)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 110, 0, 0))
        os.kill(process.pid, signal.SIGWINCH)
        wait_for(b"Provider connection", offset)
        save()
        connections = json.loads((state / "connections.json").read_text())
        assert connections["profiles"]["fixture"]["authentication"]["key"] == secret
        assert (state / "connections.json").stat().st_mode & 0o777 == 0o600
        add_agent("first", "First agent", "fixture-a")
        command("Hello first", b"Answer from fixture-a.")
        wait_for(b"Execution Completed")
        add_agent("second", "Second agent", "fixture-b")
        offset = len(captured)
        command("Hello second", b"Answer from fixture-b.")
        wait_for(b"Execution Completed", offset)
        after_turn("/agents", "Agents · global".encode())
        offset = send("First agent")
        wait_for(b"Filter: First agent", offset)
        offset = send(b"\r")
        wait_for(b"Select agent", offset)
        offset = send(b"\r")
        wait_for(b"Enter send", offset)
        answered("Hello again", 3, "Answer from fixture-a.")
        assert [r["model"] for r in requests] == ["fixture-a", "fixture-b", "fixture-a"]
        assert "Answer from fixture-a." in json.dumps(requests[1]["messages"])
        # Each agent declares its one tool, beside the engine's own
        # read_stored_result (roomkit-rs RMK-1228) and delegation (RMK-1532).
        engine = {"read_stored_result", "delegate_task", "task_status", "cancel_task"}
        assert all(len([t for t in r["tools"] if t["function"]["name"] not in engine]) == 1
                   for r in requests)
        # Reopening a secret never reveals it; Escape does not change storage.
        before = (state / "connections.json").read_bytes()
        after_turn("/providers", b"Providers")
        send("Fixture provider")
        wait_for(b"Filter: Fixture provider")
        offset = send(b"\r")
        wait_for(b"Edit connection", offset)
        offset = send(b"\r")
        wait_for(b"Provider connection", offset)
        send(b"\x15cancelled-id")
        offset = send(b"\x1b")
        wait_for(b"Enter send", offset)
        assert (state / "connections.json").read_bytes() == before
        # Discover models, open a prefilled agent form, and cancel without creating it.
        command("/providers", b"Providers")
        send("Fixture provider")
        offset = send(b"\r")
        wait_for(b"Edit connection", offset)
        send("Test connection")
        offset = send(b"\r")
        wait_for(b"Connection successful", offset)
        offset = send(b"\r")
        wait_for(b"Agent profile", offset)
        wait_for(b"fixture-discovered", offset)
        offset = send(b"\x1b")
        # The form is closed only once the composer shows again.
        wait_for(b"Enter send", offset)
        assert secret.encode() not in captured
        # Agents are global by default: shared by every project, selected here.
        agents = json.loads((state / "agents.json").read_text())
        assert len(agents["agents"]) == 2
        assert agents["selected"] == "first"
        project = json.loads(next(state.glob("projects/*/agents.json")).read_text())
        assert not project["agents"] and project["selected"] == "first"
        # Secrets are neither messages nor tool records, even in SQLite's raw pages.
        assert secret.encode() not in next(state.glob("projects/*/history.sqlite3")).read_bytes()
        # List aliases and profile menus expose deletion, with Cancel selected first.
        command("/providers delete fixture", b"Provider is used by")
        command("/agents list", "Agents · global".encode())
        send("Second agent")
        offset = send(b"\r")
        wait_for(b"Select agent", offset)
        send("Delete agent")
        offset = send(b"\r")
        wait_for(b"Delete Second agent", offset)
        offset = send(b"\r")  # Default is Cancel.
        wait_for(b"Select agent", offset)
        assert "second" in json.loads((state / "agents.json").read_text())["agents"]
        offset = send(b"\x1b")
        wait_for(b"Enter send", offset)
        for identifier, label in [("second", "Second agent"), ("first", "First agent")]:
            command(f"/agents delete {identifier}", f"Delete {label}".encode())
            send(b"\x1b[B")
            offset = send(b"\r")
            wait_for(b"Enter send", offset)
            assert identifier not in json.loads((state / "agents.json").read_text())["agents"]
        command("/providers delete fixture", b"Delete Fixture provider")
        send(b"\x1b[B")
        offset = send(b"\r")
        wait_for(b"Providers", offset)
        assert not json.loads((state / "connections.json").read_text())["profiles"]
        offset = send(b"\x1b")
        wait_for(b"Enter send", offset)
        send(b"\x04")
        process.wait(timeout=10)
        assert process.returncode == 0
        assert termios.tcgetattr(slave) == original
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)
        server.shutdown()
        server.server_close()

print("Profile PTY checks passed: account menus, bootstrap, masked credentials, persistent forms, two agents, shared history, discovery, listing, confirmed deletion and cancellation.")
