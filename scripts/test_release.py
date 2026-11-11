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
        binary = next(path for path in directory.iterdir() if path.suffix != ".sha256")
        for data in (b"changed", b""):
            binary.write_bytes(data)
            with self.assertRaises(ValueError):
                release.verify_assets("v0.1.1", directory, self.root)
        binary.unlink()
        binary.symlink_to(self.root / "Cargo.toml")
        with self.assertRaises(ValueError):
            release.verify_assets("v0.1.1", directory, self.root)


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
