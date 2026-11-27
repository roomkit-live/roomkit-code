"""Check exact pool placement and the loss of a worker after it takes a turn."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import re
import select
import sqlite3
import subprocess
import tempfile
import threading
import time
from urllib.parse import urlsplit
import uuid

from pty_checks import model_fixture_env
from executor_fixture import StreamingModel, Terminal, child, redis, start_from_composer, tool
from executor_peer_fixture import Peer
from session_messages_fixture import provider


class Model(StreamingModel):
    def reply(self, request):
        name = request["model"]
        messages = request["messages"]
        last = messages[-1]
        content = str(last["content"])
        if name == "owner":
            return self.owner_reply(messages, last, content)

        return self.worker_reply(name, messages)

    def owner_reply(self, messages, last, content):
        self.server.owner_inputs.append(content)
        if last["role"] == "tool":
            if last.get("tool_call_id") == "delegate-followup":
                self.server.task = json.loads(content)["task_id"]
                self.server.room = json.loads(content)["child_room_id"]
                return {"content": "Follow-up task submitted."}
            if last.get("tool_call_id") == "limited":
                self.server.failed_room = json.loads(content)["child_room_id"]
                return {"content": "Unpinned task submitted."}
            if last.get("tool_call_id") == "ask-live":
                msg = json.loads(content)
                assert msg["status"] == "sent"
                self.server.live_id = msg["message_id"]
                return {"content": "Question sent to the running reviewer."}
            if last.get("tool_call_id") == "ask-failed":
                assert json.loads(content)["status"] == "sent"
                return {"content": "Asked the failed reviewer."}
            if last.get("tool_call_id") == "find-review":
                sessions = json.loads(content)["sessions"]
                target, = [s for s in sessions if s["name"] == "Review before handback"]
                assert target["to"] == self.server.room and target["task_status"] == "completed", target
                assert any(s["name"] == "Confirm placement" and s["to"] != target["to"] for s in sessions)
                return tool("send_message", "ask-review", to=target["to"], text="Which finding did we add?")
            if last.get("tool_call_id") == "ask-review":
                assert json.loads(content)["status"] == "sent", content
                return {"content": "Question sent to the finished review."}
            if last.get("tool_call_id") == "followup":
                if "No running executor task with that ID" in content:
                    return {"content": "Worker refused the update."}
                assert json.loads(content)["status"] == "sent", content
                return {"content": "Worker accepted the update."}
            return {"content": "Unpinned task submitted."}
        result = next((str(m["content"]) for m in messages[-2:] if "Session correspondence result." in str(m.get("content"))), "")
        if result:
            if "without sending a reply" in result:
                return {"content": "Owner got the delivery notice."}
            if "I am reviewing." in result:
                return {"content": "Owner got the live answer."}
            if "The finding was amber." in result:
                return {"content": "Owner got the original reviewer's answer."}
        # Each hand-back opens a turn of its own: one handed to a running
        # turn would read as a participant's words, its mark a copy, and the
        # wait for its answer fails (RoomKit RMK-2146).
        if "[Background task from remote-linux " in content:
            if "Included the requested finding." in content:
                return {"content": "Owner received the revised finding."}
            if "[Background task from remote-linux failed: " in content:
                return {"content": "Owner observed worker loss."}
            for worker in ("macbook", "linux"):
                if f"Proof from {worker}." in content:
                    return {"content": f"Owner received proof from {worker}."}
            return {"content": "Unexpected task result."}
        prompts = ("Start a review that accepts updates", "Pass the finding to my worker",
                   "Reach the tool round limit", "Delegate without a machine constraint", "Ask the finished reviewer",
                   "Ask the reviewer now", "Ask the failed reviewer")
        prompt = next((candidate for m in reversed(messages) if m["role"] == "user"
                       for line in reversed(str(m.get("content")).splitlines())
                       for candidate in prompts if line == candidate or line.endswith(": " + json.dumps(candidate))), "")
        if prompt == "Ask the reviewer now":
            return tool("send_message", "ask-live", to=self.server.room, text="What are you doing?")
        if prompt == "Ask the failed reviewer":
            return tool("send_message", "ask-failed", to=self.server.failed_room, text="Did you finish?")
        if prompt == "Ask the finished reviewer":
            return tool("list_sessions", "find-review")
        if "Start a review that accepts updates" in prompt:
            return tool("delegate_task", "delegate-followup", agent="remote-linux", task="Review before handback",
                        context={"executor": {"labels": {"fixture": "macbook"}}})
        if "Pass the finding to my worker" in prompt:
            return tool("send_message", "followup", to="task:" + self.server.task, text="Include the amber finding")
        if "Reach the tool round limit" in prompt:
            return tool("delegate_task", "limited", agent="remote-linux", task="Review until capped")
        assert prompt == "Delegate without a machine constraint", (prompt, last)
        return tool("delegate_task", "admit", agent="remote-linux", task="Hold after admission")

    def worker_reply(self, name, messages):
        transcript = json.dumps(messages)
        last = messages[-1]
        if last.get("tool_call_id") == "answer-review":
            assert json.loads(last["content"])["status"] == "sent", last
            return {"content": "PRIVATE_WORKER_CHAT"}
        if last.get("tool_call_id") == "answer-live":
            assert json.loads(last["content"])["status"] == "sent", last
            return {"content": "Included the requested finding."}
        incoming = next((str(m["content"]) for m in reversed(messages)
                         if "Incoming session correspondence." in str(m.get("content"))), "")
        if "Did you finish?" in incoming:
            return {"content":"PRIVATE_FAILURE_DISCUSSION"}
        if "What are you doing?" in incoming:
            msg_id = re.search(r"session-msg-[a-f0-9]{64}", incoming).group()
            return tool("send_message", "answer-live", reply_to=msg_id, text="I am reviewing.")
        if "Which finding did we add?" in incoming:
            assert name == "macbook" and "Included the requested finding." in transcript
            msg_id = re.search(r"session-msg-[a-f0-9]{64}", incoming).group()
            return tool("send_message", "answer-review", reply_to=msg_id, text="The finding was amber.")
        if "Review until capped" in transcript:
            return tool("list", "list-" + str(len(messages)), path=".")
        if "Review before handback" in transcript:
            if "Include the amber finding" in transcript:
                return {"content": "Included the requested finding."}
            self.server.review_started.set()
            self.server.release_review.wait(45)
            return tool("list", "inspect", path=".")
        if "Hold after admission" in transcript:
            self.server.attempts.append(name)
            self.server.accepted.set()
            self.server.release.wait(75)
        return {"content": f"Proof from {name}."}


def app(binary, work, model, port):
    return [binary, str(work / "workspace"), "--state-dir", str(work / "state"),
            "--base-url", f"http://127.0.0.1:{port}/v1", "--model", model,
            "--mode", "plan", "--no-update-check", "--no-herdr", "--no-animations"]


def start_worker(stack, peer, binary, name, group, port, redis_port):
    work = peer.work
    args = ["env", f"HOME={work}", f"XDG_DATA_HOME={work}/data", f"XDG_CONFIG_HOME={work}/config",
            f"ROOMKIT_EXECUTOR_REDIS_URL=redis://127.0.0.1:{peer.port(redis_port)}",
            *app(binary, work, name, peer.port(port)), "--serve-pool", group, "--worker-name", name,
            "--worker-label", f"fixture={name}", "--max-tool-rounds", "3"]
    proc = stack.enter_context(child(peer.command(args), os.environ, stdout=subprocess.PIPE, text=True))
    ready = select.select([proc.stdout], [], [], 25)[0]
    pid = re.search(r"^Serving .*PID (\d+)\)", proc.stdout.readline()) if ready else None
    assert pid, f"{name} did not announce its instance"
    def stop(signal="-TERM"):
        if proc.poll() is None:
            peer.run(["kill", signal, pid[1]])
            proc.wait(10)
    stack.callback(stop)
    return stop


def run(options):
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"]))
    binary = str(Path(metadata["target_directory"]) / "debug/roomkit-code")
    with ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="pool-handover-")))
        url = stack.enter_context(redis())
        group = "handover-" + uuid.uuid4().hex
        server = provider(stack, Model)
        server.review_started = threading.Event()
        server.release_review = threading.Event()
        stack.callback(server.release_review.set)
        server.attempts = []
        server.owner_inputs = []
        server.accepted = threading.Event()
        server.release = threading.Event()
        stack.callback(server.release.set)
        stops = {}
        for name, host, exe in (("linux", None, binary), ("macbook", options.ssh, options.peer_binary or binary)):
            peer = Peer(stack, root, host, name)
            stops[name] = start_worker(stack, peer, exe, name, group, server.server_port, urlsplit(url).port)
        (root / "workspace").mkdir()
        env = model_fixture_env()
        env.update(HOME=str(root), XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                   ROOMKIT_EXECUTOR_REDIS_URL=url, TERM="xterm-256color")
        owner = [*app(binary, root, "owner", server.server_port), "--executor-pool", group, "--session", "owner-pool"]
        term = Terminal(stack, owner, env)
        term.send("Start a review that accepts updates", "Follow-up task submitted.")
        assert server.review_started.wait(15), "Worker did not start"
        offset = term.send("Pass the finding to my worker", "Worker accepted the update.")
        db = stack.enter_context(sqlite3.connect(next((root / "state").glob("projects/*/history.sqlite3"))))
        term.send("Ask the reviewer now", "Question sent to the running reviewer.")
        # Release the simulated LLM only after RoomKit steered its active turn.
        deadline = time.monotonic() + 15
        while not db.execute("SELECT 1 FROM private_state WHERE key = ? AND json_extract(value, '$.id') = ? AND json_extract(value, '$.dispatch') = 'Delivered'",
                             ("roomkit_code:session_mail_dispatch:v1", server.live_id)).fetchone():
            assert time.monotonic() < deadline, "The live question was not delivered"
            time.sleep(.05)
        server.release_review.set()
        term.wait("Owner received the revised finding.", offset)
        term.wait("Owner got the live answer.", offset)
        term.send("Pass the finding to my worker", "Worker refused the update.")
        print("PASS Owner steered its active pool task and could not reopen it by sending again.", flush=True)
        offset = term.send("Reach the tool round limit", "Unpinned task submitted.")
        term.wait("it reached the limit of tool rounds", offset)
        print("PASS The parent terminal received the reason the worker stopped.", flush=True)
        # Both workers expose the same profile. Only the chosen one may receive this task.
        offset = start_from_composer(term, "Confirm placement")
        try:
            term.wait("Owner received proof from macbook.", offset)
        except AssertionError:
            # What the owner's model was given tells why: two hand-backs
            # in one turn showed here before RoomKit RMK-2146.
            for given in server.owner_inputs[-3:]:
                print("Owner was given:", json.dumps(given[:1500]), flush=True)
            raise
        # The resumed coordinator has no in-memory worker queue or previous turn.
        os.write(term.master, b"\x04")
        assert term.proc.wait(15) == 0
        term = Terminal(stack, owner, env)
        offset = term.send("Ask the finished reviewer", "Question sent to the finished review.")
        term.wait("Owner got the original reviewer's answer.", offset)
        stored, = db.execute("SELECT data FROM rooms WHERE id = ?", (server.room,)).fetchone()
        assert json.loads(stored)["metadata"]["task_status"] == "completed"
        offset = term.send("Ask the failed reviewer", "Asked the failed reviewer.")
        term.wait("Owner got the delivery notice.", offset)
        failed, = db.execute("SELECT data FROM rooms WHERE id = ?", (server.failed_room,)).fetchone()
        assert json.loads(failed)["metadata"]["task_status"] == "failed"
        assert b"PRIVATE_WORKER_CHAT" not in term.output and b"PRIVATE_FAILURE_DISCUSSION" not in term.output
        print("PASS After coordinator restart, the original reviewer answered, a silent worker produced a delivery notice, and both task outcomes were preserved.", flush=True)
        offset = term.send("Delegate without a machine constraint", "Unpinned task submitted.")
        assert server.accepted.wait(15), "The worker never took the unpinned task"
        lost, = server.attempts
        stops[lost]("-KILL")
        term.wait("Owner observed worker loss.", offset)
        survivor = next(name for name in stops if name != lost)
        offset = start_from_composer(term, "Confirm survivor", survivor)
        term.wait(f"Owner received proof from {survivor}.", offset)
        assert server.attempts == [lost], f"An admitted task was replayed: {server.attempts}"
        print(f"PASS Named placement on macbook; loss of {lost} reported, no replay; {survivor} served fresh work.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ssh", help="run the macbook worker on this SSH host")
    parser.add_argument("--peer-binary", help="matching binary on the SSH host")
    options = parser.parse_args()
    if bool(options.ssh) != bool(options.peer_binary):
        parser.error("--ssh and --peer-binary must be supplied together")
    run(options)
