"""An agent starts a conversation, does local work and consumes replies in its own turn."""
from contextlib import contextmanager
import json
import os
import select
import sqlite3
import threading

from executor_fixture import StreamingModel, tool
from session_messages_fixture import provider


class Initiator(StreamingModel):
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


def exercise_agents(stack, root, launch, reviewer, token):
    model = provider(stack, Initiator)
    model.token, model.answers = token, []
    model.local_work, model.release = threading.Event(), threading.Event()
    reviewer.agent_release = threading.Event()
    author = launch(model.server_port)
    stack.callback(model.release.set)
    stack.callback(reviewer.agent_release.set)
    (root / "author/workspace/local.txt").write_text("LOCAL_WORK_PROOF")
    for phase in ["IDLE", "BUSY", "NOTICE"]:
        model.phase = phase
        reviewer.agent_release.clear()
        model.local_work.clear()
        start = len(author.output)
        os.write(author.master, ("BEGIN_" + phase + "\r").encode())
        author.wait("local.txt", start)
        assert model.local_work.wait(15), "The asking agent could not work before the reply"
        if phase != "BUSY":
            author.wait("Independent work finished.", start)
        reviewer.agent_release.set()
        if phase == "BUSY":
            author.wait("Reply from @review", start)
            model.release.set()
        author.wait("RESUMED_" + phase, start)
    assert model.answers == ["IDLE", "BUSY", "NOTICE"], model.answers
    with sqlite3.connect(next((root / "author/state").glob("projects/*/history.sqlite3"))) as db:
        posted = [json.loads(row[0]) for row in db.execute(
            "SELECT data FROM events WHERE idempotency_key LIKE 'dispatch-session-%'")]
        assert len(posted) == 2 and all("AGENT_BUSY" not in str(ev["content"]) for ev in posted), posted
        assert db.execute("SELECT 1 FROM events WHERE data LIKE '%PRIVATE_REVIEW_FINISHED%'").fetchone() is None
    model.phase = "OUTSIDE"
    author.send("/new", "Start a conversation")
    author.send("BEGIN_OUTSIDE", "UNPUBLISHED_ROOM_REFUSED")
    os.write(author.master, b"\x04")
    author.proc.wait(timeout=15)
    print("PASS agent discovery and async send, local work before reply, idle resumption, active-turn steering, delivery notice and refusal from an unpublished room.")
