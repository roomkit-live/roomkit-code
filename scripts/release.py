"""Validate tagged source, release notes and the complete native asset set."""

import argparse
import hashlib
import re
import subprocess
from pathlib import Path
import tomllib

TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
)


def version(tag, root=Path(".")):
    if not re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag):
        raise ValueError("Expected a stable tag such as v0.1.1")
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    if tag != "v" + manifest["package"]["version"]:
        raise ValueError("Release tag must match Cargo.toml")
    return tag[1:]


def notes(tag, root=Path(".")):
    number = version(tag, root)
    changelog = (root / "CHANGELOG.md").read_text()
    match = re.search(rf"^## \[{re.escape(number)}\][^\n]*\n(.*?)(?=^## |\Z)",
                      changelog, re.MULTILINE | re.DOTALL)
    if not match or not match[1].strip():
        raise ValueError("The tagged version needs release notes in CHANGELOG.md")
    return match[1].strip() + "\n"


def verify_source(tag, root=Path(".")):
    notes(tag, root)

    def git(*args):
        return subprocess.check_output(["git", *args], cwd=root, text=True).strip()

    head = git("rev-parse", "HEAD")
    if head != git("rev-parse", f"refs/tags/{tag}^{{commit}}"):
        raise ValueError("The checkout must be the tagged commit")
    subprocess.run(["git", "merge-base", "--is-ancestor", head,
                    "refs/remotes/origin/main"], cwd=root, check=True)


def verify_assets(tag, directory, root=Path(".")):
    version(tag, root)
    expected = {f"roomkit-code-{tag}-{target}{suffix}"
                for target in TARGETS for suffix in ("", ".sha256")}
    if {path.name for path in directory.iterdir()} != expected:
        raise ValueError("A release needs exactly four binary/checksum pairs")
    for target in TARGETS:
        name = f"roomkit-code-{tag}-{target}"
        binary, checksum = directory / name, directory / (name + ".sha256")
        if any(path.is_symlink() or not path.is_file() for path in (binary, checksum)):
            raise ValueError("Release assets must be regular files")
        if not 0 < binary.stat().st_size <= 256 * 1024 * 1024 or checksum.stat().st_size > 1024:
            raise ValueError("Invalid release asset size")
        digest = hashlib.sha256(binary.read_bytes()).hexdigest()
        if checksum.read_text() != f"{digest}  {name}\n":
            raise ValueError(f"Invalid checksum for {name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("source", "assets", "notes"))
    parser.add_argument("tag")
    parser.add_argument("--directory", type=Path, default=Path("dist"))
    args = parser.parse_args()
    if args.action == "source":
        verify_source(args.tag)
    elif args.action == "assets":
        verify_assets(args.tag, args.directory)
    else:
        print(notes(args.tag), end="")


if __name__ == "__main__":
    main()
