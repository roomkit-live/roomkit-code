"""Check exact pool placement and the loss of a worker after it takes a turn."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import re
import select
import subprocess
import tempfile
import threading
from urllib.parse import urlsplit
import uuid

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
        if last["role"] == "tool":
            if last.get("tool_call_id") == "delegate-followup":
                self.server.task = json.loads(content)["task_id"]
                return {"content": "Follow-up task submitted."}
            if last.get("tool_call_id") == "followup":
                if "No running executor task with that ID" in content:
                    return {"content": "Worker refused the update."}
                assert json.loads(content)["status"] == "sent", content
                return {"content": "Worker accepted the update."}
            return {"content": "Unpinned task submitted."}
        if "[Background task from remote-linux " in content:
            if "Included the requested finding." in content:
                return {"content": "Owner received the revised finding."}
            if " failed." in content:
                return {"content": "Owner observed worker loss."}
            for worker in ("macbook", "linux"):
                if f"Proof from {worker}." in content:
                    return {"content": f"Owner received proof from {worker}."}
            return {"content": "Unexpected task result."}
        prompts = ("Start a review that accepts updates", "Pass the finding to my worker",
                   "Reach the tool round limit", "Delegate without a machine constraint")
        prompt = next((m["content"] for m in reversed(messages) if m["role"] == "user"
                       and m.get("content") in prompts), "")
        if "Start a review that accepts updates" in prompt:
            return tool("delegate_task", "delegate-followup", agent="remote-linux", task="Review before handback",
                        context={"executor": {"labels": {"fixture": "macbook"}}})
        if "Pass the finding to my worker" in prompt:
            return tool("send_message", "followup", to="task:" + self.server.task, text="Include the amber finding")
        if "Reach the tool round limit" in prompt:
            return tool("delegate_task", "limited", agent="remote-linux", task="Review until capped")
        return tool("delegate_task", "admit", agent="remote-linux", task="Hold after admission")

    def worker_reply(self, name, messages):
        transcript = json.dumps(messages)
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
        server.accepted = threading.Event()
        server.release = threading.Event()
        stack.callback(server.release.set)
        stops = {}
        for name, host, exe in (("linux", None, binary), ("macbook", options.ssh, options.peer_binary or binary)):
            peer = Peer(stack, root, host, name)
            stops[name] = start_worker(stack, peer, exe, name, group, server.server_port, urlsplit(url).port)
        (root / "workspace").mkdir()
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env.update(HOME=str(root), XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                   ROOMKIT_EXECUTOR_REDIS_URL=url, TERM="xterm-256color")
        term = Terminal(stack, [*app(binary, root, "owner", server.server_port), "--executor-pool", group], env)
        term.send("Start a review that accepts updates", "Follow-up task submitted.")
        assert server.review_started.wait(15), "Worker did not start"
        offset = term.send("Pass the finding to my worker", "Worker accepted the update.")
        server.release_review.set()
        term.wait("Owner received the revised finding.", offset)
        term.send("Pass the finding to my worker", "Worker refused the update.")
        print("PASS Owner steered its active pool task and could not reopen it by sending again.", flush=True)
        offset = term.send("Reach the tool round limit", "Unpinned task submitted.")
        term.wait("it reached the limit of tool rounds", offset)
        print("PASS The parent terminal received the reason the worker stopped.", flush=True)
        # Both workers expose the same profile. Only the chosen one may receive this task.
        offset = start_from_composer(term, "Confirm placement")
        term.wait("Owner received proof from macbook.", offset)
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
