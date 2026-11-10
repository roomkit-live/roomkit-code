"""Opt-in paid check: recover from a real Fable signature rejection.

Set ANTHROPIC_API_KEY and build first (or set ANTHROPIC_TEST_BINARY).
A local proxy changes the synthetic system prefix after the first signed round.
ANTHROPIC_TEST_SIGNATURE_MODE=invalid instead corrupts that round's signature.
Only disposable project data reaches Anthropic; credentials and raw provider
responses are never printed.
"""
import http.server
import json
import os
from pathlib import Path
import secrets
import re
import subprocess
import tempfile
import threading
import urllib.error
import urllib.request


def main():
    if not os.environ.get("ANTHROPIC_API_KEY"):
        raise SystemExit("Set ANTHROPIC_API_KEY to run this paid integration check")
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], text=True))
    binary = Path(os.environ.get("ANTHROPIC_TEST_BINARY", str(Path(metadata["target_directory"]) / "debug" / "roomkit-code")))
    invalid = os.environ.get("ANTHROPIC_TEST_SIGNATURE_MODE") == "invalid"
    corrupted = None
    state = {"changed": False, "rejections": 0, "requests": 0, "recovered": False, "errors": []}

    class Proxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            nonlocal corrupted
            body = json.loads(self.rfile.read(int(self.headers["content-length"])))
            state["requests"] += 1
            thinking = any(part.get("type") == "thinking"
                           for message in body["messages"]
                           if isinstance(message.get("content"), list)
                           for part in message["content"])
            state["changed"] |= thinking
            if invalid:
                for message in body["messages"]:
                    if not isinstance(message.get("content"), list):
                        continue
                    for part in message["content"]:
                        if part.get("type") == "thinking":
                            if corrupted is None:
                                corrupted = part["signature"]
                            if part["signature"] == corrupted:
                                part["signature"] = "invalid-signature"
            elif state["changed"]:
                # Keep this prefix edit on the retry and every later request.
                body["system"].append({"type": "text", "text": "Keep the final confirmation brief."})
            headers = {name: self.headers[name] for name in
                       ("x-api-key", "anthropic-version", "content-type")}
            if self.headers.get("anthropic-beta"):
                headers["anthropic-beta"] = self.headers["anthropic-beta"]
            request = urllib.request.Request("https://api.anthropic.com/v1/messages",
                                             data=json.dumps(body).encode(), headers=headers)
            try:
                with urllib.request.urlopen(request, timeout=180) as response:
                    payload, status = response.read(), response.status
                    kind = response.headers.get("content-type", "text/event-stream")
                    if state["rejections"]:
                        state["recovered"] = True
            except urllib.error.HTTPError as response:
                payload, status = response.read(), response.code
                kind = "application/json"
                message = json.loads(payload).get("error", {}).get("message", "")
                state["errors"].append({"status": status,
                    "shape": [[m["role"], [p["type"] for p in m["content"]] if isinstance(m["content"], list) else "text"] for m in body["messages"]],
                    "path": re.findall(r"messages\.\d+\.content\.\d+", message),
                    "expected_wording": "Invalid signature in thinking block" in message.replace("`", ""),
                    "bound": "bound to a different conversation" in message})
                if status == 400 and (b"signature" in payload if invalid else b"bound to a different conversation" in payload):
                    state["rejections"] += 1
            self.send_response(status)
            self.send_header("content-type", kind)
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        with tempfile.TemporaryDirectory(prefix="roomkit-anthropic-recovery-") as directory:
            root = Path(directory)
            project = root / "project"
            project.mkdir()
            token = secrets.token_hex(12)
            (project / "fixture.txt").write_text(token + "\n")
            env = {k: v for k, v in os.environ.items() if not k.startswith(("ROOMKIT_", "HERDR_"))}
            result = subprocess.run([
                str(binary), str(project), "--state-dir", str(root / "state"),
                "--provider", "anthropic", "--base-url", f"http://127.0.0.1:{server.server_port}",
                "--model", "claude-fable-5-2", "--reasoning-effort", "xhigh",
                "--mode", "yolo", "--no-herdr", "--no-update-check", "--json",
                "--prompt", "Read fixture.txt with the read tool, then write its exact contents "
                "to result.txt with the write tool. Finish with a brief confirmation.",
            ], env=env, capture_output=True, text=True, timeout=360)
            if result.returncode:
                records = [json.loads(line) for path in (root / "state").rglob("diagnostics.jsonl") for line in path.read_text().splitlines()]
                safe = [{k: r[k] for k in ("event", "code", "reason") if k in r} for r in records if r.get("event") in ("engine_error", "provider_recovery")]
                raise AssertionError(f"Synthetic launch failed ({result.returncode}); {state}; {safe}")
            assert (project / "result.txt").read_text().strip() == token
            assert state["rejections"] == 1, "Expected exactly one real signature rejection"
            assert state["recovered"], "No successful request after the rejection"
            logs = list((root / "state").rglob("diagnostics.jsonl"))
            records = [json.loads(line) for path in logs for line in path.read_text().splitlines()]
            assert sum(r.get("reason") == "reasoning_replay" for r in records) == 1
            failures = [r for r in records if r.get("event") == "provider_request_finished"
                        and r.get("outcome") == "error"]
            assert len(failures) == 1
            assert failures[0]["http_status"] == 400
            assert failures[0]["recovery"] == "retry"
            assert failures[0]["block_path"] is not None
            assert failures[0]["grouped_target"] is not None
    finally:
        server.shutdown()
        server.server_close()
        worker.join()
    print("Fable live recovery passed: one real signature rejection, automatic retry and completed tools")


if __name__ == "__main__":
    main()
