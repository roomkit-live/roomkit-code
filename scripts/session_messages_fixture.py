"""Exercise the same SQLite workspace from two independent terminals with a scripted provider."""
from contextlib import ExitStack
import json
from pathlib import Path
import re
import sqlite3
import tempfile
import threading

from executor_fixture import StreamingModel, tool
from fixture_http import LoopbackHTTPServer


class Recipient(StreamingModel):
    def reply(self, request):
        messages = request["messages"]
        if not self.server.silent and any("CHECK_SILENT" in str(m.get("content")) for m in messages):
            self.server.silent = True
            return {"content": "PRIVATE_LOCAL_ANSWER"}
        last = messages[-1]
        if last["role"] == "tool" and last["tool_call_id"] == "reply":
            assert '"sent"' in str(last["content"])
            return {"content": "Recipient finished."}
        if not self.server.waiting.is_set() and any(
                m["role"] == "user" and "RUN_UNTIL_MESSAGE" in str(m.get("content")) for m in messages):
            self.server.waiting.set()
            assert self.server.release.wait(18)
            return tool("list", "first-read", path=".")
        for m in reversed(messages):
            content = str(m.get("content", ""))
            if "Incoming session correspondence." not in content:
                continue
            match = re.search(r'session-msg-[0-9a-f]{64}', content)
            if match and "CHECK_BUSY" in content:
                return tool("send_message", "reply", reply_to=match.group(), text="BUSY_RESPONSE")
            if match and "CHECK_IDLE" in content and not self.server.waiting.is_set():
                return tool("send_message", "reply", reply_to=match.group(), text="IDLE_RESPONSE")
        if any("RUN_UNTIL_MESSAGE" in str(m.get("content", "")) for m in messages):
            self.server.waiting.set()
            assert self.server.release.wait(18)
            return tool("list", "continue", path=".")
        raise AssertionError("Unexpected model request")


class Sender(StreamingModel):
    def reply(self, request):
        self.server.called.set()
        return {"content": "Unexpected sender turn"}


def provider(stack, handler):
    server = stack.enter_context(LoopbackHTTPServer(("127.0.0.1", 0), handler))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    stack.callback(server.shutdown)
    return server


def exercise(App):
    with tempfile.TemporaryDirectory(prefix="roomkit-session-mail-") as directory, ExitStack() as stack:
        root = Path(directory)
        project = root / "project"
        project.mkdir()
        state = root / "state"
        server = provider(stack, Recipient)
        server.waiting, server.release = threading.Event(), threading.Event()
        server.silent = False
        sender_model = provider(stack, Sender)
        sender_model.called = threading.Event()

        def open_session(room, endpoint):
            app = App(project, state, room, url=f"http://127.0.0.1:{endpoint.server_port}/v1")
            stack.callback(app.cleanup)
            app.wait("Tab commands")
            return app

        sender = open_session("api", sender_model)
        # The first launch imports its provider once. The second process loads
        # the saved project connection; give it a distinct fixture endpoint.
        connection = state / "connections.json"
        saved = json.loads(connection.read_text())
        profile, = saved["profiles"].values()
        profile["base_url"] = f"http://127.0.0.1:{server.server_port}/v1"
        connection.write_text(json.dumps(saved))
        receiver = open_session("review", server)
        stack.callback(server.release.set)
        sender.wait("Message an open session", sender.send("@rev\r"))
        sender.wait("Message to review", sender.send("\r"))
        sender.wait("Tab commands", sender.send("\x1b"))
        sender.wait("Message to @review", sender.send("@review CHECK_IDLE\r"))
        sender.wait("IDLE_RESPONSE")
        receiver.wait("Recipient finished.")
        sender.send("@review RUN_UNTIL_MESSAGE\r")
        assert server.waiting.wait(12), "recipient is not running"
        sender.wait("Message to @review", sender.send("@review CHECK_BUSY\r"))
        receiver.wait("CHECK_BUSY")
        server.release.set()
        sender.wait("BUSY_RESPONSE")
        receiver.wait("Recipient finished.")
        offset = sender.send("@review CHECK_SILENT\r")
        receiver.wait("PRIVATE_LOCAL_ANSWER")
        sender.wait("Message: CHECK_SILENT", offset)
        receiver.finish()
        sender.wait("Recipient unavailable", sender.send("@review OFFLINE_MARKER\r"))
        dbpath = next(state.glob("projects/*/history.sqlite3"))
        with sqlite3.connect(dbpath) as db:
            rows = [(room, json.loads(data)) for room, data in db.execute("select room_id,data from events")]
        assert not any(room == "api" and ev["source"]["channel_type"] == "ai" and
                       ev["content"]["type"] in ("text", "tool_call") and ev["type"] != "system"
                       for room, ev in rows), "Sender's model unexpectedly ran"
        replies = [ev for room, ev in rows if room == "api" and ev["id"].startswith("session-reply-")]
        assert len(replies) == 2, replies
        notices = [ev for room, ev in rows if room == "api" and ev["id"].startswith("session-notice-")]
        assert any("without sending a reply" in str(ev) and "CHECK_SILENT" in str(ev) for ev in notices)
        assert not any("CHECK_IDLE" in str(ev) or "CHECK_BUSY" in str(ev) for ev in notices), notices
        assert not any("PRIVATE_LOCAL_ANSWER" in str(ev) for room, ev in rows if room == "api")
        assert all("OFFLINE_MARKER" not in str(ev) for room, ev in rows if room == "review")
        posted = [ev for room, ev in rows if room == "review" and
                  str(ev.get("idempotency_key") or "").startswith("dispatch-")]
        assert len(posted) == 3 and not any("CHECK_BUSY" in str(ev["content"]) for ev in posted), posted
        assert not sender_model.called.is_set(), "The sender contacted its model"
        sender.finish()
    print("Session messages passed: recipient picker, idle post, follow-up during its turn, two replies, zero sender model calls, offline refusal.")
