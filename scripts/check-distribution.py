"""Exercise the real installer with local fake GitHub assets and a managed PTY.

No network, real credentials, user's installation or application data are used.
The input binary must be built with ROOMKIT_RELEASE_BUILD=1.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time

if len(sys.argv) > 1:
    binary = Path(sys.argv[1]).resolve()
else:
    metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1']))
    binary = Path(metadata['target_directory']) / 'debug' / 'roomkit-code'
version = subprocess.check_output([str(binary), '--version'], text=True).strip().split()[-1]
with tempfile.TemporaryDirectory(prefix='roomkit-distribution-') as directory:
    root = Path(directory)
    fixture_binary = root / 'release-binary'
    shutil.copy2(binary, fixture_binary)
    subprocess.run(['strip', '-S', str(fixture_binary)], check=True)
    if sys.platform == 'darwin': subprocess.run(['codesign', '--force', '--sign', '-', str(fixture_binary)], check=True)
    home, bin_dir, fake, project = (root / name for name in ['home', 'bin', 'fake', 'project'])
    for path in [home, fake, project]: path.mkdir()
    gh = fake / 'gh'
    gh.write_text('''#!/usr/bin/env python3
import hashlib, os, pathlib, shutil, sys
args=sys.argv[1:]
if args[:2] == ['auth','status']: sys.exit(0)
if args[:1] == ['api']: print('v'+os.environ['FIXTURE_VERSION']); sys.exit(0)
if args[:2] == ['release','download']:
    out=pathlib.Path(args[args.index('--dir')+1])
    names=[args[i+1] for i, v in enumerate(args) if v=='--pattern']
    name=next(n for n in names if not n.endswith('.sha256'))
    source=pathlib.Path(os.environ['FIXTURE_BINARY'])
    shutil.copyfile(source, out/name)
    digest=hashlib.sha256(source.read_bytes()).hexdigest()
    if os.environ.get('FIXTURE_CORRUPT'): digest='0'*64
    (out/(name+'.sha256')).write_text(f'{digest}  {name}\\n')
    sys.exit(0)
sys.exit(1)
''')
    gh.chmod(0o755)
    env = {k:v for k,v in os.environ.items() if not k.startswith(('ROOMKIT_', 'GH_', 'GITHUB_', 'HERDR_'))}
    env.update(HOME=str(home), XDG_DATA_HOME=str(home/'data'), PATH=str(fake)+os.pathsep+os.environ['PATH'],
               ROOMKIT_CODE_BIN_DIR=str(bin_dir), FIXTURE_BINARY=str(fixture_binary), FIXTURE_VERSION=version,
               TERM='xterm-256color', ROOMKIT_NO_UPDATE_CHECK='true')
    subprocess.run(['sh', 'install.sh', version], env=env, check=True)
    entry = bin_dir / 'roomkit-code'
    assert entry.is_symlink() and entry.resolve().is_file()
    assert subprocess.check_output([str(entry), '--version'], env=env, text=True).strip() == f'roomkit-code {version}'
    first = entry.resolve()
    subprocess.run(['sh', 'install.sh', version], env=env, check=True)
    assert entry.resolve() == first, 'same-version reinstall changed activation'
    bad = subprocess.run(['sh', 'install.sh', version], env=dict(env, FIXTURE_CORRUPT='1'), capture_output=True)
    assert bad.returncode != 0 and b'Checksum verification failed' in bad.stderr
    assert entry.resolve() == first
    external = root / 'external'; external.mkdir(); (external/'roomkit-code').write_text('external binary')
    bad = subprocess.run([str(fixture_binary), '--install-dir', str(external)], env=env, capture_output=True)
    assert bad.returncode != 0 and (external/'roomkit-code').read_text() == 'external binary'
    assert not (home/'data').exists(), 'installer must not load providers or create sessions'

    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 140, 0, 0))
    original = termios.tcgetattr(slave)
    process = subprocess.Popen([str(entry), str(project), '--no-update-check', '--no-herdr', '--no-terminal-status', '--color', 'never'],
                               stdin=slave, stdout=slave, stderr=slave, env=env)
    output = bytearray()
    def wait(text, after=0):
        deadline=time.monotonic()+20
        while text.encode() not in re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', output[after:]):
            if time.monotonic()>deadline or process.poll() is not None: raise AssertionError(f'Missing {text}: {output[-5000:]!r}')
            if select.select([master],[],[],0.05)[0]: output.extend(os.read(master,65536))
    try:
        wait(f'v{version}')
        offset=len(output); os.write(master,b'/update rollback\r')
        wait('Could not complete this operation',offset)
        os.write(master,b'\x1b'); time.sleep(0.15)
        os.write(master,b'\x04')
        deadline=time.monotonic()+10
        while process.poll() is None and time.monotonic()<deadline:
            if select.select([master],[],[],0.05)[0]: output.extend(os.read(master,65536))
        assert process.poll()==0
        assert termios.tcgetattr(slave)==original
        if path := os.environ.get('ROOMKIT_DISTRIBUTION_CAPTURE'): Path(path).write_bytes(output)
    finally:
        if process.poll() is None: process.kill(); process.wait()
        os.close(master); os.close(slave)
print('Distribution checks passed: verified install, repeat install, corruption, external binary protection, welcome version, rollback error and terminal restoration.')
