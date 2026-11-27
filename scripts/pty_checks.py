"""Process waits and screen reconstruction for native terminal integration checks."""

import errno
import os
import re
import select
import subprocess
import time
import unicodedata


def model_fixture_env():
    """Keep a scripted model's request sequence independent of background title generation."""
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
    env["ROOMKIT_NO_AUTO_TITLE"] = "true"
    return env


def wait_for_exit(process, master, output, timeout=10):
    """Drain terminal output until exit without extending the shutdown deadline.

    A frame can exceed the PTY buffer before the application reads its exit
    key. Waiting without reading would block both the renderer and this check.
    Keep trailing output for terminal restoration and diagnostic assertions.
    """
    deadline = time.monotonic() + timeout
    while True:
        code = process.poll()
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(process.args, timeout, output=bytes(output))
        ready = select.select([master], [], [], 0 if code is not None else min(0.05, remaining))[0]
        if not ready:
            if code is not None:
                return code
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            chunk = b""  # Linux reports a closed PTY slave as EIO.
        if not chunk:
            return process.wait(timeout=max(0, deadline - time.monotonic()))
        output.extend(chunk)


OSC = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")
CSI = re.compile(r"\x1b\[([0-9;?<]*)([A-Za-z])")


def screen_text(captured: bytes | bytearray, rows: int, cols: int) -> str:
    """Reconstruct the visible grid from incremental terminal updates."""
    cells = [[" "] * cols for _ in range(rows)]
    row = col = 0
    data = captured.decode(errors="replace")
    index = 0
    while index < len(data):
        # Match in place. Slicing the remaining transcript at every
        # character makes screen reconstruction quadratic.
        osc = OSC.match(data, index)
        if osc:
            index = osc.end()
            continue
        csi = CSI.match(data, index)
        if csi:
            args, command = csi.groups()
            if command == "H":
                position = [int(x) for x in args.split(";") if x.isdigit()] or [1, 1]
                row, col = position[0] - 1, (position[1] - 1 if len(position) > 1 else 0)
            elif command == "J" and args == "2":
                cells = [[" "] * cols for _ in range(rows)]
            elif command == "K" and args == "2" and 0 <= row < rows:
                cells[row] = [" "] * cols
            index = csi.end()
            continue
        char = data[index]
        index += 1
        if char == "\x1b" or char in "\r\n" or not (0 <= row < rows and 0 <= col < cols):
            continue
        cells[row][col] = char
        col += 2 if unicodedata.east_asian_width(char) in "WF" else 1
    return "\n".join("".join(line) for line in cells)
