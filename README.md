# RoomKit Code

A Rust coding assistant assembled from the RoomKit ecosystem. This application
owns development behavior, configuration and workspace choices. The reusable
terminal interface, conversation engine and tool implementations live in their
own repositories.

This first version explores projects with **read-only tools**. It supports an
OpenAI-compatible Chat Completions endpoint, streamed answers, tool details,
activity timing, mouse selection, configurable appearance and SQLite history.
Linux and macOS are the supported platforms. The repositories are currently
private; building requires Git read access to all three dependencies.

## Run

Install Rust through rustup; this checkout selects its pinned toolchain.

```sh
cargo build --locked
export ROOMKIT_BASE_URL=http://localhost:8000/v1
export ROOMKIT_MODEL=your-model
# Set ROOMKIT_API_KEY only if the endpoint needs authentication.
cargo run --locked -- /path/to/project
```

The model needs to support function calling and streamed Chat Completions.
API requests go to the selected endpoint. File contents returned by tools can
be included in those requests.

Alternatively, copy `roomkit-code.example.toml` to `roomkit-code.local.toml` and
pass `--config roomkit-code.local.toml`. CLI values override environment values,
which override the explicit TOML file. Project configuration and `.env` files
are never loaded automatically. API keys come only from `ROOMKIT_API_KEY`.

```sh
cargo run --locked -- /path/to/project --config roomkit-code.local.toml
cargo run --locked -- /path/to/project --session investigation --theme light
cargo run --locked -- /path/to/project --check-config
```

Each normal launch starts a new conversation. Use **/resume** to list earlier
sessions for this project, filter by their first message, date or identifier,
then press Enter to continue one. Escape cancels without switching. **/new**
starts another conversation without leaving the application. These commands
are local navigation and never call the model; wait for an active response to
finish before switching.

`--session NAME` explicitly opens or creates a named session. Existing sessions,
including the former default `main`, remain available through `/resume` or
`--session main`. The picker shows up to 200 recent sessions, with timestamps in
UTC; an older known identifier can be opened with `/resume ID`.

`--check-config` validates configuration and the workspace without contacting
the provider or creating state. `--help` lists all options. Terminal controls
are provided by `roomkit-tui`: F2 expands tool details, clicking a tool's
disclosure does the same, and dragging text requests clipboard copying through
OSC 52 when supported by the terminal. `--native-selection` delegates selection
to the terminal and disables application mouse interactions. Ctrl+D detaches;
this local application then closes its owned engine. Per-run interruption is
not yet available in the local adapter.

Scrolling above the latest activity reveals a **Back to bottom** control.
Click it or press **Ctrl+End** to return without losing the current draft.
The filled input area starts at three rows, centers the draft vertically and
grows with newlines and soft wrapping. `ui.input_min_lines` and `ui.input_padding`
configure its editable height and vertical padding. `ui.input_placeholder` sets
the hint shown while empty, and `ui.input_prompt` sets its small marker; an empty
string hides either. Neither is included in messages sent to the model.
The selected theme supplies its background and text colors; monochrome mode
uses reverse video to keep the area visible.

## Tools and persistence

The selected agent has `list`, `glob`, `grep` and `read`, implemented by
`roomkit-tools-fs` and registered through its existing RoomKit adapters. It has
no write, shell, process or web tools. Returned pages and scans are bounded by
the tools' defaults. Hidden files are included and ignore files are not applied;
choose a workspace appropriate to share with the configured model. Root checks
are filesystem boundaries, not an OS sandbox.

Tool cards show paths, line numbers, entry/match counts and a short preview;
F2 or clicking their disclosure reveals more detail. The product decodes the
known tools' JSON envelopes only for presentation, using their Rust output
types. Failed, unsupported or truncated envelopes fall back to literal/JSON
display; presentation never changes the host's confirmed execution state.
Model messages use the CLI's Markdown renderer, including colored code blocks.
Set `ui.markdown`, `ui.message_gap` and `ui.tool_preview_lines` in explicit TOML
configuration to control formatting and density. Message source and stored
tool results remain unchanged.

History lives in the platform's per-user application data directory, under
`projects/<workspace-path-hash>/history.sqlite3`. `--state-dir` or
`ROOMKIT_CODE_STATE_DIR` overrides that location. The canonical workspace path
determines the project identity: symlink aliases share history, moving a project
starts a separate history. Each project storage has an
exclusive process lock; simultaneous local instances for that project are
refused. Files are created with private permissions on Linux and macOS.

The engine owns persistence and bounded context restoration. The terminal
adapter displays up to 500 stored events; the current provider uses the engine's
bounded message/tool history, not the entire on-screen conversation. Model
changes on restart apply to future turns. Effort selection, context percentage,
interactive tool approvals and a remote-engine
transport are not implemented in this version.

## Boundaries

| Repository | Responsibility |
| --- | --- |
| [roomkit-code](https://github.com/roomkit-live/roomkit-code) | Development instructions, selected tools, workspace, configuration, lifecycle |
| [roomkit-cli](https://github.com/roomkit-live/roomkit-cli) | Generic terminal interface, client protocol, local engine adapter |
| [roomkit-rs](https://github.com/roomkit-live/roomkit-rs) | Conversation engine, agents, providers, policies, persistence |
| [roomkit-tools](https://github.com/roomkit-live/roomkit-tools) | Independently reusable tools and their RoomKit adapters |

`src/agent.rs` selects capabilities; `src/coding-agent.txt` supplies development
instructions. `src/application.rs` assembles and closes the engine.
`src/config.rs` and `src/storage.rs` own application settings and project state.
`src/sessions.rs` owns new/resume commands and the project session catalogue.
The TUI receives only its generic client connection, preserving the boundary
needed for a future remote host.

## Development

```sh
make check
make msrv
make audit # requires cargo-deny
```

Tests use temporary projects and a local HTTP fixture, with no model service or
credentials. They exercise real file tools, streaming, persistence and terminal
cleanup. See [CONTRIBUTING.md](CONTRIBUTING.md) for dependency and CI setup.

MIT licensed. This is an initial development version, not a published release.
