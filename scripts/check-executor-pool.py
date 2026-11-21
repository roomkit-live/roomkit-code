"""Exercise placement, queued cancellation and overlapping work through a real terminal."""
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import threading
import time
import uuid

from executor_fixture import StreamingModel, child, redis, tool
from fixture_http import LoopbackHTTPServer

TASKS = {
    "inspect-pool-from-worker": {"labels": {"role": "review"}},
    "bootstrap-task": {"labels": {"role": "general"}},
    "review-one": {"labels": {"role": "review"}, "os": "linux", "min_ram_mib": 1},
    "review-two": {"labels": {"role": "review"}},
    "ordinary-read": {},
    "new-worker-task": {"labels": {"role": "spare"}},
    "waiting-task": {"labels": {"role": "spare"}},
    "invalid-task": {"min_ram_mb": 1},
    "windows-task": {"os": "windows"},
    "large-task": {"min_ram_mib": 2**48},
}


INVENTORY_QUESTION = "Which executor machines are in the pool?"


def inventory_call(request):
    names = {entry["function"]["name"] for entry in request["tools"]}
    if "list_executors" in names:
        return tool("list_executors", "inventory")
    return {"content": "No executor inventory tool is available."}


class Model(StreamingModel):
    def reply(self, request):
        last = request["messages"][-1]
        content = str(last.get("content", ""))
        if last.get("tool_call_id") == "inventory":
            if "No workers registered" in content:
                return {"content": "Live inventory is empty."}
            if all(value in content for value in ("macbook [online]", "host macos", "model macbook", "role=review",
                                                   "linux [online]", "model linux", "role=general")):
                if request["model"] != "owner":
                    self.server.worker_saw_inventory.set()
                return {"content": "Live inventory includes the MacBook and Linux workstation."}
            return {"content": "Incomplete pool inventory."}
        if request["model"] == "owner":
            if last["role"] == "tool":
                if "Invalid executor requirements" in content:
                    return {"content": "Requirements rejected."}
                task_id = re.search(r"task-[a-z0-9]+", content)
                if task_id:
                    self.server.ids[self.server.submitting] = task_id.group()
                return {"content": "Submitted " + self.server.submitting + "."}
            if "[Background task from remote-linux " in content:
                return {"content": "Background report received."}
            content = next((str(m.get("content", "")) for m in reversed(request["messages"])
                            if m["role"] == "user" and m.get("content") in (*TASKS, "cancel-waiting", INVENTORY_QUESTION)), "")
            if content == INVENTORY_QUESTION:
                return inventory_call(request)
            if "cancel-waiting" in content:
                self.server.submitting = "cancel-waiting"
                return tool("cancel_task", "cancel", task_id=self.server.ids["waiting-task"])
            name = next((key for key in TASKS if key in content), None)
            if name is None:
                return {"content": "No task requested."}
            self.server.submitting = name
            return tool("delegate_task", "submit", agent="remote-linux", task=name,
                        context={"executor": TASKS[name]})

        transcript = json.dumps(request["messages"])
        name = next((key for key in TASKS if key in transcript), "unknown")
        if "inspect-pool-from-worker" in transcript:
            return inventory_call(request)
        if last["role"] != "tool":
            self.server.started[name] = request["model"]
            if name == "review-one":
                self.server.release.wait(90)
            return tool("read", "marker", path="marker.txt")
        for worker, marker in self.server.markers.items():
            if marker in content:
                self.server.finished[name] = worker
                return {"content": f"{name} read the marker on {worker}."}
        return {"content": "Missing workspace marker."}


class Terminal:
    def __init__(self, stack, args, env):
        self.master, slave = pty.openpty()
        stack.callback(os.close, self.master)
        stack.callback(os.close, slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 58, 200, 0, 0))
        self.proc = stack.enter_context(child(args, env, stdin=slave, stdout=slave,
                                              stderr=slave, start_new_session=True))
        self.output = bytearray()
        self.wait("[Plan]", 0)

    def wait(self, text, offset):
        end = time.monotonic() + 35
        while time.monotonic() < end and self.proc.poll() is None:
            if select.select([self.master], [], [], .1)[0]:
                self.output.extend(os.read(self.master, 65536))
            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.output[offset:])
            if text.encode() in plain:
                return
        raise AssertionError(f"Terminal did not show {text!r}: {self.output[-2800:]!r}")

    def send(self, message, expect):
        offset = len(self.output)
        os.write(self.master, message.encode() + b"\r")
        if message == "/executors":
            self.wait("PgUp/PgDn output", offset)
        self.wait(expect, offset)
        if message == "/executors":
            os.write(self.master, b"\x1b")
            time.sleep(.2)
        return offset


