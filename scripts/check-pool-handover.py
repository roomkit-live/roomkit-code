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
            if last["role"] == "tool":
                return {"content": "Unpinned task submitted."}
            if "[Background task from remote-linux " in content:
                if " failed." in content:
                    return {"content": "Owner observed worker loss."}
                for worker in ("macbook", "linux"):
                    if f"Proof from {worker}." in content:
                        return {"content": f"Owner received proof from {worker}."}
                return {"content": "Unexpected task result."}
            return tool("delegate_task", "admit", agent="remote-linux", task="Hold after admission")

        if "Hold after admission" in json.dumps(messages):
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
            *app(binary, work, name, peer.port(port)), "--serve-pool", group, "--worker-name", name]
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
