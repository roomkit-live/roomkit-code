# RoomKit Code

A Rust coding assistant assembled from the RoomKit ecosystem. This application
owns development behavior, configuration and workspace choices. The reusable
terminal interface, conversation engine and tool implementations live in their
own repositories.

This version can **explore, edit and test projects**, with Ask/Yolo permissions.
It supports an
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

You can also launch without a URL/model and configure everything using
**/providers**, then **/agents**. Messages are disabled until an agent is ready.

The model needs to support function calling and streamed Chat Completions.
API requests go to the selected endpoint. File contents returned by tools can
be included in those requests.

Alternatively, copy `roomkit-code.example.toml` to `roomkit-code.local.toml` and
pass `--config roomkit-code.local.toml`. CLI values override environment values,
which override the explicit TOML file. Project configuration and `.env` files
are never loaded automatically. These connection values initialize the project's first agent once. Thereafter,
saved profiles take precedence so an old launch command cannot undo UI edits.
Appearance and permission flags still apply at every launch. A supplied
`ROOMKIT_API_KEY` is imported into the protected local connection profile.

```sh
cargo run --locked -- /path/to/project --config roomkit-code.local.toml
cargo run --locked -- /path/to/project --session investigation --theme light
cargo run --locked -- /path/to/project --check-config
```

Each normal launch opens a blank conversation, reusing an unused application
draft when available. Use **/resume** to list earlier nonempty
sessions for this project, filter by their first message, date or identifier,
then press Enter to continue one. Escape cancels without switching. **/new**
starts another conversation without leaving the application, or keeps the
current draft if it is already empty. These commands
are local navigation and never call the model; wait for an active response to
finish before switching.

`--session NAME` explicitly opens or creates a named session. Existing sessions,
including the former default `main`, remain available through `/resume` or
`--session main`. The picker hides empty drafts and shows up to 200 recent
nonempty sessions, with timestamps in
UTC; an older known identifier can be opened with `/resume ID`.

`--check-config` validates launch options and the workspace without loading
saved profiles, contacting a provider or creating state. `--help` lists all options. Terminal controls
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

## Providers and agents

**/providers** manages connections shared across your projects. Add a name, API
base URL, timeout and authentication: no key, a stored key, or a named environment
variable. Stored keys live in a private local file (0600), not an OS keychain;
secret fields are masked, never prefilled, never echoed in diagnostics and never
stored in chat history. Leaving a stored-key field blank preserves that key.
Choosing another authentication mode removes the stored key from that profile.
Testing a connection makes a bounded `GET /models` request with no redirects;
it sends no conversation. Some compatible servers do not implement discovery,
so model identifiers can always be entered manually.

**/agents** manages the project's interlocutors. Each has a stable ID, display
name, provider, model, optional context capacity, additional instructions and
selected tools. The tools field accepts comma-separated names; an empty field
creates an agent without tools. Saving an agent also selects it. Selection and
configuration wait for the current execution to finish. The selected agent's
name/model appear in the footer, and only that agent receives the next message.
The conversation remains shared, with stable authorship for previous messages;
selecting a different provider can therefore send existing conversation context
to that provider. This version does not orchestrate simultaneous responders.

Forms use **Tab / Shift+Tab** for fields, **Left / Right** for choices,
**Alt+Enter** for multiline instructions, **Ctrl+U** to clear a field,
**Ctrl+S** to save and **Esc** to cancel. Invalid values retain your draft.
Existing IDs cannot be renamed because they identify authors and references.
Profile deletion and effort selection are not implemented yet.

Connections are stored in `connections.json` under the user state directory;
project composition and the last selected agent live in
`projects/<workspace-path-hash>/agents.json`. Files are versioned, private and
replaced atomically; conflicting connection edits from another process are
refused instead of overwritten. To apply a connection changed by another
project, reopen `/agents` and select the agent again. Profile files are outside
the source tree and are never loaded from project instructions or model output.

## Tools and persistence

Use **/tools** to browse the capabilities actually registered by the application,
with filtering and their tool descriptions. Selecting an entry only reports its
availability; it never invokes the tool or contacts the model. This catalogue
and the model's current capability inventory are derived from the same selected
toolsets. Resuming keeps conversation history, but older assistant claims about
missing tools do not change the currently registered capabilities.

