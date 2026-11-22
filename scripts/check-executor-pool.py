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
import sys
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
    "review-one": {"labels": {"role": "review"}, "os": "macos" if sys.platform == "darwin" else "linux",
                   "profile": "code", "min_ram_mib": 1},
    "review-two": {"profile": "reviewer"},
    "ordinary-read": {},
    "new-worker-task": {"labels": {"role": "spare"}},
    "waiting-task": {"labels": {"role": "spare"}},
    "invalid-task": {"min_ram_mb": 1},
    "unpublished-task": {"profile": "private-agent"},
    "rust-failing": {"profile": "code", "labels": {"role": "review"}},
    "rust-passing": {"profile": "code", "labels": {"role": "review"}},
    "rust-denied": {"profile": "code", "labels": {"role": "review"}},
    "rust-plan": {"profile": "code", "labels": {"role": "review"}},
    "rust-diff": {"profile": "reviewer"},
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
            if all(value in content for value in ("profile code (default)", "profile reviewer", "model review-model",
                                                  "macbook [online]", "host macos", "model macbook", "role=review",
                                                   "linux [online]", "model linux", "role=general")):
                if request["model"] == "macbook":
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
                for task in self.server.rust:
                    if task + " reported." in content:
                        return {"content": task + " report received."}
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
        if name.startswith("rust-"):
            if last["role"] == "tool":
                self.server.rust[name] = content
                return {"content": name + " reported."}
            if name == "rust-diff":
                return tool("diff", "review-change", old_path="sample/before.rs", new_path="sample/src/lib.rs")
            return tool("cargo_test", "execute-test", cwd="sample")
        if last["role"] != "tool":
            self.server.started[name] = request["model"]
            if name in ("review-one", "review-two"):
                self.server.calls[name] = (self.path, {t["function"]["name"] for t in request["tools"]}, transcript)
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


def configure_pool(command, env, group, *, worker=None):
    """Save through the form, then launch the returned command without pool flags."""
    clean = {key: value for key, value in env.items() if key != "ROOMKIT_EXECUTOR_REDIS_URL"}
    with ExitStack() as editor:
        term = Terminal(editor, command + ["--local"], clean)
        term.send("/executors configure", "Schedule remote tasks on launch")
        url = env["ROOMKIT_EXECUTOR_REDIS_URL"]
        fields = [b"" if worker else b"\x1b[C", group.encode(), url.encode()]
        if worker:
            fields += [worker.encode(), b"\x151", b"role=review", b"code,reviewer", b"macos"]
        offset = len(term.output)
        os.write(term.master, b"\t".join(fields) + b"\x13")
        term.wait("Executor settings saved", offset)
        assert url.encode() not in term.output, "Redis connection was visible in the terminal"
    return (command + ["--executor"] if worker else command), clean


def eventually(predicate, message):
    end = time.monotonic() + 25
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.1)
    raise AssertionError(message)


def scenario(term, server, spare):
    term.send("/executors", "role=review")
    term.send("/executors", "profile reviewer")
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
    assert server.started["review-two"] == "review-model", server.started
    primary, review = server.calls["review-one"], server.calls["review-two"]
    assert primary[0] == "/v1/chat/completions" and "write" in primary[1]
    assert review[0] == "/review/v1/chat/completions" and "read" in review[1]
    assert "cargo_test" in primary[1] and "cargo_test" not in review[1]
    assert "write" not in review[1] and "shell" not in review[1] and "review-fixture-instruction" in review[2]
    print("PASS  Two exposed profiles shared one slot and workspace, with distinct connections, models and tools.", flush=True)
    term.send("unpublished-task", "Submitted unpublished-task.")
    term.send("/executors", "no online worker matches")
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
    assert all(name not in server.started for name in ("windows-task", "unpublished-task", "invalid-task", "waiting-task", "large-task")), server.started
    print("PASS  OS/RAM and invalid constraints enforced. New worker ran only the pending task.", flush=True)


def rust_scenario(term, server, root):
    crate = root / "macbook" / "sample"
    (crate / "src").mkdir(parents=True)
    (crate / "Cargo.toml").write_text('[package]\nname="sample"\nversion="0.1.0"\nedition="2021"\n')
    source = crate / "src" / "lib.rs"
    broken = "pub fn answer() -> u8 { 41 }\n#[test] fn expected_answer() { assert_eq!(answer(), 42); }\n"
    source.write_text(broken)
    (crate / "before.rs").write_text(broken)

    def result(task, decision=None):
        offset = term.send(task, "Submitted " + task + ".")
        profile = "reviewer · model review-model" if task == "rust-diff" else "code · model macbook"
        term.wait("assigned to macbook · profile " + profile, offset)
        if decision is not None:
            term.wait("Allow cargo_test?", offset)
            time.sleep(.75)  # The approval card arms after it appears.
            os.write(term.master, decision)
        term.wait(task + " report received.", offset)
        return server.rust[task]

    assert "plan" in result("rust-plan").lower() and not (crate / "target").exists()
    term.send("/mode ask", "[Ask]")
    assert "exit_code=" not in result("rust-denied", b"\x1b") and not (crate / "target").exists()
    for task, code, verdict in [("rust-failing", 101, "FAILED"), ("rust-passing", 0, "ok")]:
        if not code:
            term.send("/mode yolo", "[Yolo]")
            source.write_text(broken.replace("{ 41 }", "{ 42 }"))
        report = result(task, b"\x19" if code else None)
        assert f"exit_code={code}" in report.splitlines(), report
        assert "expected_answer" in report and verdict in report, report
    diff = result("rust-diff")
    assert "41" in diff and "42" in diff, diff
    term.send("/executors", "assigned to macbook · profile reviewer · model review-model")
    print("PASS  Plan blocked Cargo, Ask handled refusal and approval, and the review followed two real test runs.", flush=True)


