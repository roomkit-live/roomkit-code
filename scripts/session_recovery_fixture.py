"""Exercise storage failures at the boundaries between two independent owners."""
from contextlib import contextmanager
import json
import os
import re
import socket
import sqlite3
import threading
import time
import uuid

from executor_fixture import StreamingModel, Terminal, tool
from session_messages_fixture import Sender, provider


class Responder(StreamingModel):
    def reply(self, request):
        last = request["messages"][-1]
        if last.get("tool_call_id") == "answer":
            if "Remote session refused the message" in str(last["content"]):
                self.server.transmit_failed.set()
                assert self.server.release_failed.wait(20)
                return {"content": "Reviewer finished."}
            result = json.loads(last["content"])
            assert result["status"] == "sent", result
            if result.get("local_receipt") == "pending":
                self.server.receipt_failed.set()
                assert self.server.finish.wait(20)
            return {"content": "Reviewer finished."}
        question = next(str(m["content"]) for m in reversed(request["messages"])
                        if "Incoming session correspondence." in str(m.get("content")))
        message_id = re.search(r"session-msg-[0-9a-f]{64}", question).group()
        return tool("send_message", "answer", reply_to=message_id, text="Answer to " + message_id)


def rows(database, statement, *values):
    with sqlite3.connect(database) as db:
        return db.execute(statement, values).fetchall()


def wait_for(query):
    until = time.monotonic() + 20
    while not (result := query()):
        if time.monotonic() >= until:
            raise AssertionError("Timed out waiting for the stored correspondence")
        time.sleep(.05)
    return result


@contextmanager
def reject(database, operation, condition):
    rows(database, f"""CREATE TRIGGER fail_mail BEFORE {operation} WHEN {condition}
                        BEGIN SELECT RAISE(FAIL, 'mail recovery fixture'); END""")
    try:
        yield
    finally:
        rows(database, "DROP TRIGGER fail_mail")


def request_id(database, text):
    return wait_for(lambda: rows(database, "SELECT id FROM events WHERE id LIKE 'session-msg-%' AND data LIKE ?",
                                 "%" + text + "%"))[0][0]


