"""An agent starts a conversation, does local work and consumes replies in its own turn."""
from contextlib import contextmanager
import json
import os
import select
import sqlite3
import threading
import time

from executor_fixture import StreamingModel, Terminal, tool
from session_messages_fixture import provider
from pty_checks import screen_text, wait_for_exit


class Initiator(StreamingModel):
    def after_delta(self, delta: dict) -> None:
        if delta.get("content") == "WAITING_FOR_REVIEW":
            assert self.server.finish_text.wait(25), "the terminal did not display the open stream"

    def reply(self, request):
        last = request["messages"][-1]
        content = str(last.get("content") or "")
        phase = self.server.phase
        if phase == "OUTSIDE":
            if last.get("tool_call_id") == "outside":
                assert "has not joined" in content
                return {"content": "UNPUBLISHED_ROOM_REFUSED"}
            return tool("send_message", "outside", to="review", text="UNPUBLISHED_MESSAGE")
        if last.get("tool_call_id") == "invalid":
            assert "exactly one" in content
            return tool("send_message", "ask-" + phase, to=self.server.peer, reply_to="", text="AGENT_" + phase)
        if last.get("tool_call_id") == "discover":
            peers = json.loads(content)["sessions"]
            assert {p["name"] for p in peers} == {"review"}
            to = next(p["to"] for p in peers if p["name"] == "review")
            if phase == "IDLE":
                self.server.peer = to
                return tool("send_message", "invalid", to=to, reply_to="invalid", text="INVALID_MESSAGE")
            return tool("send_message", "ask-" + phase, to=to, text="AGENT_" + phase)
        if last.get("tool_call_id") == "ask-" + phase:
            receipt = json.loads(content)
            assert receipt["status"] == "sent", receipt
            self.server.request_id = receipt["message_id"]
            return tool("read", "local-work", path="local.txt")
        if last.get("tool_call_id") == "local-work":
            assert "LOCAL_WORK_PROOF" in content
            self.server.local_work.set()
            if phase == "BUSY":
                assert self.server.release.wait(20)
                return {"content": "WAITING_FOR_REVIEW"}
            return {"content": "Independent work finished."}
        result = next((str(m["content"]) for m in reversed(request["messages"])
                       if "Session correspondence result." in str(m.get("content"))
                       and "AGENT_" + phase in str(m.get("content"))), None)
        if result:
            content = result
            assert self.server.request_id in content
            assert "AGENT_" + phase in content
            if phase == "NOTICE":
                assert "delivery_notice" in content and "without sending a reply" in content, content
            else:
                assert self.server.token in content
            self.server.answers.append(phase)
            return {"content": "RESUMED_" + phase}
        if any(m["role"] == "user" and "BEGIN_" + phase in str(m.get("content")) for m in request["messages"]):
            assert {"list_sessions", "send_message"}.issubset(t["function"]["name"] for t in request["tools"])
            return tool("list_sessions", "discover")
        raise AssertionError(f"Unexpected agent step in {phase}: {last!r}")


@contextmanager
def drain_terminal(terminal):
    """Keep a non-observed PTY flowing while the other terminal drives the exchange."""
    stop = threading.Event()

    def drain():
        while not stop.is_set():
            if select.select([terminal.master], [], [], .1)[0]:
                try:
                    if not os.read(terminal.master, 65536):
                        return
                except OSError:
                    if terminal.proc.poll() is not None:
                        return
                    raise

    thread = threading.Thread(target=drain, daemon=True)
    thread.start()
    try:
        yield
    finally:
        stop.set()
        thread.join()


@contextmanager
def fail_idle_post(root, *, checkpoint):
    """Hold only the addressed reply's insertion until the receiver reports the error."""
    private = next((root / "author/state").glob("projects/*/history.sqlite3")).parent
    diag = private / "diagnostics.jsonl"
    offset = len(diag.read_text())
    with sqlite3.connect(private / "history.sqlite3") as db:
        if checkpoint:
            db.execute("""CREATE TRIGGER reject_idle_post BEFORE INSERT ON private_state
                          WHEN NEW.key = 'roomkit_code:session_mail_dispatch:remote:v1'
                            AND json_extract(NEW.value, '$.id') LIKE 'session-reply-%'
                            AND json_type(NEW.value, '$.dispatch.Post') = 'object'
                          BEGIN SELECT RAISE(FAIL, 'inbox-checkpoint-fixture'); END""")
        else:
            db.execute("""CREATE TRIGGER reject_idle_post BEFORE INSERT ON events
                      WHEN NEW.idempotency_key LIKE 'dispatch-session-reply-%'
                      BEGIN SELECT RAISE(FAIL, 'inbox-retry-fixture'); END""")
    def wait_for_failure():
        deadline = time.monotonic() + 15
        while "session_inbox_unavailable" not in diag.read_text()[offset:]:
            if time.monotonic() >= deadline:
                raise AssertionError("The idle reply did not reach the injected failure: " + (private / "diagnostics.jsonl").read_text()[-2400:])
            time.sleep(.05)
    try:
        yield wait_for_failure
    finally:
        with sqlite3.connect(private / "history.sqlite3") as db:
            db.execute("DROP TRIGGER reject_idle_post")


