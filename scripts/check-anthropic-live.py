"""Opt-in paid Anthropic check: build first, then supply ANTHROPIC_API_KEY.

Run from the repository root. The model reads a disposable fixture, writes a
file, then resumes the saved session in a separate process. No real workspace
is sent, and the temporary profile and its stored credential are removed.
This check is deliberately outside `make check` and CI.
"""

import json
import os
from pathlib import Path
import secrets
import subprocess
import tempfile


def main():
    key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not key:
        raise SystemExit("Set ANTHROPIC_API_KEY to run this paid integration check")
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        text=True,
    ))
    binary = Path(metadata["target_directory"]) / "debug" / "roomkit-code"
    if not binary.is_file():
        raise SystemExit("Run cargo build --locked first")
    model = os.environ.get("ANTHROPIC_TEST_MODEL", "claude-sonnet-5-5")
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("ROOMKIT_", "HERDR_"))}
    with tempfile.TemporaryDirectory(prefix="roomkit-code-anthropic-") as directory:
        root = Path(directory)
        project = root / "project"
        project.mkdir()
        expected = secrets.token_hex(16)
        (project / "fixture.txt").write_text(expected + "\n")
        base = [str(binary), str(project), "--state-dir", str(root / "state"),
                "--session", "anthropic-check", "--mode", "yolo", "--no-herdr",
                "--no-update-check", "--json"]
        bootstrap = ["--provider", "anthropic", "--model", model,
                     "--provider-timeout-seconds", "120"]
        if effort := os.environ.get("ANTHROPIC_TEST_EFFORT"):
            bootstrap += ["--reasoning-effort", effort]
        launch(base + bootstrap, env,
               "Read fixture.txt using the read tool, then use the write tool to "
               "copy its exact contents into result.txt. Finish with a short confirmation.")
        assert (project / "result.txt").read_text().strip() == expected
        # Existing connections take precedence over new bootstrap credentials.
        env["ANTHROPIC_API_KEY"] = "ignored-bootstrap-credential"
        env["ROOMKIT_API_KEY"] = "ignored-bootstrap-credential"
        (project / "fixture.txt").unlink()
        (project / "result.txt").unlink()
        launch(base, env,
               "Resume our previous conversation. From the file content you read "
               "earlier, write the same exact token into resumed.txt using the write "
               "tool. The earlier files have been removed. Finish with a short confirmation.")
        assert (project / "resumed.txt").read_text().strip() == expected
    print("Anthropic live check passed: native launch, read/write tools and saved session resume")


def launch(arguments, env, prompt):
    result = subprocess.run(arguments + ["--prompt", prompt], env=env,
                            capture_output=True, text=True, timeout=180)
    # Never print raw provider output or stored credentials on failure.
    if result.returncode:
        raise AssertionError(f"Anthropic launch failed with exit status {result.returncode}")
    events = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
    assert events, "The application returned no protocol events"


if __name__ == "__main__":
    main()
