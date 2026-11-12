"""Exercise --prompt as an integrator runs it: no terminal, pipes, exit status.

With an argument, only the scenarios whose name holds it run.
"""
from http.server import BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import pty
import select
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

from fixture_http import LoopbackHTTPServer

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
                    if thought := args.get("_thought"):
                        deltas.append({"reasoning_content": thought})
                        args = {key: value for key, value in args.items() if key != "_thought"}
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

        self.server = LoopbackHTTPServer(("127.0.0.1", 0), Handler)
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


def environment(root):
    """The launch's environment: its default storage stays under `root`."""
    storage = root / "storage"
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    env.update({"HOME": str(storage / "home"), "XDG_DATA_HOME": str(storage / "data")})
    return env


def launch(root, model, *options, stdin=None, name="fixture-model", stderr=subprocess.PIPE, named_storage=True):
    """Start the binary on pipes, the way a benchmark harness does."""
    storage = ["--state-dir", str(root / "storage" / "state")] if named_storage else []
    argv = [str(binary), str(root / "project"), *storage, "--base-url", model.url, "--model", name, "--no-update-check", *options]
    return subprocess.Popen(argv, stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=stderr, text=True, env=environment(root))


def run(root, model, *options, stdin=None, name="fixture-model", named_storage=True):
    process = launch(root, model, *options, stdin=stdin, name=name, named_storage=named_storage)
    out, err = process.communicate(stdin, timeout=60)
    return process.returncode, out, err


def scenario(name, replies, check):
    if len(sys.argv) > 1 and sys.argv[1] not in name:
        return
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
    # Nobody watches pipes: no header, no line per call. A storage named at
    # launch is named again to reopen it.
    assert "model: " not in err and "Write note.txt" not in err, err
    assert "--state-dir" in err, err
    # Nobody could approve leaving Plan: the mode tools are not offered. Nor
    # are the plans and the guide, to keep each request small; the web stays.
    tools = model.tools()
    assert {"shell", "process_start", "output_read", "read_stored_result", "web_search", "web_fetch"} <= tools, tools
    assert not tools & {"plan_mode_enter", "plan_mode_exit"}, tools
    # Nobody can answer a question either.
    assert "ask_user" not in tools, tools
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
    # The model reads the text alone, the views keep the structured result.
    written = next(item["call"] for item in tools if item["call"]["name"] == "write" and item["call"]["state"] == "succeeded")
    assert written["data"]["data"]["path"] == "note.txt", written
    read = model.tool_results()[0]["content"]
    assert read.startswith('write "note.txt": created=true'), read
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


def reasoning_sent_back(model):
    """The reasoning the second request gives back on the first round."""
    return [m["reasoning_content"] for m in model.requests[1]["messages"] if m.get("role") == "assistant" and "reasoning_content" in m]


def reasoning_goes_back_to_glm(root, model):
    code, out, err = run(root, model, "--mode", "yolo", "--prompt", "Look.", name="glm-5.3-flash")
    assert code == 0, (code, err)
    assert reasoning_sent_back(model) == ["I look first."], model.requests[1]["messages"]


def reasoning_stays_home_elsewhere(root, model):
    code, out, err = run(root, model, "--mode", "yolo", "--prompt", "Look.")
    assert code == 0, (code, err)
    assert reasoning_sent_back(model) == [], model.requests[1]["messages"]


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
    assert len(results) == 1 and results[0].startswith("exit_code=0"), results
    assert (root / "project" / "long").read_text() == "ran"


class Screen:
    """A terminal standing in for a person's, on standard error alone."""

    def __init__(self):
        self.master, self.slave = pty.openpty()
        self.seen = ""

    def read(self, timeout):
        """What came next; False once the launch closed the terminal."""
        ready, _, _ = select.select([self.master], [], [], timeout)
        if not ready:
            return True
        try:
            data = os.read(self.master, 65536)
        except OSError:
            return False
        self.seen += data.decode(errors="replace").replace("\r\n", "\n")
        return bool(data)

    def wait(self, text):
        deadline = time.monotonic() + 30
        while text not in self.seen:
            assert time.monotonic() < deadline, (text, self.seen)
            assert self.read(0.1), (text, self.seen)

    def rest(self):
        """Read buffered output after exit, before closing the last slave."""
        try:
            # Darwin flushes unread output when the last slave closes. Keep
            # ours open until collection, and do not wait for an inherited FD.
            deadline = time.monotonic() + 10
            while select.select([self.master], [], [], 0)[0]:
                assert time.monotonic() < deadline, self.seen
                if not self.read(0):
                    break
            return self.seen
        finally:
            os.close(self.master)
            os.close(self.slave)


def watched(root, model):
    screen = Screen()
    process = launch(root, model, "--mode", "yolo", "--prompt", "Wait for a while.", stderr=screen.slave)
    # The call shows while it runs, not when the launch ends.
    screen.wait("Run sleep 3")
    assert process.poll() is None, screen.seen
    out = process.stdout.read()
    process.wait(timeout=60)
    lines = screen.rest().split("\n")
    assert process.returncode == 0, lines
    assert out == ANSWER + "\n", out
    assert lines[0] == f"roomkit-code · {(root / 'project').resolve()}", lines
    assert lines[1].startswith("model: fixture-model · provider: ") and " · mode: yolo · session: " in lines[1], lines
    assert lines[2:4] == ["", "  Run sleep 3"], lines
    assert not any(ANSWER in line for line in lines), lines
    assert any(line.startswith("Session: ") for line in lines), lines
    assert any(line.startswith("  Run sleep 3 · 3s") for line in lines), lines