def eventually(predicate, message):
    end = time.monotonic() + 25
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.1)
    raise AssertionError(message)


def scenario(term, server, spare):
    term.send("/executors", "role=review")
    term.send("review-one", "Submitted review-one.")
    eventually(lambda: server.started.get("review-one") == "macbook", "First review misplaced")
    term.send("review-two", "Submitted review-two.")
    term.send("/executors", "all matching slots busy")
    term.send("ordinary-read", "Submitted ordinary-read.")
    eventually(lambda: server.finished.get("ordinary-read") == "linux", "Linux did not read its marker")
    assert "review-two" not in server.started
    print("PASS  Second review queued while Linux read its own workspace.", flush=True)
    server.release.set()
    eventually(lambda: all(server.finished.get(t) == "macbook" for t in ("review-one", "review-two")),
               "Both reviews must read the Mac worker workspace")
    print("PASS  Both reviews read the Mac marker, in order, with one slot.", flush=True)
    term.send("invalid-task", "Requirements rejected.")
    term.send("windows-task", "Submitted windows-task.")
    term.send("/executors", "no online worker matches")
    term.send("large-task", "Submitted large-task.")
    term.send("/executors", "insufficient available RAM")
    term.send("waiting-task", "Submitted waiting-task.")
    term.send("cancel-waiting", "Submitted cancel-waiting.")
    term.send("new-worker-task", "Submitted new-worker-task.")
    spare()
    eventually(lambda: server.finished.get("new-worker-task") == "spare",
               "A queued task did not wake when a compatible worker joined")
    assert all(name not in server.started for name in ("windows-task", "invalid-task", "waiting-task", "large-task")), server.started
    print("PASS  OS/RAM and invalid constraints enforced. New worker ran only the pending task.", flush=True)


def model_server(stack):
    server = stack.enter_context(LoopbackHTTPServer(("127.0.0.1", 0), Model))
    server.started, server.finished, server.ids = {}, {}, {}
    server.release = threading.Event()
    server.worker_saw_inventory = threading.Event()
    server.markers = {name: uuid.uuid4().hex for name in ("macbook", "linux", "spare")}
    threading.Thread(target=server.serve_forever, daemon=True).start()
    stack.callback(server.shutdown)
    stack.callback(server.release.set)
    return server


def main():
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
    binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
    with ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
        group = "pool-" + uuid.uuid4().hex
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env.update(ROOMKIT_EXECUTOR_REDIS_URL=stack.enter_context(redis()), HOME=str(root/"home"),
                   XDG_CONFIG_HOME=str(root/"config"), XDG_DATA_HOME=str(root/"data"),
                   ROOMKIT_NO_UPDATE_CHECK="true", TERM="xterm-256color")
        server = model_server(stack)

        def args(role, flag):
            project = root / role
            project.mkdir()
            return [binary, project, flag, group, "--state-dir", root / (role + "-state"),
                    "--base-url", f"http://127.0.0.1:{server.server_port}/v1", "--model", role,
                    "--mode", "plan", "--no-herdr", "--no-animations"]

        def worker(name, label):
            command = args(name, "--serve-pool") + ["--worker-name", name, "--worker-label",
                                                    "role=" + label, "--worker-slots", "1"]
            command.extend(["--worker-host-os", "macos" if name == "macbook" else "linux"])
            (root / name / "marker.txt").write_text(server.markers[name])
            proc = stack.enter_context(child(command, env, stdout=subprocess.PIPE, text=True))
            if not select.select([proc.stdout], [], [], 20)[0] or not proc.stdout.readline().startswith("Serving"):
                raise RuntimeError("Worker failed to register: " + name)

        term = Terminal(stack, args("owner", "--executor-pool"), env)
        term.send(INVENTORY_QUESTION, "Live inventory is empty.")
        term.send("bootstrap-task", "Submitted bootstrap-task.")
        term.send("/executors", "no online worker matches")
        worker("linux", "general")
        eventually(lambda: server.finished.get("bootstrap-task") == "linux",
                   "Task submitted to an empty pool did not wake")
        print("PASS  A task submitted before the first worker joined completed.", flush=True)
        worker("macbook", "review")
        term.send(INVENTORY_QUESTION, "Live inventory includes the MacBook and Linux workstation.")
        term.send("inspect-pool-from-worker", "Submitted inspect-pool-from-worker.")
        assert server.worker_saw_inventory.wait(15), "Pool worker could not read the inventory"
        print("PASS  Owner and delegated worker can inspect the same live pool as /executors.", flush=True)
        scenario(term, server, lambda: worker("spare", "spare"))


if __name__ == "__main__":
    main()

