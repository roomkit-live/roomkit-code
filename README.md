# RoomKit Code

A Rust coding assistant assembled from the RoomKit ecosystem. This application
owns development behavior, configuration and workspace choices. The reusable
terminal interface, conversation engine, authentication and tool implementations live in their
own repositories.

This version can **explore, edit and test projects**, with Ask/Yolo permissions.
It supports OpenAI-compatible Chat Completions endpoints and Sign in with
ChatGPT through Responses, with streamed answers, tool details,
activity timing, mouse selection, configurable appearance and SQLite history.
Linux and macOS are the supported platforms. The repositories are currently
private; building requires Git read access to all four dependencies.

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

The selected model must support function calling and the provider's streaming protocol.
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
this local application then closes its owned engine. Esc closes the current menu or panel, otherwise it interrupts the active
execution while keeping the draft. Ctrl+C clears the draft and interrupts active
work; a second press within two seconds exits. Typing or pasting disarms exit.
Interruption waits for engine cleanup, preserves completed tool effects and
leaves the conversation ready to continue.

Type **!command** (for example `!git status`) to run a command directly in the
project directory without invoking the model. `/shell command` is equivalent.
The composer shows a command hint; Enter runs, Alt+Enter adds a line, and `\!`
sends a literal leading exclamation mark to the agent. Explicit commands execute
in both Ask and Yolo; those modes govern commands proposed by the agent.

Commands use `/bin/sh -c` without a login shell or PTY, with stdin closed and a
five-minute lifetime. Each starts in the project directory: `cd` affects that
command only. Stdout/stderr, exit status and duration appear in the conversation;
Esc interrupts and waits for cleanup. F6 or **/processes** inspects or stops the
process. Finished commands automatically leave the live list and footer.
The retained conversation contains up to 4 KiB per output stream, with explicit
truncation; use `/processes ID` from the result to page larger output while the
record is retained in the current application. Old completed manual process slots are reclaimed as needed.
The command and bounded result survive resume and enter the next agent request;
no model request is sent automatically. Avoid entering credentials in commands
that should not be saved in conversation history.

Scrolling above the latest activity reveals a **Back to bottom** control.
Click it or press **Ctrl+End** to return without losing the current draft.
The filled input area starts at three rows, centers the draft vertically and
grows with newlines and soft wrapping. `ui.input_min_lines` and `ui.input_padding`
configure its editable height and vertical padding. `ui.input_placeholder` sets
the hint shown while empty, and `ui.input_prompt` sets its small marker; an empty
string hides either. Neither is included in messages sent to the model.
The selected theme supplies its background and text colors; monochrome mode
uses reverse video to keep the area visible.

## Terminal integrations

Two independent integrations are enabled by default. Neither changes the tools,
permissions or provider configuration selected by the application.

| Integration | Behavior | Disable |
| --- | --- | --- |
| Terminal status | OSC 7501 for Herdr App/Core and compatible terminals | `--no-terminal-status` or `[ui] terminal_status = false` |
| Herdr TUI | Activity, conversation title and session resume through `herdr` | `--no-herdr` or `[integrations] herdr = false` |

Terminal status reports idle, working, permission waits, completion and failure.
Completion/failure stays visible until the next user interaction; exit clears it.
No status is emitted for redirected output or `TERM=dumb`. This integration
does not need Herdr environment variables or execute `hc`.

The Herdr TUI adapter activates only with `HERDR_ENV=1` and nonempty
`HERDR_BIN_PATH`, `HERDR_SOCKET_PATH` and `HERDR_PANE_ID`. It uses that executable
and socket without a shell. Reports use the stable source/agent `roomkit-code`;
the separate `roomkit-code-title` metadata source is scoped to that agent.
Only the latest observed state is retained, duplicate reports are skipped, and
each command has a 500 ms deadline. Failures retry without blocking the UI;
private diagnostics contain an integration error code, never raw command output.
Exit freezes reporting, completes application cleanup, clears the title and
releases the agent.

