"""Exercise --prompt as an integrator runs it: no terminal, pipes, exit status."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
ANSWER = "Headless fixture done."


def call(name, text=None, **args):
    """A tool call, after `text` when the generation writes some first."""
    reply = (name, {**args, "_roomkit_intent": f"I call {name}"})
    return reply + (text,) if text else reply


class Model:
    """One model reply per request, in order; every request is kept."""

    def __init__(self, replies):
        self.replies, self.requests = replies, []
        model = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                model.requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                index = len(model.requests) - 1
                reply = model.replies[min(index, len(model.replies) - 1)]
                deltas = []
                if isinstance(reply, tuple):
                    name, args, *text = reply
                    deltas += [{"content": line} for line in text]
                    deltas.append({"tool_calls": [{"index": 0, "id": f"call-{index}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]})
                    reason = "tool_calls"
                else:
                    deltas, reason = [{"content": reply}], "stop"
                chunks = [{"choices": [{"index": 0, "delta": delta}]} for delta in deltas]
                chunks.append({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
                body = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks) + "data: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_port}/v1"

    def close(self):
        self.server.shutdown()

    def tools(self, index=0):
        return {tool["function"]["name"] for tool in self.requests[index].get("tools", [])}

    def said(self, text):
        """Whether the first request carries `text` in a user message."""
        return any(m.get("role") == "user" and text in json.dumps(m.get("content")) for m in self.requests[0]["messages"])

    def tool_results(self):
        return [message for request in self.requests for message in request["messages"] if message.get("role") == "tool"]


def launch(root, model, *options, stdin=None):
    """Start the binary on pipes, the way a benchmark harness does."""
    storage = root / "storage"
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    env.update({"HOME": str(storage / "home"), "XDG_DATA_HOME": str(storage / "data")})
    argv = [str(binary), str(root / "project"), "--state-dir", str(storage / "state"), "--base-url", model.url, "--model", "fixture-model", "--no-update-check", *options]
    return subprocess.Popen(argv, stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)


def run(root, model, *options, stdin=None):
    process = launch(root, model, *options, stdin=stdin)
    out, err = process.communicate(stdin, timeout=60)
    return process.returncode, out, err


def scenario(name, replies, check):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "project").mkdir()
        model = Model(replies)
        try:
            check(root, model)
        finally:
            model.close()
    print(f"ok: {name}")


def yolo_text(root, model):
    code, out, err = run(root, model, "--mode", "yolo", "--prompt", "Write the note.")
    assert code == 0, (code, err)
    # Standard output holds the answer alone: a script can capture it.
    assert out == ANSWER + "\n", out
    assert (root / "project" / "note.txt").read_text() == "hello\n"
    assert "Session: " in err and "--session" in err, err
    # Nobody could approve leaving Plan: the mode tools are not offered.
    tools = model.tools()
    assert "plan_create" in tools and "shell" in tools, tools
    assert not tools & {"plan_mode_enter", "plan_mode_exit"}, tools
    assert model.said("Write the note."), model.requests[0]["messages"]


def yolo_json(root, model):
    code, out, err = run(root, model, "--mode", "yolo", "--prompt", "Write the note.", "--json")
    assert code == 0, (code, err)
    events = [json.loads(line) for line in out.splitlines()]
    kinds = [event["kind"] for event in events]
    assert kinds[0] == "snapshot", kinds
    for kind in ("run_started", "item", "message_finalized", "run_finished", "operation_finished"):
        assert kind in kinds, (kind, kinds)
    assert [e["outcome"] for e in events if e["kind"] == "run_finished"] == ["completed"], events
    tools = [e["item"] for e in events if e["kind"] == "item" and e["item"].get("kind") == "tool"]
    assert any(item["call"]["name"] == "write" for item in tools), tools
    answers = [e["message"]["text"] for e in events if e["kind"] == "message_finalized"]
    assert answers[-1] == ANSWER, answers
    # The answer is in the stream, never printed beside it.
    assert ANSWER + "\n" not in out.split("\n", 1)[0], out


def ask_refuses(root, model):
    code, out, err = run(root, model, "--prompt", "Create the file.")
    # The agent reads the refusal and answers: the launch completed.
    assert code == 0, (code, err)
    assert out == ANSWER + "\n", out
    assert "Refused, nobody can approve it" in err, err
    assert not (root / "project" / "denied.txt").exists()
    results = model.tool_results()
    assert len(results) == 1 and "denied.txt" not in os.listdir(root / "project"), results


def round_limit(root, model):
    code, out, err = run(root, model, "--mode", "yolo", "--max-tool-rounds", "1", "--prompt", "Look around.")
    assert code == 1, (code, out, err)
    assert "Tool round limit reached" in err, err
    assert out == "Still looking.\n", out
    # The second round's call never ran: one result reached the model.
    assert len(model.tool_results()) == 1, model.tool_results()
    assert len(model.requests) == 2, len(model.requests)


def from_stdin(root, model):
    code, out, err = run(root, model, "--prompt", "-", stdin="A message read from standard input.\n")
    assert code == 0, (code, err)
    assert out == ANSWER + "\n", out
    assert model.said("A message read from standard input."), model.requests[0]["messages"]


def interrupted(root, model):
    process = launch(root, model, "--mode", "yolo", "--json", "--prompt", "Wait for a while.")
    started = time.monotonic()
    # Wait until the shell call runs, then signal as a harness timeout would.
    for line in process.stdout:
        event = json.loads(line)
        if event["kind"] == "item" and event["item"].get("kind") == "tool" and event["item"]["call"]["state"] == "running":
            break
    process.send_signal(signal.SIGINT)
    rest, err = process.communicate(timeout=30)
    assert process.returncode == 130, (process.returncode, err)
    assert time.monotonic() - started < 20, "the sleep was not cut"
    outcomes = [json.loads(line)["outcome"] for line in rest.splitlines() if json.loads(line)["kind"] == "run_finished"]
    assert outcomes == ["interrupted"], outcomes


def refused_without_agent(root, model):
    # An empty message is refused before anything opens.
    code, out, err = run(root, model, "--prompt", "   ")
    assert code == 1 and "empty" in err, (code, err)
    assert not model.requests
    # --json needs --prompt.
    code, out, err = run(root, model, "--json")
    assert code == 2 and "--prompt" in err, (code, err)


scenario("yolo answer on standard output", [call("write", path="note.txt", content="hello\n"), ANSWER], yolo_text)
scenario("yolo events as JSON lines", [call("write", path="note.txt", content="hello\n"), ANSWER], yolo_json)
scenario("ask refuses approvals", [call("shell", command="touch denied.txt"), ANSWER], ask_refuses)
# The cut round writes text: its end rides that message. A cut round with
# calls only loses its end before the host reads it, in the terminal too
# (a defect of the kit's delivery, reported apart).
scenario("round limit fails the launch", [call("list", path="."), call("list", "Still looking.", path="."), ANSWER], round_limit)
scenario("message from standard input", [ANSWER], from_stdin)
scenario("a signal interrupts the turn", [call("shell", command="sleep 30", timeout_ms=60000), ANSWER], interrupted)
scenario("invalid launches", [ANSWER], refused_without_agent)
print("headless checks passed")