WRITING = "WAITING_FOR_REVIEW"
REPLY = "PEER_ORDER_PROOF"
SUMMARY = "RESUMED_BUSY"


def check_reading_order(terminal: Terminal, markers: tuple[str, ...]) -> None:
    # Read what is on screen. The PTY byte stream records updates in arrival order.
    deadline = time.monotonic() + 10
    while True:
        frame = screen_text(terminal.output, rows=58, cols=200)
        if all(frame.count(m) == 1 for m in markers):
            positions = [frame.index(m) for m in markers]
            assert positions == sorted(positions), frame
            return
        if time.monotonic() >= deadline:
            raise AssertionError("Missing or duplicate conversation text on screen:\n" + frame)
        if select.select([terminal.master], [], [], .1)[0]:
            terminal.output.extend(os.read(terminal.master, 65536))


def exercise_agents(stack, root, launch, reviewer, token):
    model = provider(stack, Initiator)
    model.token, model.answers = token, []
    model.local_work, model.release = threading.Event(), threading.Event()
    model.finish_text = threading.Event()
    stack.callback(model.finish_text.set)
    reviewer.agent_release = threading.Event()
    author = launch(model.server_port)
    stack.callback(model.release.set)
    stack.callback(reviewer.agent_release.set)
    (root / "author/workspace/local.txt").write_text("LOCAL_WORK_PROOF")
    for phase in ["IDLE", "BUSY", "NOTICE", "RESTART"]:
        model.phase = phase
        reviewer.agent_release.clear()
        model.local_work.clear()
        start = len(author.output)
        os.write(author.master, ("BEGIN_" + phase + "\r").encode())
        author.wait("local.txt", start)
        assert model.local_work.wait(15), "The asking agent could not work before the reply"
        if phase != "BUSY":
            author.wait("Independent work finished.", start)
        if phase in ("IDLE", "RESTART"):
            with fail_idle_post(root, checkpoint=phase == "IDLE") as failed:
                reviewer.agent_release.set()
                author.wait("Reply from @review", start)
                failed()
                if phase == "RESTART":
                    author.proc.kill()
                    author.proc.wait(timeout=10)
            assert phase not in model.answers
            if phase == "RESTART":
                author = launch(model.server_port)
                start = 0
        else:
            reviewer.agent_release.set()
        if phase == "BUSY":
            author.wait(REPLY, start)
            model.release.set()
            check_reading_order(author, (WRITING, REPLY))
            model.finish_text.set()
        author.wait("RESUMED_" + phase, start)
        if phase == "BUSY":
            check_reading_order(author, (WRITING, REPLY, SUMMARY))
            os.write(author.master, b"\x04")
            assert wait_for_exit(author.proc, author.master, author.output) == 0
            author = launch(model.server_port)
            author.wait("RESUMED_BUSY", 0)
            check_reading_order(author, (WRITING, REPLY, SUMMARY))
            # Reopen the same fixture in the format used before the engine stored anchors.
            os.write(author.master, b"\x04")
            assert wait_for_exit(author.proc, author.master, author.output) == 0
            with sqlite3.connect(next((root / "author/state").glob("projects/*/history.sqlite3"))) as db:
                updated = db.execute("""UPDATE events SET data = json_remove(data, '$.metadata.follows_index')
                    WHERE json_type(data, '$.metadata.follows_index') IS NOT NULL""").rowcount
                assert updated > 0, "the fixture must originally contain anchored messages"
            author = launch(model.server_port)
            check_reading_order(author, (REPLY, WRITING, SUMMARY))
            print("PASS Writing, intervening reply and synthesis appear once in order during streaming and after reopen. Older history retains storage order.", flush=True)
    assert model.answers == ["IDLE", "BUSY", "NOTICE", "RESTART"], model.answers
    with sqlite3.connect(next((root / "author/state").glob("projects/*/history.sqlite3"))) as db:
        posted = [json.loads(row[0]) for row in db.execute(
            "SELECT data FROM events WHERE idempotency_key LIKE 'dispatch-session-%'")]
        assert len(posted) == 3 and all("AGENT_BUSY" not in str(ev["content"]) for ev in posted), posted
        assert db.execute("SELECT 1 FROM events WHERE data LIKE '%PRIVATE_REVIEW_FINISHED%'").fetchone() is None
    model.phase = "OUTSIDE"
    author.send("/new", "Start a conversation")
    author.send("BEGIN_OUTSIDE", "UNPUBLISHED_ROOM_REFUSED")
    os.write(author.master, b"\x04")
    author.proc.wait(timeout=15)
    print("PASS agent discovery and async send, local work before reply, idle resumption, active-turn steering, delivery notice and refusal from an unpublished room.")
