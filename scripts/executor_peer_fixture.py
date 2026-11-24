"""An isolated workspace on this host or over a loopback-only SSH forward."""
from pathlib import Path
import shlex
import subprocess


class Peer:
    def __init__(self, stack, root, host, name="review"):
        self.host = host
        if host:
            self.conn = ["ssh", "-o", "BatchMode=yes", "-S", str(root / "ssh")]
            subprocess.run([*self.conn, "-MNf", host], check=True)
            stack.callback(subprocess.run, [*self.conn, "-O", "exit", host], capture_output=True)
            self.work = Path(self.run(["mktemp", "-d", "/tmp/roomkit-peer-test.XXXXXXXX"]).strip())
            stack.callback(self.run, ["rm", "-rf", str(self.work)])
        else:
            self.work = root / name
        self.run(["mkdir", "-p", str(self.work / "workspace")])

    def command(self, args, tty=False):
        return [*self.conn, *(["-tt"] if tty else []), self.host, shlex.join(args)] if self.host else args

    def run(self, args, data=None):
        return subprocess.check_output(self.command(args), text=True, input=data)

    def port(self, local):
        if not self.host:
            return local
        forwarding = f"127.0.0.1:0:127.0.0.1:{local}"
        return int(subprocess.check_output([*self.conn, "-O", "forward", "-R", forwarding, self.host], text=True))
