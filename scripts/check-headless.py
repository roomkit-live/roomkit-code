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
    # Nobody could approve leaving Plan: the mode tools are not offered. Nor
    # are the plans and the guide, to keep each request small; the web stays.
    tools = model.tools()
    assert {"shell", "process_start", "output_read", "read_stored_result", "web_search", "web_fetch"} <= tools, tools
    assert not tools & {"plan_mode_enter", "plan_mode_exit"}, tools
    assert not [name for name in tools if name.startswith(("plan_", "docs_"))], tools
    # Nor does the prompt carry what it says of them.
    prompt = model.requests[0]["messages"][0]["content"]
    for paragraph in ("plan_mode_enter asks", "manage durable plans", "own user guide"):
        assert paragraph not in prompt, paragraph
    assert "Web search uses Exa" in prompt
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


def past_fifty_rounds(root, model):
    # The terminal's bound of 50 rounds does not apply unattended.
    code, out, err = run(root, model, "--mode", "yolo", "--prompt", "Look around for long.")
    assert code == 0, (code, err)
    assert out == ANSWER + "\n", out
    assert len(model.tool_results()) >= 55, len(model.tool_results())


def from_stdin(root, model):
    code, out, err = run(root, model, "--reasoning-effort", "low", "--prompt", "-", stdin="A message read from standard input.\n")
    assert code == 0, (code, err)
    # A benchmark pins the effort from the command line, as a new project's agent starts.
    assert model.requests[0].get("reasoning_effort") == "low", model.requests[0].get("reasoning_effort")
    assert out == ANSWER + "\n", out
    assert model.said("A message read from standard input."), model.requests[0]["messages"]


def same_session(root, model):
    # Each launch's message is new: the second one on a session is sent too.
    code, out, err = run(root, model, "--session", "ci", "--prompt", "Task one.")
    assert (code, out) == (0, "Answer one.\n"), (code, out, err)
    code, out, err = run(root, model, "--session", "ci", "--prompt", "Task two.")
    assert (code, out) == (0, "Answer two.\n"), (code, out, err)
    assert len(model.requests) == 2, len(model.requests)
    assert any("Task two." in json.dumps(m) for m in model.requests[1]["messages"])


def closed_output(root, model):
    # A reader that stops early does not crash the launch, which still closes.
    process = launch(root, model, "--prompt", "Write a lot.")
    process.stdout.readline()
    process.stdout.close()
    err = process.stderr.read()
    process.wait(timeout=60)
    assert process.returncode == 0, (process.returncode, err)
    assert "panicked" not in err and "Session: " in err, err


def closed_events(root, model):
    # Events written to a reader that left fail the launch, which still says
    # what it leaves running.
    process = launch(root, model, "--mode", "yolo", "--json", "--prompt", "Start the server.")
    # Leave once the server started, while the long answer still streams.
    for line in process.stdout:
        if '"process_start"' in line and '"succeeded"' in line:
            break
    process.stdout.close()
    _, err = process.communicate(timeout=60)
    pid = pid_in(root / "project" / "server.pid")
    try:
        assert process.returncode == 1 and "Cannot write the events" in err, (process.returncode, err)
        assert "Left 1 background process running:" in err, err
    finally:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def too_long(root, model):
    code, out, err = run(root, model, "--prompt", "-", stdin="x" * 70_000)
    assert code == 1 and "65536 bytes" in err, (code, err)
    assert not model.requests


def interrupted(root, model, sig=signal.SIGINT):
    process = launch(root, model, "--mode", "yolo", "--json", "--prompt", "Wait for a while.")
    started = time.monotonic()
    # Wait until the shell call runs, then signal as a harness timeout would.
    for line in process.stdout:
        event = json.loads(line)
        if event["kind"] == "item" and event["item"].get("kind") == "tool" and event["item"]["call"]["state"] == "running":
            break
    process.send_signal(sig)
    rest, err = process.communicate(timeout=30)
    assert process.returncode == 130, (process.returncode, err)
    assert time.monotonic() - started < 20, "the sleep was not cut"
    outcomes = [json.loads(line)["outcome"] for line in rest.splitlines() if json.loads(line)["kind"] == "run_finished"]
    assert outcomes == ["interrupted"], outcomes


