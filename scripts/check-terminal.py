"""Run the real application in a POSIX PTY with a local model fixture."""

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
continue_response = threading.Event()


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(request)
        if len(requests) == 3:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            partial = {"choices": [{"index": 0, "delta": {"content": "Partial answer kept."}}]}
            self.wfile.write(("data: " + json.dumps(partial) + "\n\n").encode())
            self.wfile.flush()
            continue_response.wait(timeout=15)
            return
        if len(requests) == 1:
            delta = {"tool_calls": [{"index": 0, "id": "read-1", "type": "function", "function": {
                "name": "read", "arguments": json.dumps({"path": "fixture.txt", "_roomkit_intent": "Reading the fixture document"})
            }}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": '\n\n## Inspection\n\nFixture **inspection** complete.\n\n- Read `fixture.txt`\n- Read-only\n\n```json\n{"ok": true, "count": 1}\n```\n\nLatest answer marker.\n'}, "stop"
        body = ("data: " + json.dumps({"choices": [{"index": 0, "delta": delta}]}) + "\n\n"
                + "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
                + "\n\ndata: " + json.dumps({"choices":[], "usage": {
                    "prompt_tokens":1000 if len(requests)==1 else 2000,
                    "completion_tokens":20 if len(requests)==1 else 50,
                    "prompt_tokens_details":{"cached_tokens":500}}})
                + "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass  # The interruption scenario deliberately closes the response.



def scenario(project, state, base_url, resume=False, color="never"):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
    original = termios.tcgetattr(slave)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    env["NO_COLOR"] = "1" # Explicit always must override this; auto/never must not.
    args = [str(binary), str(project), "--state-dir", str(state), "--base-url", base_url,
            "--model", "fixture-model", "--context-window", "32768", "--color", color, "--no-animations"]
    if resume:
        args += ["--native-selection", "--no-logo"]
    else:
        args += ["--session", "fixture"]
    process = subprocess.Popen(args, stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True)
    captured = bytearray()

    def wait_for(marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:]):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}; terminal output: {bytes(captured[-3000:])!r}")
            if select.select([master], [], [], min(0.2, max(0, deadline - time.monotonic())))[0]:
                captured.extend(os.read(master, 65536))

    try:
        wait_for(b"fixture-model")
        wait_for(b"RoomKit Code")
        wait_for("› Ask about this project…".encode())
        wait_for(b"Start a conversation")
        assert b"Fixture inspection complete." not in captured, "Launch unexpectedly resumed the previous session"
        if not resume:
            offset = len(captured)
            os.write(master, b"/tools\r")
            wait_for(b"Tools available to code", offset)
            wait_for(b"29 registered", offset)
            os.write(master, b"shell")
            wait_for(b"Filter: shell", offset)
            os.write(master, b"\r")
            wait_for(b"shell is registered", offset)
            assert not requests, "Tool inspection must not contact the model"
            os.write(master, b"Read fixture.txt\r")
        else:
            offset = len(captured)
            os.write(master, b"/resume\r")
            wait_for(b"Resume session", offset)
            assert b"Empty conversation" not in captured[offset:], "Unused drafts leaked into the picker"
            # Filtering is local; an unmatched query and Escape never call the model.
            os.write(master, b"no-such-session")
            wait_for(b"No matching choices", offset)
            offset = len(captured)
            os.write(master, b"\x1b")
            wait_for(b"Tab commands", offset)
            os.write(master, b"/resume\r")
            wait_for(b"Resume session", offset)
            os.write(master, b"review")
            wait_for(b"Filter: review", offset)
            os.write(master, b"\r")
        wait_for(b"Fixture inspection complete.")
        assert b"person:local-user" not in captured, "Own messages must not expose the technical author label"
        # The first exchange leaves welcome and exposes the sidebar. Hide it
        # for the existing full-width history/footer checks; keep that preference
        # through /new and /resume.
        wait_for(b"F7 hide")
        # Lower sections scroll on a 30-row terminal; the context legend shows.
        wait_for(b"System prompt")
        offset = len(captured)
        os.write(master, b"\x1b[18~")
        wait_for(b"F7 sidebar", offset)
        wait_for(b"Read-only")
        if resume:
            # Recall comes from durable messages, not the /resume commands typed
            # in this new process. A suffix distinguishes the composer from old output.
            offset = len(captured)
            os.write(master, b"\x1b[A\x1b[AHISTORY_RECALL_PROBE")
            wait_for(b"Check the context again.HISTORY_RECALL_PROBE", offset)
            offset = len(captured)
            os.write(master, b"\x1b[B\x1b[B")
            wait_for("› Ask about this project…".encode(), offset)
        # Activity keeps its heading and formatted preview; F2 reveals the full result.
        os.write(master, b"\x1b[5~\x1b[1;3A")
        wait_for(b"Reading the fixture document")
        wait_for('fixture.txt · lines 1–4'.encode())
        wait_for(b"A fictional project fixture.")
        assert b"Expanded fixture line" not in captured
        offset = len(captured)
        os.write(master, b"\x1bOQ")
        wait_for(b"F2 collapse", offset)
        wait_for(b"Expanded fixture line", offset)
        assert b'"data":' not in captured, "The file card leaked its raw JSON envelope"
        assert b"**inspection**" not in captured, "Markdown was displayed as raw syntax"
        offset = len(captured)
        # Closing details follows the latest reply; revisit the old activity.
        os.write(master, b"\x1bOQ\x1b[1;3A")
        wait_for(b"Reading the fixture document", offset)
        os.write(master, b"\x1b[1;5F")
        if not resume:
            wait_for(b"Execution Completed")
            # A name leads the header, before the workspace path; the resume
            # picker of the second run filters on that name only.
            offset = len(captured)
            os.write(master, b"/rename fixture review\r")
            wait_for("fixture review · ".encode(), offset)

        os.write(master, b"Unsent draft\x1b[1;5F")
        wait_for(b"Latest answer marker.")
        wait_for(b"context 2050/32768")
        offset = len(captured)
        os.write(master, b"\x1b[18~")
        wait_for(b"2050 / 32768 tokens", offset)
        wait_for(b"6.3% used", offset)
        offset = len(captured)
        os.write(master, b"\x1b[18~")
        wait_for(b"context 2050/32768", offset)
        offset = len(captured)
        os.write(master, b"\x1b[5~") # PageUp
        wait_for(b"Back to bottom", offset)
        offset = len(captured)
        os.write(master, b"\x1b[1;5F") # Ctrl+End
        wait_for(b"Latest answer marker.", offset)
        if not resume:
            offset = len(captured)
            os.write(master, b"\x1b[<64;30;10M") # Wheel up
            wait_for(b"Back to bottom", offset)
            marker = captured.rfind(b"Back to bottom")
            positions = re.findall(rb"\x1b\[(\d+);(\d+)H", captured[:marker])
            assert positions, "Return-to-bottom control was not positioned"
            row = positions[-1][0]
            offset = len(captured)
            # The badge reaches the right edge of the 120-column frame.
            os.write(master, b"\x1b[<0;115;" + row + b"M\x1b[<0;115;" + row + b"m")
            wait_for(b"Latest answer marker.", offset)
            assert b"\x1b]52;c;" not in captured, "Navigation unexpectedly requested a clipboard write"
        # Trigger an editor redraw so a lost or submitted draft is detectable.
        offset = len(captured)
        os.write(master, b"!")
        wait_for(b"Unsent draft!", offset)
        if not resume:
            offset = len(captured)
            os.write(master, b"\x7f" * len("Unsent draft!") + b"/new\r")
            wait_for(b"Start a conversation", offset)
            os.write(master, b"/resume\r")
            wait_for(b"Resume session", offset)
            os.write(master, b"fixture")
            wait_for(b"Filter: fixture", offset)
            offset = len(captured)
            os.write(master, b"\r")
            wait_for(b"Latest answer marker.", offset)
            # Hold the next provider response until the prior context is
            # visibly labeled. It must not disappear during a working turn.
            offset = len(captured)
            os.write(master, b"Check the context again.\r")
            wait_for(b"context (last report) 2050/32768", offset)
            # Cancel a real pending HTTP generation; the host must join it
            # without closing the engine or consuming the unsent draft.
            wait_for(b"Partial answer kept.", offset)
            os.write(master, b"Preserved after stop")
            wait_for(b"Preserved after stop")
            offset = len(captured)
            os.write(master, b"\x1b")
            wait_for(b"Execution Interrupted", offset)
            continue_response.set()
            offset = len(captured)
            os.write(master, b"!")
            wait_for(b"Preserved after stop!", offset)
            os.write(master, b"\x03")
            wait_for(b"Press Ctrl+C again to exit", offset)
            assert process.poll() is None, "First Ctrl+C exited"
            offset = len(captured)
            os.write(master, b"Continue after interruption.\r")
            wait_for(b"Execution Completed", offset)
            wait_for(b"context 2050/32768", offset)
            offset = len(captured)
            os.write(master, b"\x03")
            wait_for(b"Press Ctrl+C again to exit", offset)
            os.write(master, b"\x03")
        else:
            os.write(master, b"\x04")
        code = process.wait(timeout=10)
        while select.select([master], [], [], 0)[0]:
            captured.extend(os.read(master, 65536))
        assert code == 0, bytes(captured[-3000:])
        assert termios.tcgetattr(slave) == original, "Terminal modes not restored"
        assert b"\x1b[?1049l" in captured, "Alternate screen not restored"
        assert b"\x1b[?2004l" in captured, "Bracketed paste not restored"
        has_colors = re.search(rb"\x1b\[(?:38|48);", captured) is not None
        assert has_colors == (color == "always"), "Color policy was not respected"
        if resume:
            assert b"\x1b[?1006h" not in captured
        else:
            assert b"\x1b[?1006l" in captured, "Mouse mode not restored"
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
        project, state = root / "project", root / "state"
        project.mkdir()
        (project / "fixture.txt").write_text("A fictional project fixture.\nSecond line.\nExpanded fixture line.\nFinal line.\n")
        base_url = f"http://127.0.0.1:{server.server_port}/v1"
        scenario(project, state, base_url)
        scenario(project, state, base_url, resume=True)
        scenario(project, state, base_url, resume=True, color="always")
        scenario(project, state, base_url, resume=True, color="auto")
        assert len(requests) == 4, "Resume reexecuted a model turn"
        assert sum(m.get("content") == "Partial answer kept." for m in requests[-1]["messages"]) == 1, "Cancelled partial response was lost or duplicated"
        assert any(m.get("role") == "tool" and "fictional project fixture" in m.get("content", "")
                   for m in requests[-1]["messages"]), "The model did not receive the actual file"
        assert sorted(p.name for p in project.iterdir()) == ["fixture.txt"], "Workspace was modified"
finally:
    continue_response.set()
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)

print("PTY checks passed: tools, Markdown, composer, history navigation, fresh launch, rename, new/resume/filter/cancel, empty-draft filtering, working context report, Esc interruption and continuation, double Ctrl+C, preserved draft, colors, restart and terminal restoration.")
