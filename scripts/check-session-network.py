"""Two interactive owners with independent stores using RoomKit's Redis channel controls."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import re
import shlex
import sqlite3
import subprocess
import tempfile
import threading
import uuid
from urllib.parse import urlsplit

from executor_fixture import StreamingModel, Terminal, redis, tool
from session_messages_fixture import Sender, provider
from session_agent_fixture import drain_terminal, exercise_agents


class Reviewer(StreamingModel):
    def reply(self, request):
        messages = request["messages"]
        last = messages[-1]
        if last.get("tool_call_id") == "answer" and getattr(self.server, "repeat_answer", False):
            self.server.repeat_answer = False
            return tool("send_message", "repeat-answer", reply_to=self.server.message_id, text=self.server.answer)
        if last.get("tool_call_id") in ("answer", "busy-answer", "repeat-answer"):
            assert "sent" in str(last["content"])
            return {"content": "PRIVATE_REVIEW_FINISHED"}
        if last.get("tool_call_id") == "read-proof":
            token = re.search(r"PROOF-[a-f0-9]{32}", str(last["content"])).group()
            self.server.answer = token
            return tool("send_message", "answer", reply_to=self.server.message_id, text=token)
        if not self.server.waiting.is_set() and any(
                m["role"] == "user" and "HOLD_FOR_MESSAGE" in str(m.get("content")) for m in messages):
            self.server.waiting.set()
            assert self.server.release.wait(25)
            return tool("list", "hold", path=".")
        incoming = next(str(m["content"]) for m in reversed(messages)
                        if "Incoming session correspondence." in str(m.get("content")))
        self.server.message_id = re.search(r"session-msg-[0-9a-f]{64}", incoming).group()
        if any(tag in incoming for tag in ("AGENT_IDLE", "AGENT_BUSY", "AGENT_NOTICE")):
            self.server.repeat_answer = "AGENT_IDLE" in incoming
            assert self.server.agent_release.wait(20)
            if "AGENT_NOTICE" in incoming:
                return {"content": "PRIVATE_REVIEW_FINISHED"}
        if "SILENT_PROBE" in incoming:
            return {"content": "PRIVATE_SILENT_ANSWER"}
        if "BUSY_PROBE" in incoming:
            return tool("send_message", "busy-answer", reply_to=self.server.message_id, text="BUSY_ANSWER")
        return tool("read", "read-proof", path="proof.txt")


def args_for(binary, work, name, pool, port):
    return [binary, str(work / "workspace"), "--state-dir", str(work / "state"),
            "--session", name, "--session-group", pool, "--model", "fixture",
            "--base-url", f"http://127.0.0.1:{port}/v1", "--mode", "plan",
            "--no-update-check", "--no-herdr", "--no-terminal-status", "--color", "never"]


class Peer:
    def __init__(self, stack, root, host):
        self.host = host
        if host:
            self.conn = ["ssh", "-o", "BatchMode=yes", "-S", str(root / "ssh")]
            subprocess.run([*self.conn, "-MNf", host], check=True)
            stack.callback(subprocess.run, [*self.conn, "-O", "exit", host], capture_output=True)
            self.work = Path(self.run(["mktemp", "-d", "/tmp/roomkit-mail-test.XXXXXXXX"]).strip())
            stack.callback(self.run, ["rm", "-rf", str(self.work)])
        else:
            self.work = root / "review"
        self.run(["mkdir", "-p", str(self.work / "workspace")])

    def command(self, args, tty=False):
        return [*self.conn, *(["-tt"] if tty else []), self.host, shlex.join(args)] if self.host else args

    def run(self, args, data=None):
        return subprocess.check_output(self.command(args), text=True, input=data)

    def port(self, local):
        if not self.host:
            return local
        forwarding = f"127.0.0.1:0:127.0.0.1:{local}"
        return int(subprocess.check_output([*self.conn, "-O", "forward", "-R", forwarding, self.host], text=True))


def check_histories(stack, owner, peer):
    db = stack.enter_context(sqlite3.connect(next((owner / "state").glob("projects/*/history.sqlite3"))))
    assert db.execute("SELECT COUNT(*) FROM rooms WHERE id='review'").fetchone() == (0,)
    assert db.execute("SELECT COUNT(*) FROM events WHERE id LIKE 'session-reply-%'").fetchone() == (2,)
    notices = db.execute("SELECT data FROM events WHERE id LIKE 'session-notice-%'").fetchall()
    assert len(notices) == 1 and "SILENT_PROBE" in notices[0][0]
    assert db.execute("SELECT COUNT(*) FROM events WHERE data LIKE '%PRIVATE_%'").fetchone() == (0,)
    probe = """import pathlib, sqlite3, sys
