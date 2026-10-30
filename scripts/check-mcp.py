"""Exercise stdio MCP configuration, permission display and cleanup in a PTY."""
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


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        if len(requests) == 1:
            delta = {"tool_calls": [{"index": 0, "id": "mcp-call", "type": "function",
                                    "function": {"name": "mcp__demo__read", "arguments": '{}'}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": "MCP fixture complete."}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) +
                "\n\ndata: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}) +
                "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


with tempfile.TemporaryDirectory(prefix="roomkit-mcp-pty-") as directory:
    base = Path(directory)
    project = base / "project"
    project.mkdir()
    script = project / "server.py"
    script.write_text(Path("src/application/tests/mcp_server.py").read_text())
    pidfile, trace = project / "server.pid", project / "calls.txt"
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
    original = termios.tcgetattr(slave)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    env["TERM"] = "xterm-256color"
    process = subprocess.Popen([
        str(binary), str(project), "--state-dir", str(base / "state"),
        "--base-url", f"http://127.0.0.1:{server.server_port}/v1", "--model", "fixture-model",
        "--color", "never", "--no-animations", "--no-logo",
    ], stdin=slave, stdout=slave, stderr=slave, env=env)
    captured = bytearray()

    def wait_for(marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:]):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}: {bytes(captured[-4000:])!r}")
            if select.select([master], [], [], 0.1)[0]:
                captured.extend(os.read(master, 65536))

    def send(data):
        offset = len(captured)
        os.write(master, data.encode() if isinstance(data, str) else data)
        return offset

    def command(text, marker):
        wait_for(marker, send(text + "\r"))

    def dismiss():
        send(b"\x1b")
        time.sleep(0.1)

    try:
        wait_for(b"RoomKit Code")
        command("/mcp add", b"saving does not start it")
        args = json.dumps([str(script), "normal", str(pidfile), str(trace)])
        for index, value in enumerate(["demo", "python3", args, "{}", "5"]):
            send(b"\x15" + value.encode())
            if index < 4:
                send(b"\t")
        wait_for(b"saved. Open /mcp", send(b"\x13"))
        assert not pidfile.exists() and not requests
        config_path = next((base / "state/projects").glob("*/mcp.json"))
        assert config_path.stat().st_mode & 0o777 == 0o600
        assert json.loads(config_path.read_text())["servers"]["demo"]["args"] == json.loads(args)
        command("/mcp connect demo", b"Execution Completed")
        pid = int(pidfile.read_text())
        os.kill(pid, 0)
        command("/mcp show demo", b"Status: Connected")
        wait_for(b"Choose tools for this agent")
        dismiss()
        command("/mcp tools demo", b"[off] read")
        # Select the first discovered tool in the real menu.
        wait_for(b"[on] read", send(b"\r"))
        dismiss()
        command("Inspect the fixture", b"Allow mcp__demo__read?")
        assert not trace.exists()
        accepted = send(b"\x19")  # Ctrl+Y
        wait_for(b"MCP fixture complete.", accepted)
        wait_for(b"\x1b]7501;state=done", accepted)
        assert trace.read_text() == "read\n"
        declared = [t["function"]["name"] for t in requests[0]["tools"]]
        assert "mcp__demo__read" in declared and "mcp__demo__danger" not in declared
        assert "read" in declared
        # A sub-frame command can leave the same completed status pixels;
        # observe its ordered completion report, not a forced redraw.
        command("/mcp disconnect demo", b"\x1b]7501;state=done")
        try:
            os.kill(pid, 0)
            raise AssertionError("Disconnected MCP process is still alive")
        except ProcessLookupError:
            pass
        command("/mcp disable demo", b"Selected for code: none")
        dismiss()
        command("/mcp delete demo", b"Delete configuration")
        wait_for(b"configuration removed", send(b"\r"))
        assert not json.loads(config_path.read_text())["servers"]
        send(b"\x04")
        assert process.wait(timeout=10) == 0
        assert termios.tcgetattr(slave) == original
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)
        server.shutdown()
print("MCP PTY: configuration, discovery, individual selection, approval, disconnect and deletion passed.")
