"""Automatic conversation titles on the real terminal, with a delayed model response."""
from contextlib import ExitStack
from http.server import BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import threading
import time

from executor_fixture import Terminal, redis
from fixture_http import LoopbackHTTPServer
from pty_checks import model_fixture_env, screen_text


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if body.get("stream"):
            self.server.turns += 1
            reply = f"Reply delivered for turn {self.server.turns}."
            frames = [
                {"choices": [{"index": 0, "delta": {"content": reply}}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
            data = "".join(f"data: {json.dumps(f)}\n\n" for f in frames) + "data: [DONE]\n\n"
            self.respond("text/event-stream", data)
            return
        assert not body.get("tools") and body["model"] == "title-fixture"
        user, = [m["content"] for m in body["messages"] if m["role"] == "user"]
        self.server.prompts.append(user)
        if "manual" in user:
            self.server.generating.set()
            self.server.release.wait(10)
        truncated = "truncated" in user
        content = "" if truncated else json.dumps({"title": "Budget du modèle"})
        message = {"role": "assistant", "content": content}
        self.respond("application/json", json.dumps({"choices": [{"index": 0, "message": message,
                                                                 "finish_reason": "length" if truncated else "stop"}]}))

    def respond(self, kind, text):
        data = text.encode()
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def wait_screen(term, expected, *, header=False):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        screen = screen_text(term.output, 58, 200)
        visible = "\n".join(screen.splitlines()[:3]) if header else screen
        if expected in visible:
            return
        if select.select([term.master], [], [], .05)[0]:
            term.output.extend(os.read(term.master, 65536))
    raise AssertionError(f"Missing {expected!r}: {[line.strip() for line in screen.splitlines() if line.strip()]}")


def turn(term, message, server):
    # An unchanged status line is not repainted between two fast turns.
    term.send(message, f"Reply delivered for turn {server.turns + 1}.")
    wait_screen(term, "Execution Completed")


def network(stack, args, env, root, server):
    group = ["--session-group", "title-test"]
    env = {**env, "ROOMKIT_EXECUTOR_REDIS_URL": stack.enter_context(redis()),
           "ROOMKIT_SESSION_GROUP_KEY": "test-title-key-for-independent-rooms"}
    review = Terminal(stack, [*args, "--session", "review", *group], env)
    turn(review, "Pourquoi le modèle dépasse son budget ?", server)
    wait_screen(review, "Budget du modèle", header=True)

    def picker(key):
        other = root / key
        other.mkdir()
        peer_args = [*args]
        peer_args[1] = str(other)
        peer_args[peer_args.index("--state-dir") + 1] = str(other / "state")
        return Terminal(stack, [*peer_args, "--session", key, *group],
                        env if key != "mismatch" else {**env, "ROOMKIT_SESSION_GROUP_KEY": "different-test-key-for-independent-rooms"})

    peer = picker("api")
    os.write(peer.master, b"@")
    wait_screen(peer, "Budget du modèle")
    wait_screen(peer, "Network session")
    other = picker("mismatch")
    os.write(other.master, b"@")
    wait_screen(other, "Network session · review")
    assert "Budget du modèle" not in screen_text(other.output, 58, 200)
    review.send("/rename Review findings", "Review findings")
    # A fresh lookup sees the same original mailbox under its updated title.
    peer2 = picker("observer")
    os.write(peer2.master, b"@")
    wait_screen(peer2, "Review findings")
    messenger = picker("sender")
    received = len(review.output)
    messenger.send("@review Check the renamed mailbox", "Message to @review")
    review.wait("Check the renamed mailbox", received)
    assert len(server.prompts) == 1
    print("Redis titles passed: independent databases, automatic and manual names, key mismatch fallback.")


def run():
    cargo = subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"])
    binary = Path(json.loads(cargo)["target_directory"]) / "debug/roomkit-code"
    with tempfile.TemporaryDirectory() as tmp, ExitStack() as stack:
        root = Path(tmp)
        server = stack.enter_context(LoopbackHTTPServer(("127.0.0.1", 0), Model))
        server.prompts = []
        server.turns = 0
        server.generating, server.release = threading.Event(), threading.Event()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.shutdown)
        project = root / "project"
        project.mkdir()
        args = [str(binary), str(project), "--state-dir", str(root / "state"), "--model", "title-fixture",
                "--base-url", f"http://127.0.0.1:{server.server_port}/v1", "--mode", "plan",
                "--no-animations", "--no-terminal-status", "--no-herdr", "--no-update-check"]
        env = {**model_fixture_env(), "ROOMKIT_NO_AUTO_TITLE": "false", "ROOMKIT_API_KEY": "fixture", "TERM": "xterm-256color"}
        if sys.argv[1:] == ["--network"]:
            network(stack, args, env, root, server)
            return
        with ExitStack() as owner:
            a = Terminal(owner, args, env)
            wait_screen(a, "Start a conversation")
            turn(a, "salut", server)
            wait_screen(a, "New session", header=True)
            turn(a, "Pourquoi le modèle dépasse son budget ?", server)
            wait_screen(a, "Budget du modèle", header=True)
            with ExitStack() as observer:
                peer = Terminal(observer, [*args, "--session", "api"], env)
                os.write(peer.master, b"@")
                wait_screen(peer, "Budget du modèle")
            turn(a, "Explore another topic", server)
            assert len(server.prompts) == 1 and "budget" in server.prompts[0]
        peer = Terminal(stack, [*args, "--session", "reader"], env)
        peer.send("/resume", "Budget du modèle")
        os.write(peer.master, b"\r")
        wait_screen(peer, "Budget du modèle", header=True)
        peer.send("/new", "Start a conversation")
        turn(peer, "Exercise a manual rename", server)
        assert server.generating.wait(10)
        os.write(peer.master, b"/rename My title\r")
        wait_screen(peer, "My title", header=True)
        server.release.set()
        turn(peer, "Check the stored title", server)
        wait_screen(peer, "My title", header=True)
        assert len(server.prompts) == 2
        peer.send("/new", "Start a conversation")
        turn(peer, "Exercise a truncated title", server)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if any("Title model reached its output limit" in path.read_text()
                   for path in (root / "state").rglob("diagnostics.jsonl")):
                break
            time.sleep(.05)
        else:
            raise AssertionError("Missing title failure reason in diagnostics")
        turn(peer, "Discuss the model budget", server)
        wait_screen(peer, "Budget du modèle", header=True)
        assert len(server.prompts) == 4
    print("Session titles passed: greeting, language, live header, @, resume, stable name and manual override.")


if __name__ == "__main__":
    run()
