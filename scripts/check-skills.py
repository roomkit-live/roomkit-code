"""Exercise skill discovery, per-agent opt-in and invocation in an isolated PTY."""
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
requests = []


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(request)
        if len(requests) == 1:
            delta = {"tool_calls": [{"index": 0, "id": "skill-call", "type": "function",
                                    "function": {"name": "activate_skill", "arguments": '{"name":"project-review"}'}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": "Skill fixture complete."}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) +
                "\n\ndata: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}) +
                "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


with tempfile.TemporaryDirectory() as directory:
    base = Path(directory)
    project = base / "project"
    project.mkdir()
    skill = project / ".agents/skills/project-review"
    skill.mkdir(parents=True)
    (skill / "SKILL.md").write_text("---\nname: project-review\ndescription: Review fixture project\n---\nBODY-ONLY-ON-ACTIVATION\n")
    server = LoopbackHTTPServer(("127.0.0.1", 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 38, 130, 0, 0))
    original = termios.tcgetattr(slave)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    process = subprocess.Popen([
        str(binary), str(project), "--state-dir", str(base / "state"),
        "--base-url", f"http://127.0.0.1:{server.server_port}/v1", "--model", "fixture-model",
        "--color", "never", "--no-animations", "--no-logo",
    ], stdin=slave, stdout=slave, stderr=slave, start_new_session=True, env=env)
    captured = bytearray()

    def wait_for(marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:]):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}: {bytes(captured[-4000:])!r}")
            if select.select([master], [], [], 0.1)[0]:
                captured.extend(os.read(master, 65536))

    try:
        wait_for(b"RoomKit Code")
        wait_for(b"Tab commands")
        os.write(master, b"/skills\r")
        wait_for(b"project-review")
        wait_for(b"disabled")
        # Do not queue the next slash while Escape is still unread: together
        # they become an Alt key sequence instead of closing the menu.
        offset = len(captured)
        os.write(master, b"\x1b")
        wait_for(b"Tab commands", offset)
        os.write(master, b"/skills project-review\r")
        wait_for(b"Enable for this agent")
        assert not requests
        offset = len(captured)
        os.write(master, b"\r")  # Enable, without running the model.
        wait_for(b"Use in this conversation", offset)
        assert not requests
        os.write(master, b"\r")  # Request the skill through the ordinary engine path.
        wait_for(b"Skill fixture complete.")
        wait_for(b"Execution Completed")
        assert len(requests) == 2
        prompt = "\n".join(m["content"] for m in requests[0]["messages"] if m["role"] == "system")
        assert "project-review" in prompt and "BODY-ONLY-ON-ACTIVATION" not in prompt
        assert "BODY-ONLY-ON-ACTIVATION" in json.dumps(requests[1])
        offset = len(captured)
        os.write(master, b"/skills disable project-review\r")
        wait_for(b"Enable for this agent", offset)
        agents = next((base / "state/projects").glob("*/agents.json"))
        assert json.loads(agents.read_text())["agents"]["code"]["skills"] == []
        offset = len(captured)
        os.write(master, b"\x1b")
        wait_for(b"Tab commands", offset)
        os.write(master, b"\x04")
        wait_for_exit(process, master, captured)
        assert process.returncode == 0
        assert termios.tcgetattr(slave) == original
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        server.shutdown()
        server.server_close()
        os.close(master)
        os.close(slave)
print("Skill menus, opt-in, progressive invocation, persistence and terminal cleanup passed")
