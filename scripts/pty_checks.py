"""Shared process waits for native terminal integration checks."""

import errno
import os
import select
import subprocess
import time


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