Herdr's published resume command uses `roomkit-code` from `PATH`. Install it there
before relying on pane restoration. The command preserves the absolute workspace,
state directory, explicit config file, appearance flags, currently displayed
conversation and host-confirmed Ask/Yolo mode. Saved profiles supply the provider
and selected agent; bootstrap model options and credentials are not copied into
the command. Session changes update the command only after successful navigation.
Resuming restores history without replaying model requests or tools.

Herdr TUI accepts at most 64 arguments and 8 KiB of argument text, without
apostrophes or control characters. Non-UTF-8 paths or arguments outside these
limits disable resume while retaining status and title reporting. The adapter
clears any previous resume command in that case. App/Core receives terminal
status only; this TUI resume integration does not call the Core API.

Sequence counters and exclusive reporter locks live under
`integrations/herdr` in the default user state directory, independently of
`--state-dir`, so changing project storage cannot roll reports backward in the
same pane. An unavailable/invalid counter or another reporter holding that pane
disables the TUI adapter for that launch and records `sequence_unavailable`.
These files contain counters, not conversation content or resume arguments.

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

New agents initially select all 19 native coding tools:

| Purpose | Tools |
| --- | --- |
| Explore and understand | `code_outline`, `list`, `glob`, `grep`, `read` |
| Compare and change files | `diff`, `write`, `edit`, `patch` |
| Run commands and tests | `shell` |
| Public web sources | `web_search`, `web_fetch` |
| Persistent commands and stdin | `process_start`, `process_status`, `process_read`, `process_write`, `process_stop` |
| Recover long output | `output_read`, `output_forget` |

These reuse the native `roomkit-tools` implementations and their RoomKit adapters.
`/agents` can restrict each participant's comma-separated selection. On the first
upgrade, profiles with exactly the original ten-tool catalogue receive the new
catalogue; existing subsets stay restricted. This migration is saved once in
project schema 2. Later explicit selections, including the original ten tools,
are preserved unchanged. Older application versions cannot read schema 2.

`web_search` uses **Exa's anonymous service**, independently of the model's
provider; no API key or model credential is sent to Exa. Availability and rate
limits belong to that service, with no silent fallback. `web_fetch` retrieves
bounded public HTTP(S) pages as Markdown/text without JavaScript (8 KiB content previews). Private
network destinations are refused by default, including redirects. Search queries
are sent to Exa; keep project secrets and private source out of queries. Retrieved
content is untrusted data. Web-service configuration in the UI is not yet exposed.

Returned pages and scans are bounded by the tools' defaults. Hidden files are
included and ignore files are not applied; choose a workspace appropriate to
share with the configured model. Filesystem tools check paths under the configured
root. Shell uses `/bin/sh -c` with your user permissions and environment; its
initial workspace directory is **not a sandbox**. It has closed stdin, bounded
output (8 KiB per stream in the immediate result), a default 10-second runtime and a maximum requested runtime of 120 seconds.

Persistent processes use pipes, not a PTY, and survive tool calls and profile
changes while the app runs. They default to a five-minute lifetime (up to one
hour), with eight retained jobs per session/agent and 32 total. Read stdout and
stderr with independent byte cursors (4 KiB pages per stream); `process_write` sends exact stdin text,
including any explicitly supplied newline, and can deliver EOF. Inspect partial
writes before deciding what to do next. Stop unneeded jobs and set `forget` to
release their retained records. The application closes remaining processes on exit.

**/processes**, **F6**, or the clickable **Processes** status open the live process
monitor. The list and footer show only running jobs from accessible conversations,
including other selected agents. Completed, failed or stopped jobs leave the list
automatically; the footer disappears when none remain. Open a running job to
inspect stdout/stderr, browse 4 KiB output pages, follow output or stop its group.
An already open detail panel retains the final result. `/processes ID` explicitly
opens a retained result by its identifier, with the same access checks, and lets
you browse output or forget the completed record.
These human controls work while the model is busy, preserve the composer draft,
and do not send a request to the model. Press Esc to return to the conversation.

