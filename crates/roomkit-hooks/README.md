# roomkit-hooks

Reusable Rust command-hook configuration, matching, execution and decision parsing.
This crate has no engine, provider, terminal or coding-agent dependency. The host
owns discovery, trust, permission policy and lifecycle dispatch.

`Configuration::parse` accepts event → matcher → command handlers. Unknown events,
handler types and fields are rejected. `run` receives a validated `CommandHook`,
working directory and JSON object. `interpret` returns `Continue`, `Deny` or `Ask`.
`Continue` means continue the host's permission checks, never bypass them.

Commands run through `/bin/sh -c` on Linux/macOS with JSON on stdin. Both output
streams are drained concurrently and bounded to 32 KiB each. Inputs are bounded
to 1 MiB. Timeouts are 1–30 seconds; configurations permit at most 60 seconds per
event per source. Dropping execution kills its process group. Normal exit and
timeout also clean up background members of that group. Commands inherit the
host's environment and user permissions; this is not a filesystem/network sandbox.
Processes deliberately escaping their process group are outside this cleanup.

The first contract supports `SessionStart`, `SessionEnd`, `UserPromptSubmit`,
`TurnStart`, `TurnEnd`, `PreToolUse`, `PostToolUse`, and `PostToolUseFailure`.
It does not implement `Stop` continuation, input rewriting, asynchronous command
handlers, HTTP/MCP handlers or prompt/agent hooks. Hosts should document exactly
which payloads and event timings they supply.
