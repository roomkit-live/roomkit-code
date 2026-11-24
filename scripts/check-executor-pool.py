"""Exercise placement, queued cancellation and overlapping work through a real terminal."""
from contextlib import ExitStack
import json
import os
from pathlib import Path
import re
import select
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid

import executor_consultation_fixture as peers
from executor_fixture import StreamingModel, Terminal, child, redis, tool
from fixture_http import LoopbackHTTPServer


class PoolTerminal(Terminal):
    def send(self, message, expect):
        offset = super().send(message, expect)
        if message == "/executors tasks":
            self.wait("PgUp/PgDn output", offset)
            os.write(self.master, b"\x1b")
            time.sleep(.2)
        return offset


TASKS = {
    peers.AUTHOR: {"labels": {"role": "general"}, "consultation": True},
    peers.ENDING: {"labels": {"role": "general"}, "consultation": True},
    peers.UNANSWERED: {"labels": {"role": "review"}, "consultation": True},
    peers.REVIEW: {"labels": {"role": "review"}, "consultation": True},
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
            workspaces = (f"workspace {path} · RoomKit Code {self.server.version}"
                          for path in self.server.workspaces.values())
            if all(path in content for path in workspaces) and all(value in content for value in ("profile code (default)", "profile reviewer", "model review-model",
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
                if "Peer ended while I was waiting." in content:
                    return {"content": "Unanswered consultation returned to the parent."}
                if "Consultation finished." in content:
                    return {"content": "Consultation returned to the parent."}
                for task in self.server.rust:
                    if task + " reported." in content:
                        if task.startswith("rust-pin-") and "exit_code=" in self.server.rust[task]:
                            evidence = [line for line in self.server.rust[task].splitlines()
                                        if line.startswith(("commit=", "platform=", "duration_ms=", "exit_code="))]
                            assert all(line in content for line in evidence)
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
            context = {"executor": TASKS[name]}
            if name in (peers.REVIEW, peers.UNANSWERED):
                target = peers.AUTHOR if name == peers.REVIEW else peers.ENDING
                context["peer_task_id"] = self.server.ids[target]
            return tool("delegate_task", "submit", agent="remote-linux", task=name, context=context)

        transcript = json.dumps(request["messages"])
        name = next((key for key in TASKS if key in transcript), "unknown")
        if name in (peers.AUTHOR, peers.REVIEW, peers.ENDING, peers.UNANSWERED):
            return self.server.peer.reply(name, request["messages"])
        if "inspect-pool-from-worker" in transcript:
            return inventory_call(request)
        if name.startswith("rust-"):
            if last["role"] == "tool":
                self.server.rust[name] = content
                if name.startswith("rust-pin-"):
                    return {"content": name + " reported.\n" + content}
                return {"content": name + " reported."}
            if name == "rust-diff":
                return tool("diff", "review-change", old_path="sample/before.rs", new_path="sample/src/lib.rs")
            arguments = {"cwd": "sample"}
            if name.startswith("rust-pin-") and name != "rust-pin-omitted":
                arguments["commit"] = TASKS[name]["test_commit"]
                assert arguments["commit"] in transcript
            return tool("cargo_test", "execute-test", **arguments)
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


def save_pool(term, fields, notice="Executor settings saved"):
    term.send("/executors configure", "Schedule remote tasks")
    offset = len(term.output)
    os.write(term.master, b"\t".join(fields) + b"\x13")
    term.wait(notice, offset)


def configure_pool(command, env, group, *, worker):
    """Save the worker's local configuration without running the service in the editor."""
    clean = {key: value for key, value in env.items() if key != "ROOMKIT_EXECUTOR_REDIS_URL"}
    with ExitStack() as editor:
        term = PoolTerminal(editor, command + ["--local"], clean)
        url = env["ROOMKIT_EXECUTOR_REDIS_URL"].encode()
        save_pool(term, [b"", group.encode(), url, worker.encode(), b"\x151",
                         b"role=review", b"code,reviewer", b"macos"])
        assert url not in term.output, "Redis connection was visible in the terminal"
    return command + ["--executor"], clean


def eventually(predicate, message):
    end = time.monotonic() + 25
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.1)
    raise AssertionError(message)


def scenario(term, server, spare):
    term.send("/executors", "2/2 online")
    offset = term.send("macbook", "Inspect reviewer")
    term.wait(str(server.workspaces["macbook"]), offset)
    term.wait(server.version, offset)
    offset = len(term.output)
    os.write(term.master, b"\x1b[B\r")
    term.wait("Tools (", offset)
    term.wait("review-model", offset)
    os.write(term.master, b"\x1b")
    time.sleep(.3)
    term.send("review-one", "Submitted review-one.")
    eventually(lambda: server.started.get("review-one") == "macbook", "First review misplaced")
    term.send("review-two", "Submitted review-two.")
    term.send("/executors tasks", "all matching slots busy")
    save_pool(term, [b"\x1b[D"], "Finish or cancel")
    os.write(term.master, b"\x1b")
    time.sleep(.3)
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
    term.send("/executors tasks", "no online worker matches")
    term.send("invalid-task", "Requirements rejected.")
    term.send("windows-task", "Submitted windows-task.")
    term.send("/executors tasks", "no online worker matches")
    term.send("large-task", "Submitted large-task.")
    term.send("/executors tasks", "insufficient available RAM")
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
    commit_scenario(term, result, crate.parent, source)
    diff = result("rust-diff")
    assert "41" in diff and "42" in diff, diff
    term.send("/executors tasks", "assigned to macbook · profile reviewer · model review-model")
    print("PASS  Plan blocked Cargo, Ask handled refusal and approval, and the review followed two real test runs.", flush=True)


def commit_scenario(term, result, workspace, source):
    def git(*args, directory=workspace):
        return subprocess.check_output(["git", "-C", directory, "-c", "user.name=Test",
                                        "-c", "user.email=test@example.org",
                                        "-c", "commit.gpgSign=false", "-c", "core.hooksPath=/dev/null",
                                        *args], text=True).strip()

    # Prepare the objects elsewhere. The running worker starts outside a checkout.
    with tempfile.TemporaryDirectory(dir=workspace.parent) as staging:
        staging = Path(staging)
        for name in ("sample/Cargo.toml", "sample/src/lib.rs"):
            (staging / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(workspace / name, staging / name)
        git("init", "-b", "feature", directory=staging)
        git("add", "sample", directory=staging)
        git("commit", "-m", "passing tests", directory=staging)
        sha = git("rev-parse", "HEAD", directory=staging)
        task = "rust-pin-wait-for-checkout"
        TASKS[task] = dict(TASKS["rust-passing"], test_commit=sha)
        offset = term.send(task, "Submitted " + task + ".")
        term.send("/executors tasks", "no online worker matches")
        (staging / ".git").rename(workspace / ".git")
        term.wait(task + " report received.", offset)
    print("PASS  Pinned task waited until the worker acquired a Git checkout.", flush=True)
    git("switch", "-c", "ongoing")
    git("commit", "--allow-empty", "-m", "unrelated work")
    passing = source.read_text()
    source.write_text("compile_error!(\"local work in progress\");")
    before = git("status", "--porcelain")
    for name, commit in [("rust-pin-target", sha), ("rust-pin-omitted", sha),
                         ("rust-pin-missing", "0" * len(sha))]:
        TASKS[name] = dict(TASKS["rust-passing"], test_commit=commit)
    term.send("/mode ask", "[Ask]")
    report = result("rust-pin-target", b"\x19")
    term.send("/mode yolo", "[Yolo]")
    assert f"commit={sha}" in report.splitlines() and "exit_code=0" in report.splitlines(), report
    assert "duration_ms=" in report and "expected_answer" in report, report
    assert "requires cargo_test with commit" in result("rust-pin-omitted")
    assert "unavailable in the worker repository" in result("rust-pin-missing")
    assert git("branch", "--show-current") == "ongoing" and git("rev-parse", "HEAD") != sha
    assert before == git("status", "--porcelain") and "local work in progress" in source.read_text()
    source.write_text(passing)
    print("PASS  Pinned Cargo used a private checkout, refused a missing pin and reported an unavailable SHA.", flush=True)


def model_server(stack):
    server = stack.enter_context(LoopbackHTTPServer(("127.0.0.1", 0), Model))
    server.started, server.finished, server.ids, server.calls = {}, {}, {}, {}
    server.rust = {}
    server.peer = peers.Consultation()
    stack.callback(server.peer.release.set)
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
        server.workspaces = {name: root.resolve() / name for name in ("linux", "macbook")}
        server.version = next(p["version"] for p in metadata["packages"] if p["name"] == "roomkit-code")

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

        owner_args = args("owner")
        owner_env = {k: v for k, v in env.items() if k != "ROOMKIT_EXECUTOR_REDIS_URL"}
        term = PoolTerminal(stack, owner_args, owner_env)
        term.send(INVENTORY_QUESTION, "No executor inventory tool is available.")
        url = env["ROOMKIT_EXECUTOR_REDIS_URL"].encode()
        save_pool(term, [b"\x1b[C", group.encode(), url], "Scheduling is on.")
        assert url not in term.output
        term.send(INVENTORY_QUESTION, "Live inventory is empty.")
        term.send("bootstrap-task", "Submitted bootstrap-task.")
        term.send("/executors tasks", "no online worker matches")
        worker("linux", "general")
        eventually(lambda: server.finished.get("bootstrap-task") == "linux",
                   "Task submitted to an empty pool did not wake")
        print("PASS  A task submitted before the first worker joined completed.", flush=True)
        worker("macbook", "review")
        term.send(INVENTORY_QUESTION, "Live inventory includes the MacBook and Linux workstation.")
        print("PASS  Coordinator enabled scheduling live and worker loaded its saved profile settings.", flush=True)
        term.send("inspect-pool-from-worker", "Submitted inspect-pool-from-worker.")
        assert server.worker_saw_inventory.wait(15), "Pool worker could not read the inventory"
        print("PASS  Owner and delegated worker can inspect the same live pool as /executors.", flush=True)
        scenario(term, server, lambda: worker("spare", "spare"))
        fresh = PoolTerminal(stack, [*owner_args, "--session", "rust-tests"], owner_env)
        reproduction = "cargo test repro_" + uuid.uuid4().hex
        (root / "linux" / "reproduction.txt").write_text(reproduction)
        peers.exercise(fresh, server, reproduction)
        assert not (root / "macbook" / "peer-output.txt").exists()
        peers.peer_ends(fresh, server)
        peers.check_history(next((root / "owner-state").glob("projects/*/history.sqlite3")), reproduction)
        rust_scenario(fresh, server, root)
        save_pool(fresh, [b"", b"", b"redis://127.0.0.1:1/"],
                  ("Redis did not answer", "Cannot reach the executor pool in Redis"))
        os.write(fresh.master, b"\x1b")
        time.sleep(.3)
        saved = json.loads(next((root / "owner-state").glob("projects/*/executors.json")).read_text())
        assert saved["url"] == url.decode(), "A failed connection overwrote the saved URL"
        fresh.send(INVENTORY_QUESTION, "Live inventory includes the MacBook and Linux workstation.")
        print("PASS  Unreachable broker preserved the working pool and saved URL.", flush=True)
        save_pool(fresh, [b"\x1b[D"], "Scheduling is off.")
        fresh.send(INVENTORY_QUESTION, "No executor inventory tool is available.")
        save_pool(fresh, [b"\x1b[C", b"\x15" + (group + "-other").encode()], "Scheduling is on.")
        fresh.send(INVENTORY_QUESTION, "Live inventory is empty.")
        save_pool(fresh, [b"", b"\x15" + group.encode()])
        fresh.send(INVENTORY_QUESTION, "Live inventory includes the MacBook and Linux workstation.")
        print("PASS  Enable, disable and switch pools in the same open conversation.", flush=True)


if __name__ == "__main__":
    main()
