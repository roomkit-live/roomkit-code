"""Run the Linux executor POC in two processes, with an isolated Redis and scripted models."""

import argparse
from contextlib import ExitStack
import fcntl
from executor_fixture import StreamingModel, child, redis, tool
import json
import os
from pathlib import Path
import pty
import re
import select
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time
import uuid

from fixture_http import LoopbackHTTPServer
from pty_checks import model_fixture_env, wait_for_exit


# The terminal arms a newly drawn permission card after 500 ms.
APPROVAL_ARMING = .65


class Model(StreamingModel):

    def reply(self, request):
        last = request["messages"][-1]
        content = str(last.get("content", ""))
        if request["model"] == "owner":
            if last["role"] == "tool":
                return {"content": "Task handed to remote-linux."}
            lines = content.splitlines()
            notice = lines[1] if len(lines) > 1 else ""
            if notice.startswith("[Background task from remote-linux "):
                if " cancelled." in notice:
                    return {"content": "Worker cancellation received."}
                if " failed: " in notice:
                    return {"content": "Worker failure received."}
                if "Worker observed Plan refusal" in content:
                    return {"content": "Plan refusal received."}
                if self.server.proof in content:
                    return {"content": f"Worker report received {self.server.proof}."}
                return {"content": "Unexpected worker outcome."}
            tasks = {"Start the second task": "Hold the second task",
                     "Test worker loss": "Wait for worker loss",
                     "Test Plan mode": "Try writing in Plan"}
            task = next((task for message in reversed(request["messages"]) if message["role"] == "user"
                         for trigger, task in tasks.items() if trigger in str(message["content"])),
                        "Check the remote workspace")
            return tool("delegate_task", "delegate", agent="remote-linux", task=task)
        if request["model"] != "worker":
            raise ValueError("unexpected model")
        messages = json.dumps(request["messages"])
        if "Try writing in Plan" in messages:
            if last["role"] != "tool":
                return tool("write", "plan", path="plan.txt", content="blocked in Plan")
            if "Plan mode" in content and "disabled" in content:
                return {"content": "Worker observed Plan refusal."}
            return {"content": "Plan check failed."}
        if "Wait for worker loss" in messages:
            self.server.disconnecting.set()
            self.server.release.wait(60)
            return {"content": "Unexpected worker response after disconnect."}
        if "Hold the second task" in messages:
            self.server.working.set()
            if select.select([self.connection], [], [], 60)[0]:
                if not self.connection.recv(1, socket.MSG_PEEK):
                    self.server.cancelled.set()
            return tool("write", "late", path="late.txt", content="too late")
        if last["role"] != "tool":
            return tool("write", "first", path="denied.txt", content="first proposal")
        if last["tool_call_id"] == "first":
            return tool("write", "second", path="allowed.txt", content="worker edit")
        if last["tool_call_id"] == "second":
            return tool("read", "third", path="proof.txt")
        if self.server.proof in content:
            return {"content": f"Read worker proof {self.server.proof}."}
        return {"content": "Could not read worker proof."}