def exercise_recovery(stack, directory, binary, env, args_for):
    models = provider(stack, Sender), provider(stack, Responder)
    sender_model, responder = models
    sender_model.called = threading.Event()
    responder.transmit_failed, responder.release_failed = threading.Event(), threading.Event()
    stack.callback(responder.release_failed.set)
    responder.receipt_failed, responder.finish = threading.Event(), threading.Event()
    stack.callback(responder.finish.set)
    group = uuid.uuid4().hex

    def launch(name, model, scope=group, launch_env=env):
        path = directory / name
        (path / "workspace").mkdir(parents=True, exist_ok=True)
        terminal = Terminal(stack, args_for(binary, path, name, scope, model.server_port), launch_env)
        database = next((path / "state").glob("projects/*/history.sqlite3"))
        return terminal, database

    reviewer, remote = launch("review", responder)
    sender, local = launch("api", sender_model)
    with reject(local, "INSERT ON events", "NEW.id LIKE 'sent-session-msg-%'"):
        sender.send("@review HISTORY_FAILURE", "Could not save the outgoing message")
        assert not rows(remote, "SELECT 1 FROM events WHERE data LIKE '%HISTORY_FAILURE%'")

    with reject(local, "UPDATE ON events", "NEW.id LIKE 'sent-session-msg-%'"):
        sender.send("@review HISTORY_CONFIRM", "local receipt could not be saved")
        ident = request_id(remote, "HISTORY_CONFIRM")
        sender.wait("Answer to " + ident, 0)
        saved = rows(local, "SELECT data FROM events WHERE id=?", "sent-"+ident)
        assert "HISTORY_CONFIRM" in saved[0][0] and "Delivery unconfirmed" in saved[0][0]

    wait_for(lambda: rows(local, """SELECT 1 FROM events WHERE id=?
        AND json_extract(data,'$.metadata.session_delivery')='accepted'""", "sent-"+ident))

    with reject(remote, "INSERT ON private_state", """NEW.key LIKE 'roomkit_code:session_reply:%'
             AND json_extract(NEW.value, '$.sent') = 1"""):
        sender.send("@review RECEIPT_FAILURE", "Message to @review")
        ident = request_id(remote, "RECEIPT_FAILURE")
        sender.wait("Answer to " + ident, 0)
        assert responder.receipt_failed.wait(15)
    responder.finish.set()
    state = wait_for(lambda: rows(remote, """SELECT value FROM private_state WHERE key=?
        AND json_extract(value,'$.sent')=1""", "roomkit_code:session_reply:" + ident))
    assert json.loads(state[0][0])["response"]["text"] == "Answer to " + ident
    assert rows(local, "SELECT id FROM events WHERE id IN (?,?)", "session-reply-"+ident, "session-notice-"+ident) == [("session-reply-"+ident,)]
    timeline = rows(local, "SELECT id FROM events WHERE id IN (?,?) ORDER BY idx", "sent-"+ident, "session-reply-"+ident)
    assert timeline == [("sent-"+ident,), ("session-reply-"+ident,)]

    with reject(local, "INSERT ON events", "NEW.id LIKE 'session-reply-%'"):
        sender.send("@review RESTART_BEFORE_REPLY", "Message to @review")
        unanswered = request_id(remote, "RESTART_BEFORE_REPLY")
        assert responder.transmit_failed.wait(15)
        reviewer.proc.kill()
        reviewer.proc.wait(timeout=10)
    responder.release_failed.set()
    reviewer, _ = launch("review", responder)
    sender.wait("Answer to " + unanswered, 0)
    assert len(rows(remote, "SELECT 1 FROM events WHERE idempotency_key=?", "dispatch-"+unanswered)) == 1

    def interrupt(text, terminal):
        with reject(remote, "INSERT ON private_state", """NEW.key = 'roomkit_code:session_mail_dispatch:remote:v1'
                AND json_extract(NEW.value,'$.dispatch')='Steering'"""):
            addr = rows(remote, "SELECT json_extract(value,'$.channel') FROM private_state WHERE key='roomkit_code:session_address:v1'")[0][0]
            sender.send("@peer:" + addr + " " + text, "Message to @review")
            ident = request_id(remote, text)
            assert not rows(remote, "SELECT 1 FROM events WHERE idempotency_key=?", "dispatch-"+ident)
            terminal.proc.kill()
            terminal.proc.wait(timeout=10)
        return ident

    waiting = interrupt("RESTART_BEFORE_DISPATCH", reviewer)
    # This owner cannot join the group. Wait for its inbox to pass the row
    # locally before restoring the original broker, so the assertion covers dispatch.
    with socket.socket() as unavailable:
        unavailable.bind(("127.0.0.1", 0))
        offline_env = dict(env, ROOMKIT_EXECUTOR_REDIS_URL=f"redis://127.0.0.1:{unavailable.getsockname()[1]}")
        offline, _ = launch("review", responder, launch_env=offline_env)
        offline.wait("could not be reached", 0)
        wait_for(lambda: rows(remote, """SELECT 1 FROM private_state, events
            WHERE key='roomkit_code:session_mail_cursor:v1' AND events.id=?
              AND json_extract(value,'$.local_index') >= events.idx
              AND coalesce(json_extract(value,'$.index'), -1) < events.idx""", waiting))
        assert not rows(remote, "SELECT 1 FROM events WHERE id=? OR idempotency_key=?",
                        "uncertain-"+waiting, "dispatch-"+waiting)
        os.write(offline.master, b"\x04")
        offline.proc.wait(timeout=15)
    reviewer, _ = launch("review", responder)
    sender.wait("Answer to " + waiting, 0)
    assert len(rows(remote, "SELECT 1 FROM events WHERE idempotency_key=?", "dispatch-"+waiting)) == 1
    assert len(rows(local, "SELECT 1 FROM events WHERE id=?", "session-reply-"+waiting)) == 1
    expired = interrupt("SWITCH_GROUP_BEFORE_DISPATCH", reviewer)
    outsider, _ = launch("review", responder, uuid.uuid4().hex)
    outsider.wait("original network address is unavailable", 0)
    assert not rows(remote, "SELECT 1 FROM events WHERE idempotency_key=?", "dispatch-"+expired)
    assert not sender_model.called.is_set()
    for terminal in (sender, outsider):
        os.write(terminal.master, b"\x04")
        terminal.proc.wait(timeout=15)
    print("PASS Durable outgoing history, reply receipt recovery, accepted mail across an offline restart and group isolation.", flush=True)
