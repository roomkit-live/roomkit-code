"""Package one already tested native binary, never local configuration or sources."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib

version = tomllib.loads(Path('Cargo.toml').read_text())['package']['version']
tag = os.environ['RELEASE_TAG']
assert tag == 'v' + version, 'Release tag must match Cargo.toml'
target = sys.argv[1]
assert target in {'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-apple-darwin', 'aarch64-apple-darwin'}
binary = Path(os.environ.get('CARGO_TARGET_DIR', 'target')) / target / 'release' / 'roomkit-code'
assert subprocess.check_output([str(binary), '--version'], text=True).strip() == f'roomkit-code {version}'
out = Path('dist')
out.mkdir(exist_ok=True)
name = f'roomkit-code-{tag}-{target}'
shutil.copy2(binary, out / name)
digest = hashlib.file_digest((out / name).open('rb'), 'sha256').hexdigest()
(out / (name + '.sha256')).write_text(f'{digest}  {name}\n')
print(f'Packaged {name} ({(out / name).stat().st_size} bytes)')