The footer follows lifecycle notifications even after the agent finishes. Output
refreshes at most once per second while the panel is open; closed panels have no
refresh timer. Running processes and these records belong to the current app
instance and are not restored by `/resume` after restarting the application.

Shell and process tools share private temporary output archives, capped at 8 MiB
per stream and 64 MiB overall, with 4 KiB read pages. `output_read` pages an owned archive using byte
cursors; `output_forget` releases it. Check completeness and omitted bytes: these
are bounded copies, not a guarantee of lossless capture. Process and archive IDs
are scoped to the originating session and agent, survive reconfiguration, and
**do not survive restarting the app**, even when present in saved history.

**Ask is the default.** Each `write`, `edit`, applied `patch`, `shell`,
`process_start`, `process_write`, `web_search` and `web_fetch` call waits for an
explicit decision. The panel shows exact arguments and the kind of action;
use PgUp/PgDn for longer details, **Ctrl+Y** to allow once or **Ctrl+N** to refuse.
Read tools, `diff`, patch previews, process status/output, stopping owned jobs and
releasing owned archives do not need approval. Requests expire after five minutes;
disconnecting refuses pending actions. Actions too large to show in full
(64 KiB of approval detail) are refused and must be split.

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
`write` creates missing parent directories. `patch` uses structured JSON
add/update/delete/move operations, rather than a unified-diff string; it requires
existing parent directories. Multi-file patches can partially apply; their result lists committed effects.
These checks coordinate the selected tools, not external editors.

Tool cards show paths, line numbers, entry/match counts and a short preview;
F2 or clicking their disclosure reveals more detail. The product decodes the
known tools' JSON envelopes only for presentation, using their Rust output
types. Failed, unsupported or truncated envelopes fall back to literal/JSON
display; presentation never changes the host's confirmed execution state.
Changes use green/red excerpts and diffs; patch previews are labeled separately
from applied changes. Shell cards show exit status, timeouts, stdout and stderr.
Web cards show source titles, URLs and excerpts; process cards show lifecycle,
exit status, output cursors and archive completeness without raw byte arrays.
A completed shell tool does not imply a successful command.

