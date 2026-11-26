"""A session group's key between two interactive sessions on a loopback Redis.

Each case launches a receiver and a sender in one new group, with
ROOMKIT_SESSION_GROUP_KEY set on both, on one, on neither, or to two
different secrets. The sender writes `@review <tag>` in its composer. A group
whose members share a key, or that has none, delivers; every other case is
refused before the receiver's agent sees anything, the sender is told what
to fix, and the receiver records the refusal in its diagnostics.
"""
from contextlib import ExitStack
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import uuid

from executor_fixture import StreamingModel, Terminal, redis
from session_messages_fixture import provider

KEY = "a" * 40
OTHER_KEY = "b" * 40
VARIABLE = "ROOMKIT_SESSION_GROUP_KEY"
# (case, sender's key, receiver's key, what the sender reads, the receiver's
# recorded refusal). The sender's notice wraps: only its first row is matched.
CASES = [
    ("one key on both", KEY, KEY, "Message to @review", None),
    ("no key on either", None, None, "Message to @review", None),
    ("a key on the sender only", KEY, None, f"refused the message: it has no {VARIABLE}", "keyless"),
    ("a key on the receiver only", None, KEY, "refused the message: it accepts only messages sealed", "unsealed"),
    ("two different keys", KEY, OTHER_KEY, f"refused the message: its {VARIABLE} differs", "other_key"),
]


class Receiver(StreamingModel):
    def reply(self, request):
        text = " ".join(str(m.get("content")) for m in request["messages"])
        if self.server.tag in text:
            self.server.received.set()
        return {"content": "RECEIVER_TURN_DONE"}


class Idle(StreamingModel):
    def reply(self, request):
        self.server.called.set()
        return {"content": "Unexpected sender turn"}


def args_for(binary, work, name, group, port):
    return [binary, str(work / "workspace"), "--state-dir", str(work / "state"),
            "--session", name, "--session-group", group, "--model", "fixture",
            "--base-url", f"http://127.0.0.1:{port}/v1", "--mode", "plan",
            "--no-update-check", "--no-herdr", "--no-terminal-status", "--color", "never"]


def refusals(state):
    """The refusals the session whose state is `state` recorded."""
    found = []
    for log in state.glob("projects/*/diagnostics.jsonl"):
        for line in log.read_text().splitlines():
            event = json.loads(line)
            if event.get("event") == "session_message_refused":
                found.append(event["refusal"])
    return found


def case(binary, broker, label, sender_key, receiver_key, told, refusal):
    with ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="roomkit-key-")))
        group = uuid.uuid4().hex
        model = provider(stack, Receiver)
        model.tag, model.received = "TAG_" + uuid.uuid4().hex[:12], threading.Event()
        idle = provider(stack, Idle)
        idle.called = threading.Event()
        base = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        base.update(TERM="xterm-256color", ROOMKIT_EXECUTOR_REDIS_URL=broker)

        def open_session(name, port, key):
            work = root / name
            (work / "workspace").mkdir(parents=True)
            env = dict(base, **({VARIABLE: key} if key else {}))
            return Terminal(stack, args_for(binary, work, name, group, port), env), work / "state"

        receiver, receiver_state = open_session("review", model.server_port, receiver_key)
        sender, _ = open_session("api", idle.server_port, sender_key)
        # Each says as it opens whether its group is sealed.
        for terminal, key in ((receiver, receiver_key), (sender, sender_key)):
            terminal.wait("messages are sealed with" if key else "is open: whoever can write", 0)
        sender.send(f"@review {model.tag}", told)
        delivered = model.received.wait(15 if refusal is None else 1)
        assert delivered == (refusal is None), f"{label}: delivered={delivered}"
        assert not idle.called.is_set(), f"{label}: the sender's model was asked"
        assert refusals(receiver_state) == ([refusal] if refusal else []), (label, refusals(receiver_state))
        for terminal in (sender, receiver):
            os.write(terminal.master, b"\x04")
            terminal.proc.wait(timeout=15)
        print("PASS", label)


def run():
    metadata = subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"])
    binary = str(Path(json.loads(metadata)["target_directory"]) / "debug/roomkit-code")
    with redis() as broker:
        for label, sender_key, receiver_key, told, refusal in CASES:
            case(binary, broker, label, sender_key, receiver_key, told, refusal)


if __name__ == "__main__":
    run()