def model_server(stack):
    server = stack.enter_context(LoopbackHTTPServer(("127.0.0.1", 0), Model))
    server.started, server.finished, server.ids, server.calls = {}, {}, {}, {}
    server.rust = {}
    server.release = threading.Event()
    server.worker_saw_inventory = threading.Event()
    server.markers = {name: uuid.uuid4().hex for name in ("macbook", "linux", "spare")}
    threading.Thread(target=server.serve_forever, daemon=True).start()
    stack.callback(server.shutdown)
    stack.callback(server.release.set)
    return server


def configure_profiles(command, env, state):
    # Bootstrap through the CLI. An unknown export must fail before registering.
    probe = subprocess.run(command + ["--worker-profile", "missing"], env=env,
                           capture_output=True, text=True, timeout=12)
    assert probe.returncode and "Unknown worker profile missing" in probe.stderr, probe.stderr
    agents_file = next(state.glob("projects/*/agents.json"))
    agents = json.loads(agents_file.read_text())
    original = agents["agents"]["code"]
    original["tools"].append("cargo_test")
    connections_file = state / "connections.json"
    connections = json.loads(connections_file.read_text())
    primary = connections["profiles"][original["provider"]]
    connections["profiles"]["review"] = dict(primary, id="review", label="Review endpoint",
                                              base_url=primary["base_url"].removesuffix("/v1") + "/review/v1")
    connections_file.write_text(json.dumps(connections))
    reviewer = dict(original, id="reviewer", label="Reviewer", provider="review", model="review-model",
                    tools=["read", "diff", "shell"], instructions="review-fixture-instruction")
    agents["agents"]["reviewer"] = reviewer
    agents["agents"]["private-agent"] = dict(original, id="private-agent", model="private-model")
    agents["selected"] = "private-agent"
    agents_file.write_text(json.dumps(agents))
    duplicate = subprocess.run(command + ["--worker-profile", "code", "--worker-profile", "code"],
                               env=env, capture_output=True, text=True, timeout=12)
    assert duplicate.returncode and "Duplicate worker profile code" in duplicate.stderr, duplicate.stderr



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
        # Rustup shims cannot select a toolchain under the isolated HOME.
        binaries = root / "bin"
        binaries.mkdir()
        for program in ("cargo", "rustc", "rustdoc"):
            path = subprocess.check_output(["rustup", "which", program], text=True).strip()
            (binaries / program).symlink_to(path)
        env["PATH"] = str(binaries) + os.pathsep + env["PATH"]
        server = model_server(stack)

        def args(role):
            project = root / role
            project.mkdir()
            return [binary, project, "--state-dir", root / (role + "-state"),
                    "--base-url", f"http://127.0.0.1:{server.server_port}/v1", "--model", role,
                    "--mode", "plan", "--no-herdr", "--no-animations"]

        def worker(name, label):
            local = args(name)
            command = local + ["--serve-pool", group, "--worker-name", name, "--worker-label",
                               "role=" + label, "--worker-slots", "1"]
            command.extend(["--worker-host-os", "macos" if name == "macbook" else "linux"])
            worker_env = env
            if name == "macbook":
                configure_profiles(command, env, root / "macbook-state")
                # Reuse the bootstrap workspace and provider settings in the editor.
                command, worker_env = configure_pool(local, env, group, worker=name)
            (root / name / "marker.txt").write_text(server.markers[name])
            proc = stack.enter_context(child(command, worker_env, stdout=subprocess.PIPE, text=True))
            if not select.select([proc.stdout], [], [], 20)[0] or not proc.stdout.readline().startswith("Serving"):
                raise RuntimeError("Worker failed to register: " + name)

        owner_args, owner_env = configure_pool(args("owner"), env, group)
        term = Terminal(stack, owner_args, owner_env)
        term.send(INVENTORY_QUESTION, "Live inventory is empty.")
        term.send("bootstrap-task", "Submitted bootstrap-task.")
        term.send("/executors", "no online worker matches")
        worker("linux", "general")
        eventually(lambda: server.finished.get("bootstrap-task") == "linux",
                   "Task submitted to an empty pool did not wake")
        print("PASS  A task submitted before the first worker joined completed.", flush=True)
        worker("macbook", "review")
        term.send(INVENTORY_QUESTION, "Live inventory includes the MacBook and Linux workstation.")
        print("PASS  Coordinator and multi-profile worker launched from saved terminal settings.", flush=True)
        term.send("inspect-pool-from-worker", "Submitted inspect-pool-from-worker.")
        assert server.worker_saw_inventory.wait(15), "Pool worker could not read the inventory"
        print("PASS  Owner and delegated worker can inspect the same live pool as /executors.", flush=True)
        scenario(term, server, lambda: worker("spare", "spare"))
        fresh = Terminal(stack, [*owner_args, "--session", "rust-tests"], owner_env)
        rust_scenario(fresh, server, root)


if __name__ == "__main__":
    main()