Sub-agent delegation, inter-agent messaging, a Python execution interpreter and
plugin discovery are not selected. The available agent-tool facade still needs
a native Rust backend and application orchestration before it can be enabled.
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
| [roomkit-rs](https://github.com/roomkit-live/roomkit-rs) | Conversation engine, agents, providers, MCP client, policies, persistence |
| [roomkit-tools](https://github.com/roomkit-live/roomkit-tools) | Independently reusable tools and their RoomKit adapters |
| [roomkit-auth](https://github.com/roomkit-live/roomkit-auth) | OAuth, account identity, credential storage and renewal |

`src/agent.rs` selects capabilities; `src/coding-agent.txt` supplies development
instructions. `src/application.rs` assembles and closes the engine.
`src/config.rs` and `src/storage.rs` own application settings and project state.
`src/processes.rs` owns scoped process monitoring and human controls.
`src/accounts.rs` owns account menus and browser launching; `src/profiles/` links
accounts, providers and agents without exposing credentials to the UI.
`src/profiles/mcp/` owns MCP configuration and exposure; the engine's
`roomkit-mcp` crate owns protocol calls, aliases and server process cleanup.
`src/sessions.rs` owns new/resume/mode/tools/processes commands and the project session catalogue.
`src/permissions.rs` owns the tool policy and hooks that await actual approval.
The TUI receives only its generic client connection, preserving the boundary
needed for a future remote host.

## Development

```sh
make check
make msrv
make audit # requires cargo-deny
```

Tests use temporary projects, local HTTP fixtures and a stdio MCP fixture, with
no model service or credentials. They exercise real file tools, streaming, persistence and terminal
cleanup. See [CONTRIBUTING.md](CONTRIBUTING.md) for dependency and CI setup.

MIT licensed. This is an initial development version, not a published release.

## Sign in with ChatGPT

Open `/providers`, choose **Sign in with ChatGPT**, then **Connect a ChatGPT
account**. Complete the browser consent and return to the terminal. If the
browser does not open, copy the local URL shown in the panel. Esc cancels the
pending sign-in. Choose a model offered by the connected account, save the
agent, and use `/agents` to switch interlocutors.

Each account has its own registration and credentials. Several agents can use
one account, or different accounts and providers. `/providers` also offers
reauthorization and sign-out. Existing OpenAI-compatible connections continue
to use their configured endpoint and API key.

Authentication is implemented by `roomkit-auth`; RoomKit Code owns the menus
and account-to-agent association. ChatGPT credentials are resolved before each
model request and used only with the public OpenAI Responses API. The model
catalogue comes from the selected account, not a hard-coded list. Eligibility,
limits and model availability depend on the account and its granted scopes.

Tokens stay in `auth/accounts.json` under the configured application state
directory, separate from project profiles. These files are **unencrypted**, with
owner-only permissions and atomic writes. They must not be committed or shared.
Provider profiles contain only an opaque account reference. If remote revocation
cannot be confirmed, sign-out clears local tokens and reports that distinction.
The OAuth callback uses a local browser on the same machine; remote browser
forwarding and device-code sign-in are not implemented in this first version.

### Reasoning effort and diagnostics

Use `/effort` to select the active agent's reasoning effort, or edit **Reasoning
effort** in `/agents`. The setting is saved per agent and applies to subsequent
requests, including tool rounds. `default` leaves the parameter out; other levels
must be supported by the chosen model. Changing effort keeps the current session.

`/diagnostics` shows recent operational records and the exact log location. Each
project's private state directory contains `diagnostics.jsonl` and, after rotation,
`diagnostics.previous.jsonl` (at most 1 MiB each). Errors and tool outcomes are
recorded automatically using RoomKit's `ON_ERROR`, `ON_TOOL_CALL` and framework
events. Run `roomkit-code --debug` for additional lifecycle and delivery records.
Records include timestamps, room/event/channel identifiers, known tool names,
outcomes and safe error classifications. They never include prompts, tool
arguments/results, credentials, encrypted reasoning or raw provider error bodies.
The panel shows recent records for the current launch; files also cover previous
launches. File write failures are shown in the panel and do not stop the agent.
These operational logs are intended for troubleshooting, not as a tamper-proof
audit trail. Raw dependency `tracing` output is not enabled by `--debug`.

Context token counts become available after a response reports usage. A percentage
also requires the agent's effective **Context capacity**, configurable in `/agents`.

### Listing and removing profiles

`/agents` (or `/agents list`) lists this project's agents and marks the active
one. Select a profile to select, edit or delete it. `/providers` (or
`/providers list`) lists shared connections with their management actions.
You can also open a deletion confirmation directly with `/agents delete <id>`
or `/providers delete <id>`. Cancel is selected first.

Deleting an agent removes its configuration while keeping conversations. Deleting
the active agent selects the first remaining profile, or returns to setup if none
remain. An explicitly empty project is not repopulated from launch options.
A provider cannot be deleted while any saved project's agent references it;
change those agents' provider or delete them first. References are checked again
at confirmation, under a shared profile edit lock. Stored API keys are removed
with their provider. For ChatGPT, deleting a provider removes the connection
profile; account sign-out and credential revocation remain separate actions in
the ChatGPT account menu.

### Sending images

In `/agents`, edit the agent and enable **Image input** only when its model
supports vision. Press **Ctrl+V** with a screenshot/image in the desktop
clipboard. Attachment labels appear above the input; **Alt+Backspace** removes
the last image and **Ctrl+C** clears the draft. Add a prompt or send an image
alone with Enter. Text in the clipboard continues to paste as text.

Up to four PNG images of 4 MiB and 16 million pixels each can accompany a message.
RoomKit Code validates the image before sending it, and stores it outside the
workspace in the private project state directory. `/resume` can reuse those
attachments; the model receives the newest four images within its conversation
history. Older images retain text mentions. Missing image files produce an
unavailable-image marker. This applies to OpenAI-compatible Chat Completions
and ChatGPT Responses connections; no image tool is needed for native vision.

Image paste requires the native clipboard of the machine running the terminal
(X11, a supported Wayland data-control compositor, or macOS). A remote SSH
process cannot read your local desktop clipboard through terminal text paste.
Images are sent only when the message is submitted. Slash commands and `!`
commands cannot be submitted with images attached.

## Skills

Agents receive this application's skill conventions even before any skill is
enabled. When asked to create a project skill, their default destination is
`.agents/skills/<name>/SKILL.md`. Creating the file does not automatically load
or enable it: run `/skills reload`, then enable it for the selected agent.
The model receives the actual discovery roots from the same catalogue as the UI.

Use **/skills** to inspect the catalogue, source paths and loading errors for the
selected agent. Skills start **disabled**. Open one, inspect its instructions,
then choose **Enable for this agent**. The choice persists with that agent's
project profile. Other agents keep their own selection.

RoomKit's native `SkillRegistry` advertises only enabled names/descriptions to
the model. The model loads instructions with `activate_skill`, then supplementary
text with `read_skill_reference`. **Use in this conversation**, or
`/skills use project-review Review the public API`, sends an explicit request to
the current agent through the normal cancellable message path. Enabling a skill
alone does not start a model request.

Install each package as `<root>/<name>/SKILL.md`. `/skills locations` shows the
actual roots. Discovery scans their direct child directories, in this order;
the last source wins for duplicate names:

1. `<state-dir>/skills` — personal packages shared by this application's projects.
2. `<workspace>/.agents/skills` — portable project packages.
3. `<workspace>/.roomkit/skills` — application-specific project packages.

The state directory is selected with `--state-dir`; by default it is
`$XDG_DATA_HOME/roomkit-code` (or `~/.local/share/roomkit-code`) on Linux and
`~/Library/Application Support/roomkit-code` on macOS. No other assistant's
configuration or ancestor directory is imported automatically.

For example, `.agents/skills/project-review/SKILL.md`:

```markdown
---
name: project-review
description: Review project structure and public APIs
---
Inspect the entry points and public interfaces before suggesting changes.
Use read_skill_reference for the bundled references/checklist.md if present.
Present findings with file paths and concise explanations.
```

Use `/skills reload` after installing or editing packages. Discovery keeps valid
packages available when another fails, reports conflicts, and refuses directory
links escaping their root. A broken higher-priority package does not silently
fall back to another source. Removing or disabling a package removes it from
future model requests; earlier conversation text is retained.

Supported format: a UTF-8 manifest, required `name` and `description`, optional
`license` and `compatibility`, followed by Markdown instructions. Names must be
kebab-case and match the directory. Manifests and each reference are bounded to
256 KiB. References are plain filenames directly under `references/`. The
loader does not execute bundled scripts; an agent can use separately enabled
execution tools subject to Ask/Yolo, with relative paths based on the package
directory. Enabling a skill never grants additional tools or permissions.

Foreign tool-gating and invocation-policy fields, and `agents/openai.yaml`
sidecars, are reported as unsupported rather than silently ignored. Full
Codex/Maki/OpenCode plugin compatibility, automatic installation, nested
references, skill-specific tool grants and MCP-hosted skills are not included.

Active instructions are scoped to the conversation and agent, with up to four
active skills retained by the engine. They are recovered from retained tool
history when resuming; a fresh activation returns the instructions if no record
is available. Reload/restart uses the current installed content, not a historical
snapshot of the package.


## MCP servers

Use **/mcp** to configure local **stdio** or remote **Streamable HTTP** servers.
Connections are independent of model providers. Each local stdio server runs
with your user permissions in this project's workspace; it is not restricted by the built-in filesystem tools'
workspace boundary.

1. Choose **Add a stdio server**. Enter a server ID, executable and arguments as a
   JSON array. For example: program `python3`, arguments `["/path/to/server.py"]`.
   No shell parsing is performed; each array element is one argument.
2. If credentials are needed, export them before launching RoomKit Code. In
   **Environment references**, map the child variable to the host variable's
   **name**, for example `{"API_KEY":"MY_SERVICE_KEY"}`. The configuration stores
   references, not their values. Keep secrets out of arguments. Apart from the
   engine's basic environment allowlist, variables are only passed explicitly.
3. **Save** connects the server, discovers its tools and enables them for the
   current agent. Editing a connected server reconnects it automatically; opening
   or cancelling its form keeps the existing connection running. Existing tool
   selections, including fully disabled servers, are preserved.
4. Use **Choose tools for this agent** when you want to adjust the defaults.
   Tools appear as `mcp__<server>__<tool>` and cannot collide with built-in tools.
   Automatic activation fills the remaining capacity, up to 64 MCP tools across
   all servers for one agent. Larger catalogues produce a notice and use paginated
   individual selection. Names unsupported by the model protocol are omitted.

Connection and discovery remain interruptible with **Esc**. An unavailable
server leaves its configuration saved for correction or retry with **Connect**;
its tools are unavailable until connected. Invalid form values or missing
credential variables leave the form and the existing connection intact.

For a remote server choose **Add an HTTP server** (or `/mcp add-http`):

- **Endpoint URL**: the full MCP endpoint, for example
  `https://mcp.example.com/mcp`. JSON replies and SSE response streams are supported.
- **Bearer token · environment variable**: optional variable **name**, such as
  `MY_MCP_TOKEN`, exported before starting the app. The app adds `Bearer ` itself.
  Leave empty for an unauthenticated server.
- **Header references**: `{}` by default. For API-key authentication use, for
  example, `{"X-API-Key":"MY_MCP_KEY"}`. Values are environment variable names;
  never paste credentials here. Do not configure Authorization twice.
- **Save** connects and enables tools for the current agent, as for stdio.
  Credentials are resolved at connection time and their values are never saved.

HTTP redirects are not followed. An expired session requires explicit reconnect;
failed calls are not automatically replayed. Authentication failures show a safe
status without exposing response bodies or credential values. OAuth login and
refresh are not part of this transport yet.

In **Ask**, every MCP call requires approval, including tools advertised as
read-only. **Yolo** executes selected tools without asking. Server instructions
are framed as external content by the engine and grant no permission. Tool
results use the CLI's generic expandable presentation. **Esc** requests MCP
cancellation of a running call; the server must honor the protocol cancellation. This
cannot undo effects already performed. **Disconnect** stops and waits for the
owned stdio process group, including subprocesses on Linux and macOS. For HTTP,
it closes local streams and attempts to delete the negotiated session; it does
not stop the remote service.

The server panel shows connection state, process ID, selected tools and the
configured command or endpoint; use **Refresh status** after an unexpected server exit.
Configuration lives in the project's private state directory, in `mcp.json`;
selections live in `agents.json`. It is not loaded from a repository-provided MCP
file. Select a different agent with **/agents** to configure its own subset.
Disabling tools leaves the shared connection open. Before deleting a server,
disconnect it and remove its selections from every agent.

Selections persist, but connections do not start automatically on launch or
resume. Reconnect explicitly with `/mcp connect <id>`. A reconnect uses the
server's current catalogue while preserving an existing selection, including an
explicitly empty one. If this agent has never selected tools for the server,
Connect activates them within the same 64-tool limit. New tools do not expand an
existing selection and removed tools are unavailable. Connections survive agent and
conversation changes and are closed on application exit.

Existing stdio configurations are read automatically and upgraded on the next
save. This version includes stdio and Streamable HTTP tool discovery and calls.
Legacy HTTP+SSE transport, MCP OAuth, prompts/resources browsing,
subscription-based catalogue refresh and
MCP-hosted skills are not yet exposed. Non-text tool content follows the current
engine adapter's text/placeholder behavior.