def automatic(owner, env, root, server, worker):
    hooks = root / "owner/.roomkit"
    hooks.mkdir()
    log = root / "owner/worker-hooks.jsonl"
    kinds = ("PreToolUse", "PostToolUse", "PostToolUseFailure")
    command = {"type":"command", "command":"cat >> worker-hooks.jsonl", "timeout":3}
    (hooks / "hooks.json").write_text(json.dumps({"hooks": {
        kind: [{"matcher":"^(read|write)$", "hooks":[command]}] for kind in kinds
    }}))
    master, slave = pty.openpty()
    captured = bytearray()
    with ExitStack() as stack:
        stack.callback(os.close, master)
        stack.callback(os.close, slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
        proc = stack.enter_context(child(owner, env, stdin=slave, stdout=slave, stderr=slave,
                                         start_new_session=True))

        def wait(text, offset=0):
            deadline = time.monotonic() + 25
            while text.encode() not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[offset:]):
                if proc.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"Missing {text!r}: {bytes(captured[-5000:])!r}")
                if select.select([master], [], [], .1)[0]:
                    captured.extend(os.read(master, 65536))

        wait("[Ask]")
        for index in range(len(kinds)):
            offset = len(captured)
            os.write(master, b"/hooks\r")
            wait("hooks discovered", offset)
            os.write(master, b"\x1b[B" * (index + 1) + b"\r")
            wait("Enable this hook", offset)
            offset = len(captured)
            os.write(master, b"\r")
            wait("State: enabled", offset)
            offset = len(captured)
            os.write(master, b"\x1b")
            wait("Ask about this project", offset)
        os.write(master, b"Check the remote workspace\r")
        wait("Allow write?")
        wait("denied.txt")
        assert not (root / "worker" / "denied.txt").exists()
        time.sleep(APPROVAL_ARMING)
        os.write(master, b"\x1b")  # Refuse this action from the parent.
        wait("allowed.txt")
        assert not (root / "worker" / "allowed.txt").exists()
        time.sleep(APPROVAL_ARMING)
        os.write(master, b"\x19")  # Allow once.
        wait("Worker report received " + server.proof)
        records = [json.loads(line) for line in log.read_text().splitlines()]
        def hook_results(kind):
            return {r["tool_use_id"]: r for r in records if r["hook_event_name"] == kind}
        assert {"first", "second"} <= hook_results("PreToolUse").keys(), records
        assert hook_results("PostToolUseFailure")["first"]["roomkit"]["outcome"] == "refused", records
        success = hook_results("PostToolUse")["second"]
        assert success["tool_input"]["path"] == "allowed.txt" and success["tool_response"], success
        assert success["roomkit"]["turn_id"] == hook_results("PreToolUse")["second"]["roomkit"]["turn_id"]
        assert (root / "worker" / "allowed.txt").read_text() == "worker edit"
        assert not (root / "worker" / "denied.txt").exists(), "Refused write reached the worker"
        assert set((root / "owner").iterdir()) == {hooks, log}
        print("PASS  Refusal, approval, worker-only file effects and result in the parent.", flush=True)

        offset = len(captured)
        os.write(master, b"Start the second task\r")
        wait("Task handed to remote-linux.", offset)
        assert server.working.wait(10), "The worker did not start the second task"
        os.write(master, b"/tasks next\r")
        wait("/back returns", offset)
        os.write(master, b"\x1b")
        wait("cancelled", offset)
        os.write(master, b"/back\r")
        wait("Worker cancellation received", offset)
        assert server.cancelled.wait(10), "Worker kept the cancelled model connection open"
        assert not (root / "worker" / "late.txt").exists()
        print("PASS  Open worker room, cancel the model request and return to parent.", flush=True)

        offset = len(captured)
        os.write(master, b"/mode plan\r")
        wait("[Plan]", offset)
        os.write(master, b"Test Plan mode\r")
        wait("Plan refusal received.", offset)
        assert not (root / "worker" / "plan.txt").exists()
        print("PASS  Parent Plan mode blocks a write on the worker.", flush=True)

        offset = len(captured)
        os.write(master, b"Test worker loss\r")
        wait("Task handed to remote-linux.", offset)
        assert server.disconnecting.wait(10), "Worker did not start before the simulated outage"
        worker.kill()
        worker.wait(5)
        wait("Worker failure received.", offset)
        print("PASS  Parent receives a failure when the running worker disappears.", flush=True)
        os.write(master, b"\x04")
        assert wait_for_exit(proc, master, captured) == 0
        assert set((root / "owner").iterdir()) == {hooks, log}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interactive", action="store_true", help="operate the real terminal yourself")
    interactive = parser.parse_args().interactive
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
    binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
    if not binary.is_file():
        parser.error("build roomkit-code first with cargo build --locked")
    with ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
        url = stack.enter_context(redis())
        env = model_fixture_env()
        env.update(ROOMKIT_EXECUTOR_REDIS_URL=url, ROOMKIT_NO_UPDATE_CHECK="true",
                   HOME=str(root / "home"), XDG_DATA_HOME=str(root / "data"),
                   XDG_CONFIG_HOME=str(root / "config"), TERM="xterm-256color")
        server = stack.enter_context(LoopbackHTTPServer(("127.0.0.1", 0), Model))
        server.proof = uuid.uuid4().hex[:12]
        server.working = threading.Event()
        server.cancelled = threading.Event()
        server.disconnecting = threading.Event()
        server.release = threading.Event()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.shutdown)
        stack.callback(server.release.set)
        group = "demo-" + uuid.uuid4().hex
        def args(role, flag):
            project = root / role
            project.mkdir()
            return [binary, project, flag, group, "--state-dir", root / (role + "-state"),
                    "--base-url", f"http://127.0.0.1:{server.server_port}/v1", "--model", role,
                    "--no-herdr", "--no-animations"]
        worker = args("worker", "--serve-worker")
        (root / "worker" / "proof.txt").write_text(server.proof)
        proc = stack.enter_context(child(worker, env, stdout=subprocess.PIPE, text=True))
        if not select.select([proc.stdout], [], [], 15)[0]:
            raise RuntimeError("Worker did not announce readiness")
        ready = proc.stdout.readline().strip()
        if not ready.startswith("Serving remote-linux"):
            raise RuntimeError("Worker failed to start")
        print(ready, flush=True)
        owner = args("owner", "--remote-worker")
        if interactive:
            print("Send 'Check the remote workspace'. Deny denied.txt and allow allowed.txt. "
                  "Then send 'Start the second task', open it with /tasks next and press Esc.", flush=True)
            subprocess.run(owner, env=env, check=True)
        else:
            automatic(owner, env, root, server, proc)


if __name__ == "__main__":
    main()
