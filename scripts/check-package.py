"""Check actual package contents and immutable dependency sources."""

import json
from pathlib import PurePosixPath
import subprocess
import tomllib

allowed = {"build.rs", "Cargo.toml", "Cargo.toml.orig", "Cargo.lock", "README.md", "USER_GUIDE.md", "LICENSE", ".cargo_vcs_info.json"}
result = subprocess.run(
    ["cargo", "package", "--locked", "--list", "--allow-dirty"],
    capture_output=True, text=True, check=True,
)
names = result.stdout.splitlines()
for name in names:
    if name not in allowed and PurePosixPath(name).parts[0] != "src":
        raise SystemExit(f"Unexpected package content: {name}")
assert {"LICENSE", "USER_GUIDE.md", "src/main.rs", "src/agent/coding-agent.txt"}.issubset(names)
with open("Cargo.toml", "rb") as manifest:
    for dependency in tomllib.load(manifest)["dependencies"].values():
        if isinstance(dependency, dict) and "git" in dependency:
            revision = dependency.get("rev", "")
            assert len(revision) == 40 and all(c in "0123456789abcdef" for c in revision)
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"]))
engines = [p for p in metadata["packages"] if p["name"] == "roomkit"]
assert len(engines) == 1, "Application and adapters must share one engine instance/type source"
workspace_members = set(metadata["workspace_members"])
assert all(p["source"] is not None or p["id"] in workspace_members for p in metadata["packages"]), "Sibling path dependency found"
result = subprocess.run(
    ["cargo", "package", "--locked", "--list", "--allow-dirty", "-p", "roomkit-hooks"],
    capture_output=True, text=True, check=True,
)
hook_names = result.stdout.splitlines()
for name in hook_names:
    assert name in allowed or PurePosixPath(name).parts[0] == "src", f"Unexpected hooks package content: {name}"
assert {"LICENSE", "README.md", "src/lib.rs"}.issubset(hook_names)
print(f"{len(names)} permitted package files; pinned Git dependencies; one engine source.")