def stored_result(root, model):
    # The engine's own read_stored_result runs as a read: the engine answers it.
    code, out, err = run(root, model, "--mode", "yolo", "--prompt", "Read the stored result.")
    assert code == 0, (code, err)
    results = [message["content"] for message in model.tool_results()]
    assert len(results) == 1 and "Result 'evicted_unknown' not found" in results[0], results
    assert "permission policy" not in results[0], results


def long_shell(root, model):
    # Unattended, a shell command may ask for up to an hour, as the prompt says.
    code, out, err = run(root, model, "--mode", "yolo", "--prompt", "Run the long build.")
    assert code == 0, (code, err)
    assert "long command (maximum 3600000)." in model.requests[0]["messages"][0]["content"]
    results = [message["content"] for message in model.tool_results()]
    assert len(results) == 1 and '"exit_code":0' in results[0].replace(" ", ""), results
    assert (root / "project" / "long").read_text() == "ran"


def alive(pid):
    """Whether pid runs: it exists, and on Linux is no zombie."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    stat = Path(f"/proc/{pid}/stat")
    try:
        return stat.read_text().rsplit(") ", 1)[1][0] not in "ZX"
    except FileNotFoundError:
        # No /proc (macOS): an orphan's zombie is reaped by init at once.
        return True


def count_in(path, at_least):
    """The number a job keeps writing in path, once it reaches at_least."""
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        text = path.read_text().strip() if path.exists() else ""
        if text.isdigit() and int(text) >= at_least:
            return int(text)
        time.sleep(0.01)
    raise AssertionError(f"{path} stopped short of {at_least}")


def pid_in(path):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        text = path.read_text().strip() if path.exists() else ""
        if text:
            return int(text)
        time.sleep(0.01)
    raise AssertionError(f"no pid in {path}")


def left_running(root, model):
    # Unattended, what the agent started runs on past the launch: a job a
    # command put in the background, writing to the pipes it inherited, and a
    # process it started. Only the process is listed in the report.
    code, out, err = run(root, model, "--mode", "yolo", "--prompt", "Start the servers.")
    project = root / "project"
    pids = [pid_in(project / "job.pid"), pid_in(project / "server.pid")]
    try:
        assert code == 0, (code, err)
        # Fifty more lines, written to its pipes after roomkit-code exited.
        written = count_in(project / "count", 0)
        count_in(project / "count", written + 50)
        assert [alive(pid) for pid in pids] == [True, True], pids
        assert "Left 1 background process running:" in err, err
    finally:
        for pid in pids:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


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
# Each call differs: the engine stops a turn that repeats one exact call.
scenario("no round limit unattended", [call("glob", pattern=f"*{i}*") for i in range(55)] + [ANSWER], past_fifty_rounds)
scenario("message from standard input", [ANSWER], from_stdin)
scenario("a second launch on one session", ["Answer one.", "Answer two."], same_session)
scenario("standard output closed early", ["\n".join(["line"] * 20_000)], closed_output)
scenario("events to a reader that left", [call("process_start", command="echo $$ > server.pid; exec sleep 60"), "\n".join(["line"] * 20_000)], closed_events)
scenario("a message too long", [ANSWER], too_long)
scenario("SIGINT interrupts the turn", [call("shell", command="sleep 30", timeout_ms=60000), ANSWER], interrupted)
scenario("SIGTERM interrupts the turn", [call("shell", command="sleep 30", timeout_ms=60000), ANSWER], lambda root, model: interrupted(root, model, signal.SIGTERM))
scenario("the engine's stored results are read", [call("read_stored_result", result_id="evicted_unknown"), ANSWER], stored_result)
scenario("a shell command past two minutes", [call("shell", command="printf ran > long", timeout_ms=130000), ANSWER], long_shell)
WRITER = "(i=0; while :; do echo line; i=$((i+1)); echo $i > count; sleep 0.02; done) & echo $! > job.pid"
scenario("what the agent started runs on", [call("shell", command=WRITER), call("process_start", command="echo $$ > server.pid; exec sleep 60"), ANSWER], left_running)
scenario("invalid launches", [ANSWER], refused_without_agent)
print("headless checks passed")
