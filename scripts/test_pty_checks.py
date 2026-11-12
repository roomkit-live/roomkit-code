"""Exercise terminal shutdown with real child processes and finite PTY buffers."""

import os
import pty
import subprocess
import sys
import time
import unittest

from pty_checks import wait_for_exit


class PtyWaitTests(unittest.TestCase):
    def start(self, source, *, keep_slave=True):
        master, slave = pty.openpty()
        self.addCleanup(os.close, master)
        if keep_slave:
            # Match application fixtures: retain the slave until all output is
            # collected. macOS flushes unread output when its last slave closes.
            self.addCleanup(os.close, slave)
        try:
            process = subprocess.Popen(
                [sys.executable, "-c", source], stdin=subprocess.DEVNULL,
                stdout=slave, stderr=slave,
            )
        finally:
            if not keep_slave:
                os.close(slave)
        self.addCleanup(self.stop, process)
        return process, master

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)

    def test_large_output_is_drained_and_failure_status_is_preserved(self):
        process, master = self.start(
            "import sys; sys.stdout.buffer.write(b'x' * 262144 + b'end'); sys.exit(7)"
        )
        output = bytearray(b"previous")
        self.assertEqual(wait_for_exit(process, master, output), 7)
        self.assertEqual(output, b"previous" + b"x" * 262144 + b"end")

    def test_output_is_collected_after_the_child_has_exited(self):
        process, master = self.start("import sys; sys.stdout.write('restored')")
        process.wait(timeout=5)
        output = bytearray()
        self.assertEqual(wait_for_exit(process, master, output), 0)
        self.assertEqual(output, b"restored")

    def test_closed_terminal_still_waits_for_the_child(self):
        process, master = self.start(
            "import os, time; os.close(1); os.close(2); time.sleep(0.1); os._exit(9)",
            keep_slave=False,
        )
        self.assertEqual(wait_for_exit(process, master, bytearray()), 9)

    def test_a_closed_terminal_does_not_hide_a_hung_child(self):
        process, master = self.start(
            "import os, time; os.close(1); os.close(2); time.sleep(30)",
            keep_slave=False,
        )
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            wait_for_exit(process, master, bytearray(), timeout=0.2)
        self.assertLess(time.monotonic() - started, 5)
        self.assertIsNone(process.poll())

    def test_continuous_output_does_not_extend_the_deadline(self):
        process, master = self.start("import os\nwhile True: os.write(1, b'x' * 65536)")
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            wait_for_exit(process, master, bytearray(), timeout=0.2)
        self.assertLess(time.monotonic() - started, 5)
        self.assertIsNone(process.poll())


if __name__ == "__main__":
    unittest.main()
