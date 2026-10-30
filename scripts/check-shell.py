"""Exercise explicit composer commands without any provider or agent configured."""
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
import time

metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True
))
binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"


def scenario(project, state, resume=False):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 120, 0, 0))
    original = termios.tcgetattr(slave)
    env = {k: v for k, v in os.environ.items() if not k.startswith("ROOMKIT_")}
    process = subprocess.Popen([
        str(binary), str(project), "--state-dir", str(state), "--session", "shell-fixture",
        "--color", "never", "--no-animations", "--no-logo",
    ], stdin=slave, stdout=slave, stderr=slave, env=env)
    captured = bytearray()

    def wait_for(marker, after=0):
        deadline = time.monotonic() + 15
        while marker not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", captured[after:]):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise RuntimeError(f"Missing {marker!r}; {bytes(captured[-4000:])!r}")
            if select.select([master], [], [], 0.1)[0]:
                captured.extend(os.read(master, 65536))

    try:
        wait_for(b"RoomKit Code")
        if resume:
            wait_for(b"User shell command")
            wait_for(b"Interrupted")
            assert not (project / "late.txt").exists()
        else:
            os.write(master, b"!")
            wait_for(b"Run a shell command in this project")
            # Bracketed paste must preserve the multiline command and quoted argument.
            os.write(master, b"\x1b[200~printf 'quoted %s\\n' 'two words'\nprintf 'err\\n' >&2\nexit 7\n\x1b[201~")
            offset = len(captured)
            os.write(master, b"\r")
            wait_for(b"exit 7", offset)
            wait_for(b"Execution Failed", offset)
            wait_for(b"quoted two words", offset)
            offset = len(captured)
            os.write(master, b"!printf 'started\\n'; sleep 30; printf bad > late.txt\r")
            wait_for(b"Esc interrupt", offset)
            os.write(master, b"\x1b[17~")  # F6
            wait_for(b"Background processes", offset)
            offset = len(captured)
            os.write(master, b"\x1b")
            wait_for(b"Esc interrupt", offset)
            offset = len(captured)
            os.write(master, b"\x1b")
            wait_for(b"Execution Interrupted", offset)
            assert not (project / "late.txt").exists()
        os.write(master, b"\x04")
        assert process.wait(timeout=10) == 0, bytes(captured[-4000:])
        while select.select([master], [], [], 0)[0]:
            captured.extend(os.read(master, 65536))
        assert termios.tcgetattr(slave) == original
        assert b"\x1b[?1049l" in captured
        assert b"\x1b[?2004l" in captured
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)


with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    project, state = root / "project", root / "state"
    project.mkdir()
    scenario(project, state)
    scenario(project, state, resume=True)
print("Shell PTY checks passed: composer prefix, multiline paste, quoted arguments, stdout/stderr, nonzero exit, F6, Esc, restart, terminal restoration and no configured provider.")