New agents initially select `code_outline`, `list`, `glob`, `grep`, `read`, `diff`, `write`, `edit`,
`patch` and `shell`, implemented by `roomkit-tools-code`, `roomkit-tools-fs` and `roomkit-tools-process`
and registered through their existing RoomKit adapters. Persistent process and
web tools are not selected. Returned pages and scans are bounded by the tools' defaults. Hidden files are included and ignore files are not applied;
choose a workspace appropriate to share with the configured model. Filesystem tools check paths under the configured root. Shell uses `/bin/sh -c`
with your user permissions and environment; its initial workspace directory is
**not a sandbox**. It has closed stdin, bounded output, a default 10-second
runtime and a maximum requested runtime of 120 seconds.

**Ask is the default.** Each `write`, `edit`, applied `patch` and `shell` call
waits for an explicit decision. The panel shows exact arguments; use PgUp/PgDn
to read longer details, **Ctrl+Y** to allow once or **Ctrl+N** to refuse.
Read tools, `diff` and patch previews do not need approval. Requests expire after
five minutes; disconnecting refuses pending actions. Actions too large to show
in full (64 KiB of approval detail) are refused and must be split.

Press **Shift+Tab** to cycle between **Ask** and **Yolo**, including while an
agent is working. The status line shows the host-confirmed mode. `/mode ask` and
`/mode yolo` select directly; `/mode` offers choices. Changing modes preserves
the draft and does not settle an already pending request or stop an action that
was already authorized. Yolo permits subsequent selected tool calls without
asking; tool path, revision, timeout and size checks still apply.

Modes are kept per conversation for this process. Restart defaults to Ask;
`--mode yolo` or `[permissions] mode = "yolo"` in an explicitly loaded TOML file
can select a different starting mode. Project files never implicitly select it.

`write`, `edit` and `patch` share a mutation lock. The agent is instructed to
read before modifying and pass content revisions to write/edit. Patch previews
do not modify files; applying requires `dry_run: false` and exact source text.
Multi-file patches can partially apply; their result lists committed effects.
These checks coordinate the selected tools, not external editors.

Tool cards show paths, line numbers, entry/match counts and a short preview;
F2 or clicking their disclosure reveals more detail. The product decodes the
known tools' JSON envelopes only for presentation, using their Rust output
types. Failed, unsupported or truncated envelopes fall back to literal/JSON
display; presentation never changes the host's confirmed execution state.
Changes use green/red excerpts and diffs; patch previews are labeled separately
from applied changes. Shell cards show exit status, timeouts, stdout and stderr.
A completed shell tool does not imply a successful command.
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
changes apply to future turns. Effort selection, automatic compaction
and a remote-engine transport are not implemented in this version.

`code_outline` uses Tree-sitter to locate Rust/Python definitions, signatures and
line ranges before a targeted `read`. It does not resolve types or index an
entire project. Outline cards show source ranges, syntax warnings and pagination;
expansion reveals more symbols only when that result contains them.

## Context, consumption and request failures

Context shares the model footer when it fits, and uses a separate row in narrow
windows. It shows the last completed generation's measured context: input tokens
(including cached input) plus output tokens. Set the agent's **Context capacity** in `/agents` to the actual server capacity to see used and remaining
percentages. Without capacity the token count remains visible; without reported
counts the context stays unknown unless an earlier measurement is available.
While awaiting a new report, that earlier context remains visible with the
explicit `last report` label. This is the last measured exchange, not a
prediction including an unsent message or new tool definitions.

The separate **turn** counter totals all reported input/output tokens across
that execution's model calls. Cached input is counted once. Missing measurements
or a failed generation mark totals partial. These are neither session-wide
billing totals nor dollar costs. Resume restores applicable measurements; a
new execution clears its turn totals. Changing the model or conversation clears
the previous-context fallback too.

The connection's **Timeout** in `/providers` controls the entire HTTP model
request, including streaming. The initial TOML/CLI timeout and context flags
remain available when bootstrapping a new project. The application defaults to 600 seconds;
the library's own default is unchanged. Failures explain timeout, access, rate
limit, server and protocol errors without echoing raw provider responses or keys.
Failed requests are not automatically retried. Existing completed tool actions
remain in the project and conversation.

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
`src/sessions.rs` owns new/resume/mode/tools commands and the project session catalogue.
`src/permissions.rs` owns the tool policy and hooks that await actual approval.
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
