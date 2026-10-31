"""Two real local instances share a project, with exclusive conversation ownership."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import sqlite3
import struct
import subprocess
import tempfile
import termios
import time

metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True
))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"


class App:
    def __init__(self, project, state, session=None):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 35, 150, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        args = [str(binary), str(project), "--state-dir", str(state), "--mode", "yolo",
                "--no-update-check", "--no-herdr", "--no-terminal-status", "--color", "never"]
        if session:
            args.extend(["--session", session])
        env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
        env["TERM"] = "xterm-256color"
        self.process = subprocess.Popen(args, stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env)
        self.output = bytearray()

    def send(self, text):
        offset = len(self.output)
        os.write(self.master, text.encode())
        return offset

    def wait(self, text, offset=0):
        deadline = time.monotonic() + 20
        while text.encode() not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", self.output[offset:]):
            if time.monotonic() > deadline or self.process.poll() is not None:
                raise AssertionError(f"Missing {text}: {self.output[-5000:]!r}")
            if select.select([self.master], [], [], 0.05)[0]:
                self.output.extend(os.read(self.master, 65536))

    def shell(self, marker):
        offset = self.send(f"!printf '{marker}'\r")
        self.wait("Exited", offset)
        self.wait("Execution Completed", offset)
        # RunFinished can precede the adapter's final persistence/cleanup boundary.
        # Drain the terminal until it is quiet before sending another control.
        deadline = time.monotonic() + 5
        while select.select([self.master], [], [], 0.3)[0]:
            self.output.extend(os.read(self.master, 65536))
            if time.monotonic() > deadline:
                raise AssertionError("Shell operation did not settle")

    def finish(self):
        self.send("\x04")
        self.process.wait(timeout=10)
        assert self.process.returncode == 0
        assert termios.tcgetattr(self.slave) == self.original

    def cleanup(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        os.close(self.master)
        os.close(self.slave)


with tempfile.TemporaryDirectory(prefix="roomkit-multi-session-") as directory:
    root = Path(directory)
    project, state = root / "project", root / "state"
    project.mkdir()
    apps = []
    try:
        first = App(project, state)
        apps.append(first)
        first.wait("Ask about this project")
        database = next(state.glob("projects/*/history.sqlite3"))

        def rooms():
            with sqlite3.connect(database) as connection:
                return {row[0] for row in connection.execute("SELECT id FROM rooms")}

        first_room, = rooms()
        second = App(project, state)
        apps.append(second)
        second.wait("Ask about this project")
        second_room, = rooms() - {first_room}
        assert first_room != second_room, "Another process claimed the occupied empty draft"
        first.shell("FIRST_SESSION_MARKER")
        second.shell("SECOND_SESSION_MARKER")
        # The picker identifies occupied sessions; attempting to open one keeps this view.
        offset = second.send("/resume\r")
        second.wait("Open in another instance", offset)
        second.send("\x1b")
        time.sleep(0.1)
        offset = second.send(f"/resume {first_room}\r")
        second.wait("This conversation is open in another", offset)
        second.shell("SECOND_STILL_USABLE")
        assert first.process.poll() is None
        first.finish()
        offset = second.send(f"/resume {first_room}\r")
        second.wait("FIRST_SESSION_MARKER", offset)
        second.shell("FIRST_SESSION_RESUMED")
        second.finish()
        # No cleanup of .lock files is needed after process death.
        third = App(project, state, first_room)
        apps.append(third)
        third.wait("FIRST_SESSION_RESUMED")
        third.process.kill()
        third.process.wait(timeout=5)
        fourth = App(project, state, first_room)
        apps.append(fourth)
        fourth.wait("FIRST_SESSION_RESUMED")
        fourth.finish()
        assert len(rooms()) == 2, "Unexpected conversations accumulated during resume"
        # Both processes append valid records to the bounded shared diagnostic log.
        records = [json.loads(line) for line in database.with_name("diagnostics.jsonl").read_text().splitlines()]
        assert len({r["instance"] for r in records}) >= 4
    finally:
        for app in reversed(apps):
            app.cleanup()

print("Multi-session PTY passed: independent drafts, simultaneous instances, busy picker/resume, continued execution, graceful handoff and recovery after process death.")