p = next(pathlib.Path(sys.argv[1]).glob('projects/*/history.sqlite3'))
with sqlite3.connect(p) as db:
    assert not db.execute("SELECT 1 FROM rooms WHERE id='api'").fetchall()
    assert db.execute("SELECT COUNT(*) FROM events WHERE idempotency_key LIKE 'dispatch-%'").fetchone() == (2,)
    assert not db.execute("SELECT 1 FROM events WHERE data LIKE '%AMBIGUOUS%' OR data LIKE '%WRONG_GROUP%'").fetchall()
"""
    peer.run(["python3", "-c", probe, str(peer.work / "state")])


def run(options):
    metadata = subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"])
    binary = str(Path(json.loads(metadata)["target_directory"]) / "debug/roomkit-code")
    with ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="roomkit-mail-")))
        peer = Peer(stack, root, options.ssh)
        owner = root / "api"
        (owner / "workspace").mkdir(parents=True)
        broker = stack.enter_context(redis())
        group = uuid.uuid4().hex
        model, unused = provider(stack, Reviewer), provider(stack, Sender)
        unused.called = threading.Event()
        model.waiting, model.release = threading.Event(), threading.Event()
        token = "PROOF-" + uuid.uuid4().hex
        peer.run(["python3", "-c", "import sys,pathlib; pathlib.Path(sys.argv[1]).write_text(sys.stdin.read())",
                  str(peer.work / "workspace/proof.txt")], token)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env.update(TERM="xterm-256color", ROOMKIT_EXECUTOR_REDIS_URL=broker)
        remote_url = f"redis://127.0.0.1:{peer.port(urlsplit(broker).port)}"
        model_port = peer.port(model.server_port)

        def open_reviewer(work, scope):
            argv = ["env", "TERM=xterm-256color", "ROOMKIT_EXECUTOR_REDIS_URL=" + remote_url,
                    *args_for(options.peer_binary or binary, work, "review", scope, model_port)]
            return Terminal(stack, peer.command(argv, tty=True), env)

        reviewer = open_reviewer(peer.work, group)
        sender = Terminal(stack, args_for(binary, owner, "api", group, unused.server_port), env)
        stack.callback(model.release.set)

        def another_session(dirname, name, scope, port=None):
            path = root / dirname
            (path / "workspace").mkdir(parents=True)
            return Terminal(stack, args_for(binary, path, name, scope, port or unused.server_port), env)

        outsider = another_session("outsider", "review", uuid.uuid4().hex)
        outsider.send("@api WRONG_GROUP", "Recipient unavailable")
        sender.send("@review Read proof.txt and send back its value.", "Message to @review")
        sender.wait(token, 0)
        reviewer.wait("PRIVATE_REVIEW_FINISHED", 0)
        os.write(reviewer.master, b"HOLD_FOR_MESSAGE\r")
        assert model.waiting.wait(12), "recipient did not enter the busy turn"
        sender.send("@review BUSY_PROBE", "Message to @review")
        reviewer.wait("BUSY_PROBE", 0)
        model.release.set()
        sender.wait("BUSY_ANSWER", 0)

        sender.send("@review SILENT_PROBE", "Message to @review")
        sender.wait("Message: SILENT_PROBE", 0)
        reviewer.wait("PRIVATE_SILENT_ANSWER", 0)

        duplicate = another_session("duplicate", "review", group)
        sender.send("@review AMBIGUOUS", "Recipient unavailable or ambiguous")
        os.write(duplicate.master, b"\x04")
        duplicate.proc.wait(timeout=15)
        check_histories(stack, owner, peer)
        assert not unused.called.is_set()
        os.write(reviewer.master, b"\x04")
        reviewer.proc.wait(timeout=15)
        sender.send("@review second question", "Recipient unavailable")
        os.write(sender.master, b"\x04")
        sender.proc.wait(timeout=15)
        agent_group = uuid.uuid4().hex
        workspace = peer.work / "agents" / "workspace"
        peer.run(["mkdir", "-p", str(workspace)])
        peer.run(["cp", str(peer.work / "workspace/proof.txt"), str(workspace / "proof.txt")])
        participant = open_reviewer(workspace.parent, agent_group)
        with drain_terminal(participant):
            exercise_agents(stack, root, lambda port: another_session("author", "author", agent_group, port), model, token)
            os.write(participant.master, b"\x04")
            participant.proc.wait(timeout=15)
        print("PASS", peer.run(["uname", "-sm"]).strip(), "read its own file and replied while idle and busy. Its transcript stayed private.")
        print("PASS separate stores, zero sender model requests, exact reply counts, ambiguous names, isolated groups, closed recipient.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ssh", help="run the second terminal on this SSH host")
    parser.add_argument("--peer-binary", help="path to the matching binary on the peer")
    opts = parser.parse_args()
    if opts.ssh and not opts.peer_binary:
        parser.error("--ssh requires --peer-binary")
    run(opts)