def watched_refusal(root, model):
    # Whoever watches an Ask launch learns before the first refusal.
    screen = Screen()
    process = launch(root, model, "--prompt", "Create the file.", stderr=screen.slave)
    out = process.stdout.read()
    process.wait(timeout=60)
    lines = screen.rest().split("\n")
    assert out == ANSWER + "\n", (out, lines)
    assert " · mode: ask (approvals refused) · session: " in lines[1], lines
    assert "  Run touch denied.txt · refused" in lines, lines


def watched_json(root, model):
    # Events are for a program, whatever standard error is: nothing is added.
    screen = Screen()
    process = launch(root, model, "--mode", "yolo", "--json", "--prompt", "Write the note.", stderr=screen.slave)
    process.stdout.read()
    process.wait(timeout=60)
    seen = screen.rest()
    assert process.returncode == 0, seen
    assert "model: " not in seen and "Write note.txt" not in seen, seen


def watched_messages(root, model):
    # What the agent says between its calls shows to whoever watches; its
    # answer stays on standard output alone. Pipes get neither.
    code, out, err = run(root, model, "--mode", "yolo", "--prompt", "Look around.")
    assert (code, out) == (0, ANSWER + "\n"), (code, out, err)
    assert "Looking around first." not in err, err
    screen = Screen()
    process = launch(root, model, "--mode", "yolo", "--prompt", "Look around.", stderr=screen.slave)
    out = process.stdout.read()
    process.wait(timeout=60)
    lines = screen.rest().split("\n")
    assert out == ANSWER + "\n", (out, lines)
    said = lines.index("Looking around first.")
    assert lines[said + 1] == "  List .", lines
    assert not any(ANSWER in line for line in lines), lines


def unreachable_once(root, model):
    # A server nobody answers on is explained once, not once per layer.
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    argv = [str(binary), str(root / "project"), "--state-dir", str(root / "storage" / "state"), "--base-url", f"http://127.0.0.1:{port}/v1", "--model", "fixture-model", "--no-update-check", "--prompt", "Hello."]
    done = subprocess.run(argv, capture_output=True, text=True, stdin=subprocess.DEVNULL, env=environment(root), timeout=120)
    assert done.returncode == 1, (done.returncode, done.stderr)
    assert done.stderr.count("Cannot reach the model server") == 1, done.stderr


def misspelled_intent(root, model):
    # A model that drops the intent's leading underscore still runs its call,
    # and the person still reads what it does.
    code, out, err = run(root, model, "--mode", "yolo", "--json", "--prompt", "Write it.")
    assert code == 0, (code, err)
    assert (root / "project" / "done").read_text() == "ok"
    tools = [json.loads(line)["item"] for line in out.splitlines() if json.loads(line)["kind"] == "item"]
    shell = [item["call"] for item in tools if item.get("kind") == "tool" and item["call"]["name"] == "shell"]
    assert shell and shell[-1]["state"] == "succeeded", shell
    assert shell[-1].get("intent") == "Je l'écris.", shell[-1]
    assert "roomkit_intent" not in shell[-1]["arguments"], shell[-1]


def blank_edges(root, model):
    # A model may open and close on blank lines: a script captures the answer.
    code, out, err = run(root, model, "--prompt", "Say it.")
    assert code == 0, (code, err)
    assert out == ANSWER + "\n", repr(out)


def default_storage(root, model):
    # The default storage needs no naming, to read or to paste.
    code, out, err = run(root, model, "--prompt", "Task one.", named_storage=False)
    assert code == 0, (code, err)
    lines = err.splitlines()
    command = shlex.split(lines[lines.index("To continue this session, run:") + 1])
    assert "--state-dir" not in command, command
    # The command as printed reopens the conversation.
    again = subprocess.run([str(binary), *command[1:], "--prompt", "Task two."], capture_output=True, text=True, stdin=subprocess.DEVNULL, env=environment(root), timeout=60)
    assert again.returncode == 0, again.stderr
    assert any("Task one." in json.dumps(m) for m in model.requests[1]["messages"]), model.requests[1]["messages"]


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
scenario("a GLM model gets its reasoning back", [call("list", path=".", _thought="I look first."), ANSWER], reasoning_goes_back_to_glm)
scenario("another model does not", [call("list", path=".", _thought="I look first."), ANSWER], reasoning_stays_home_elsewhere)
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
scenario("watched from a terminal", [call("shell", command="sleep 3"), ANSWER], watched)
scenario("watched refusal", [call("shell", command="touch denied.txt"), ANSWER], watched_refusal)
scenario("watched events", [call("write", path="note.txt", content="hello\n"), ANSWER], watched_json)
scenario("watched messages", [call("list", "Looking around first.", path="."), ANSWER, call("list", "Looking around first.", path="."), ANSWER], watched_messages)
scenario("unreachable server", [ANSWER], unreachable_once)
scenario("misspelled intent", [("shell", {"command": "printf ok > done", "roomkit_intent": "Je l'écris."}), ANSWER], misspelled_intent)
scenario("blank lines around the answer", ["\n\n" + ANSWER + "\n\n"], blank_edges)
scenario("default storage", ["Answer one.", "Answer two."], default_storage)
print("headless checks passed")
