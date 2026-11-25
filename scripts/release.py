"""Validate tagged source, release notes and the complete native asset set, and sign it."""

import argparse
import hashlib
import os
import re
import subprocess
import tempfile
from pathlib import Path
import tomllib

# The public half of the key that signs every release's checksum files. The
# updater embeds this file and install.sh holds a copy of its line.
RELEASE_KEY = Path(__file__).resolve().parent.parent / "src" / "updates" / "release-key.pub"
SIGNATURE = ".sha256.minisig"

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
                for target in TARGETS for suffix in ("", ".sha256", SIGNATURE)}
    if {path.name for path in directory.iterdir()} != expected:
        raise ValueError("A release needs exactly four binaries, each with its checksum and its signature")
    for target in TARGETS:
        name = f"roomkit-code-{tag}-{target}"
        binary, checksum = directory / name, directory / (name + ".sha256")
        signature = directory / (name + SIGNATURE)
        if any(path.is_symlink() or not path.is_file() for path in (binary, checksum, signature)):
            raise ValueError("Release assets must be regular files")
        if not 0 < binary.stat().st_size <= 256 * 1024 * 1024 or checksum.stat().st_size > 1024:
            raise ValueError("Invalid release asset size")
        if not 0 < signature.stat().st_size <= 4096:
            raise ValueError("Invalid release signature size")
        digest = hashlib.sha256(binary.read_bytes()).hexdigest()
        if checksum.read_text() != f"{digest}  {name}\n":
            raise ValueError(f"Invalid checksum for {name}")


def release_key():
    """The public key releases are signed for, as minisign's -P takes it."""
    key = RELEASE_KEY.read_text().strip()
    if not re.fullmatch(r"[A-Za-z0-9+/]{56}", key):
        raise ValueError(f"{RELEASE_KEY.name} must hold the release signing key's public half")
    return key


def checksums(tag, directory):
    return [directory / f"roomkit-code-{tag}-{target}.sha256" for target in TARGETS]


def sign(tag, directory, minisign="minisign"):
    """Sign each checksum file with the secret key the environment holds.

    MINISIGN_SECRET_KEY is the secret key file's content and MINISIGN_PASSWORD
    its password, empty for a key made without one. The key is written to a
    private temporary file for the time of the signatures and never printed.
    """
    secret = os.environ.get("MINISIGN_SECRET_KEY", "")
    if not secret.strip():
        raise ValueError("MINISIGN_SECRET_KEY is not set: a release is not published unsigned")
    password = os.environ.get("MINISIGN_PASSWORD", "") + "\n"
    with tempfile.TemporaryDirectory(prefix="roomkit-release-key-") as private:
        key = Path(private) / "minisign.key"
        key.touch(mode=0o600)
        key.write_text(secret if secret.endswith("\n") else secret + "\n")
        for checksum in checksums(tag, directory):
            signature = checksum.with_name(checksum.name + ".minisig")
            subprocess.run(
                [minisign, "-S", "-s", str(key), "-m", str(checksum), "-x", str(signature),
                 "-t", f"RoomKit Code {tag} {checksum.name}"],
                input=password, text=True, check=True, stdout=subprocess.DEVNULL)
    verify_signatures(tag, directory, minisign)


def verify_signatures(tag, directory, minisign="minisign"):
    """Check each checksum file's signature against the repository's public key."""
    key = release_key()
    for checksum in checksums(tag, directory):
        signature = checksum.with_name(checksum.name + ".minisig")
        result = subprocess.run(
            [minisign, "-V", "-P", key, "-m", str(checksum), "-x", str(signature)],
            capture_output=True, text=True)
        if result.returncode != 0:
            raise ValueError(f"{signature.name} is not the release key's signature of {checksum.name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("source", "assets", "notes", "sign", "signatures"))
    parser.add_argument("tag")
    parser.add_argument("--directory", type=Path, default=Path("dist"))
    args = parser.parse_args()
    if args.action == "source":
        verify_source(args.tag)
    elif args.action == "assets":
        verify_assets(args.tag, args.directory)
    elif args.action == "sign":
        sign(args.tag, args.directory)
    elif args.action == "signatures":
        verify_signatures(args.tag, args.directory)
    else:
        print(notes(args.tag), end="")


if __name__ == "__main__":
    main()
