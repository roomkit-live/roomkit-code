"""Release rejection paths, using disposable Git repositories and fake assets."""

import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import release


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="roomkit-release-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "Cargo.toml").write_text('[package]\nversion = "0.1.1"\n')
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## Unreleased\n\nFuture work.\n\n## [0.1.1]\n\nInitial release.\n\n## [0.1.0]\n\nOld work.\n")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, stderr=subprocess.DEVNULL, text=True).strip()

    def repository(self):
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Release Test")
        self.git("config", "user.email", "test@example.com")
        self.git("add", ".")
        self.git("commit", "-m", "Initial source")
        self.git("tag", "v0.1.1")
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")

    def assets(self):
        directory = self.root / "dist"
        directory.mkdir()
        for target in release.TARGETS:
            name = f"roomkit-code-v0.1.1-{target}"
            data = target.encode()
            (directory / name).write_bytes(data)
            (directory / (name + ".sha256")).write_text(
                f"{hashlib.sha256(data).hexdigest()}  {name}\n")
            (directory / (name + release.SIGNATURE)).write_text("signature fixture\n")
        return directory

    def test_source_and_notes_refer_to_the_exact_stable_version(self):
        self.repository()
        release.verify_source("v0.1.1", self.root)
        self.assertEqual(release.notes("v0.1.1", self.root), "Initial release.\n")
        for tag in ("v0.1.0", "v0.1.1-rc.1", "v01.1.1", "--all"):
            with self.assertRaises(ValueError):
                release.verify_source(tag, self.root)
        (self.root / "CHANGELOG.md").write_text("## Unreleased\nNo version notes.\n")
        with self.assertRaises(ValueError):
            release.notes("v0.1.1", self.root)

    def test_source_refuses_a_different_checkout_or_a_tag_outside_main(self):
        self.repository()
        self.git("commit", "--allow-empty", "-m", "Another commit")
        with self.assertRaises(ValueError):
            release.verify_source("v0.1.1", self.root)
        self.git("tag", "-f", "v0.1.1")
        with self.assertRaises(subprocess.CalledProcessError):
            release.verify_source("v0.1.1", self.root)

    def test_assets_require_every_platform_and_no_unexpected_files(self):
        directory = self.assets()
        release.verify_assets("v0.1.1", directory, self.root)
        unexpected = directory / "private.local.toml"
        unexpected.write_text("fixture")
        with self.assertRaises(ValueError):
            release.verify_assets("v0.1.1", directory, self.root)
        unexpected.unlink()
        next(directory.glob("*.sha256")).unlink()
        with self.assertRaises(ValueError):
            release.verify_assets("v0.1.1", directory, self.root)

    def test_assets_reject_corruption_empty_binaries_and_symlinks(self):
        directory = self.assets()
        binary = next(path for path in directory.iterdir() if path.suffix not in (".sha256", ".minisig"))
        for data in (b"changed", b""):
            binary.write_bytes(data)
            with self.assertRaises(ValueError):
                release.verify_assets("v0.1.1", directory, self.root)
        binary.unlink()
        binary.symlink_to(self.root / "Cargo.toml")
        with self.assertRaises(ValueError):
            release.verify_assets("v0.1.1", directory, self.root)


class SignatureTests(unittest.TestCase):
    """The signing step, with a stand-in for minisign that records what it is asked."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="roomkit-release-sign-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / "dist"
        self.directory.mkdir()
        for target in release.TARGETS:
            (self.directory / f"roomkit-code-v0.1.1-{target}.sha256").write_text("fixture\n")
        self.calls = self.root / "calls"
        self.minisign = self.root / "minisign"
        self.minisign.write_text(f"""#!/usr/bin/env python3
import pathlib, sys
args = sys.argv[1:]
with open({str(self.calls)!r}, "a") as calls:
    calls.write(" ".join(args) + "\\n")
value = lambda flag: args[args.index(flag) + 1]
if "-S" in args:
    key = pathlib.Path(value("-s"))
    assert key.stat().st_mode & 0o077 == 0, "the secret key file must be private"
    pathlib.Path(value("-x")).write_text(key.read_text() + sys.stdin.read())
    sys.exit(0)
sys.exit(0 if pathlib.Path(value("-x")).read_text().startswith("SECRET") else 1)
""")
        self.minisign.chmod(0o755)
        self.key = release.RELEASE_KEY
        self.environment = {name: os.environ.get(name) for name in ("MINISIGN_SECRET_KEY", "MINISIGN_PASSWORD")}
        self.addCleanup(self.restore)

    def restore(self):
        release.RELEASE_KEY = self.key
        for name, value in self.environment.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value

    def public_key(self, text):
        release.RELEASE_KEY = self.root / "release-key.pub"
        release.RELEASE_KEY.write_text(text)

    def test_every_checksum_is_signed_then_verified_with_the_repository_key(self):
        self.public_key("RWQqrgpISR+AeoB/AdjguT4cwFZh8h7MmPQRPCsXrmfj7CfSag4GSA0p\n")
        os.environ["MINISIGN_SECRET_KEY"] = "SECRET"
        os.environ["MINISIGN_PASSWORD"] = "password"
        release.sign("v0.1.1", self.directory, str(self.minisign))
        signatures = sorted(path.name for path in self.directory.glob("*.minisig"))
        self.assertEqual(len(signatures), 4)
        calls = self.calls.read_text()
        self.assertEqual(calls.count("-S "), 4)
        self.assertEqual(calls.count("-V -P RWQqrgpISR+AeoB/AdjguT4cwFZh8h7MmPQRPCsXrmfj7CfSag4GSA0p"), 4)
        self.assertNotIn("SECRET", calls)
        self.assertNotIn("password", calls)

    def test_a_release_is_not_signed_without_a_secret_key_or_a_public_key(self):
        self.public_key("RWQqrgpISR+AeoB/AdjguT4cwFZh8h7MmPQRPCsXrmfj7CfSag4GSA0p\n")
        os.environ.pop("MINISIGN_SECRET_KEY", None)
        with self.assertRaises(ValueError):
            release.sign("v0.1.1", self.directory, str(self.minisign))
        self.public_key("UNSET\n")
        os.environ["MINISIGN_SECRET_KEY"] = "SECRET"
        with self.assertRaises(ValueError):
            release.sign("v0.1.1", self.directory, str(self.minisign))

    def test_a_signature_the_key_did_not_make_is_refused(self):
        self.public_key("RWQqrgpISR+AeoB/AdjguT4cwFZh8h7MmPQRPCsXrmfj7CfSag4GSA0p\n")
        for checksum in self.directory.glob("*.sha256"):
            checksum.with_name(checksum.name + ".minisig").write_text("forged\n")
        with self.assertRaises(ValueError):
            release.verify_signatures("v0.1.1", self.directory, str(self.minisign))

    def test_install_sh_holds_the_repository_key(self):
        installer = (Path(__file__).resolve().parent.parent / "install.sh").read_text()
        self.assertIn(f"release_key='{self.key.read_text().strip()}'", installer)

    def test_the_manual_check_in_security_md_names_the_repository_key(self):
        security = (Path(__file__).resolve().parent.parent / "SECURITY.md").read_text()
        self.assertIn(f"-P {self.key.read_text().strip()}\n", security)


class DependencyAccessTests(unittest.TestCase):
    def test_anonymous_and_optional_private_access_are_isolated(self):
        script = Path(__file__).resolve().parent / "ci-dependency-access.sh"
        with tempfile.TemporaryDirectory(prefix="roomkit-ci-access-test-") as temporary:
            root = Path(temporary)
            config, exports = root / "gitconfig", root / "environment"
            env = dict(os.environ, RUNNER_TEMP=str(root), GITHUB_ENV=str(exports),
                       GIT_CONFIG_GLOBAL=str(config), ROOMKIT_DEPS_READ_CI="")
            anonymous = subprocess.run(["bash", str(script)], env=env, check=True, capture_output=True)
            self.assertIn(b"anonymous", anonymous.stdout)
            self.assertFalse(config.exists())
            private = subprocess.run(["bash", str(script)],
                env=dict(env, ROOMKIT_DEPS_READ_CI="test-only-fixture"), check=True, capture_output=True)
            self.assertNotIn(b"test-only-fixture", private.stdout + private.stderr)
            credentials = root / "roomkit-code-dependency-access" / "credentials"
            self.assertEqual(credentials.stat().st_mode & 0o777, 0o600)
            self.assertIn("CARGO_NET_GIT_FETCH_WITH_CLI=true", exports.read_text())


if __name__ == "__main__":
    unittest.main()
