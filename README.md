# RoomKit Code

A Rust coding assistant assembled from the RoomKit ecosystem. This application
owns development behavior, configuration and workspace choices. The reusable
terminal interface, conversation engine, authentication and tool implementations live in their
own repositories.

This version can **explore, plan, edit and test projects**, with Ask/Plan/Yolo permissions.
It supports native Anthropic Messages, OpenAI-compatible Chat Completions endpoints and Sign in with
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

For Anthropic, choose **Anthropic** as the provider type in **/providers**,
then select a model in **/agents**. An empty API base uses
`https://api.anthropic.com`; a custom Messages gateway uses its API root without
`/v1`. Existing connections keep the OpenAI-compatible protocol.

```sh
export ANTHROPIC_API_KEY=your-key
cargo run --locked -- /path/to/project --provider anthropic \
  --model claude-sonnet-5-5
```

`--provider` (or `ROOMKIT_PROVIDER`) selects the protocol for the first agent.
The explicit TOML equivalent is `[provider] kind = "anthropic"`.
`ROOMKIT_API_KEY` takes precedence over `ANTHROPIC_API_KEY` when bootstrapping
an Anthropic connection. Saved connections retain their selected authentication.

The selected model must support function calling and the provider's streaming protocol.
API requests go to the selected endpoint. File contents returned by tools can
be included in those requests.

Alternatively, copy `roomkit-code.example.toml` to `roomkit-code.local.toml` and
pass `--config roomkit-code.local.toml`. CLI values override environment values,
which override the explicit TOML file. Project configuration and `.env` files
are never loaded automatically as connection settings. Project `AGENTS.md`
instructions are discovered separately as described below. These connection values initialize the project's first agent once. Thereafter,
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
sessions for this project, filter by their name, first message, date or identifier,
then press Enter to continue one. Escape cancels without switching. **/new**
starts another conversation without leaving the application, or keeps the
current draft if it is already empty. These commands
are local navigation and never call the model; wait for an active response to
finish before switching.

**/rename NAME** names the current conversation: one visible line of at most 80
characters. The name leads the header and the Herdr pane title, before the
workspace path, so a narrow terminal cuts the path rather than the name. It
labels the session in `/resume`, which keeps the first message in the
description, and precedes the ID in the exit report. The ID stays its
identity: `/resume ID`, `--session` and the resume command still use it. An
empty conversation cannot be named, since it is reused as the next draft.
Renaming works while a response runs: the header changes in place.

`--session NAME` explicitly opens or creates a named session. Existing sessions,
including the former default `main`, remain available through `/resume` or
`--session main`. The picker hides empty drafts and shows up to 200 recent
nonempty sessions, with timestamps in
UTC; an older known identifier can be opened with `/resume ID`.

Several instances can use the same project with different conversations. The
picker marks conversations open in another instance; those cannot be resumed
until that instance closes. Switching views does not close a conversation's
engine, hooks or background processes, so ownership lasts until application
shutdown. After a crash, the operating system releases ownership automatically.

`--check-config` validates launch options and the workspace without loading
saved profiles, contacting a provider or creating state. `--help` lists all options. Terminal controls
are provided by `roomkit-tui`: F2 expands tool details, clicking a tool's
disclosure does the same, and dragging text requests clipboard copying through
OSC 52 when supported by the terminal. `--native-selection` delegates selection
to the terminal and disables application mouse interactions. Ctrl+D detaches;
this local application then closes its owned engine. While processes run, the
first Ctrl+D does not exit: press it again within two seconds. The exit hint
states how many processes closing will stop, also over panels, forms and
approval cards, and after Ctrl+C. Esc
closes the current menu or panel, otherwise it interrupts the active
execution while keeping the draft. Ctrl+C clears the draft and interrupts active
work; a second press within two seconds exits. Typing or pasting disarms exit.
Interruption waits for engine cleanup, preserves completed tool effects and
leaves the conversation ready to continue.

The model's reasoning shows as it comes, when its server sends it apart from the
answer (`reasoning_content`, `reasoning` or `<think>` tags): `∴ Thinking` with its
last lines, then one line, `∴ Thought for 12.0s`, once the model goes on. A click
on that line opens that block alone, and a second closes it; F2 opens it with the
technical history. Nothing stores it, so it does not come back with `/resume`.

While the agent works, Enter sends the message to its running turn, which reads
it at its next step: after its current tool calls, or right after the model's
answer, which the model then follows with a reply to the message. A turn that
ends on its round limit, without an answer or on a failure does not read it;
the message stays in the conversation's history. The message shows "↳ sent to
the running turn" and stays below what the turn writes without it. Once the turn
passes that step, its line says "↳ read by the turn" and what follows goes below
it. This place lasts while the session runs: reopened, the conversation shows
the messages in the order they were stored. Sent once the turn's last answer is
complete, while it is being stored, the message gets a turn of its own instead,
shown as "↳ answered in its own turn", which Esc interrupts like any turn. A message with images,
or written while Esc interrupts the turn, stays in the composer. A message sent
while a turn is being prepared or finished (its `UserPromptSubmit`, `TurnStart`
and `TurnEnd` hooks included) or during a shell command is refused; Up brings
its text back. A shell command (`!`) or another application command that
cannot run while the model is busy is refused until the turn ends.

**Ctrl+S**, or **Ctrl+Enter** where the terminal reports it, sends a message
now: it interrupts whatever runs and sends the message once nothing runs any
more, in a turn of its own. A message whose turn it cuts stays in the
conversation, and that next turn reads it. Esc while it waits keeps the text in
the composer, and only the text as it was when the key was pressed leaves.

A message already sent to the running turn waits for that turn's next step, which
may be far, as during a long build. Its line says so while the turn runs: with an
empty composer, **Ctrl+S** cuts the turn, and a turn of its own answers what the
cut turn was handed, read where it is in the conversation. Esc drops that answer,
as does typing; the cut stands.

Once the view has closed, the terminal prints a short report:

```text
Session: 8275ad8052ba446aa433e9bade3f744d
To continue this session, run:
  roomkit-code /path/to/project --session 8275ad8052ba446aa433e9bade3f744d --mode ask
Token usage: total=245120 input=188331 (+ 1204553 cached) output=56789 · 19 turns
Stopped 1 background process:
  p1 · npm run dev · 4m 12s
```

The command reopens the conversation shown at exit, in its current mode, with
the launch's explicit options such as `--config` and display flags. It names
the storage only when the launch did, with `--state-dir` or
`ROOMKIT_CODE_STATE_DIR`. With
`cargo run`, replace `roomkit-code` by `cargo run --locked --`. An unused draft
has no resume line. Token usage covers every turn of the conversation, across
launches and agents: `input` excludes the cached input shown in parentheses and
`total` is input plus output. A stopped turn counts what its generations
reported. A turn without a complete provider report marks the line `(partial)`,
as does a generation stopped or still running as the view closes. Closing stops every running process of this application,
whether an agent or a `!` command started it. The report lists them, or says
they may still be running when cleanup could not be confirmed.

Type **!command** (for example `!git status`) to run a command directly in the
project directory without invoking the model. `/shell command` is equivalent.
The composer shows a command hint; Enter runs, Alt+Enter adds a line, and `\!`
sends a literal leading exclamation mark to the agent. Explicit commands execute
in both Ask and Yolo; those modes govern commands proposed by the agent.
Plan mode also disables manual `!` and `/shell` commands until you switch modes.

Commands use `/bin/sh -c` without a login shell or PTY, with stdin closed, a
five-minute lifetime and the environment described under the shell tool below. Each starts in the project directory: `cd` affects that
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

## Input history

Up/Down recalls your last 100 saved messages from the selected conversation,
including after `/resume` or a restart. Down past the newest entry restores your
unfinished draft. Commands typed during this launch are also recalled; old
commands are not reconstructed because they are not saved as conversation
messages.

## Unattended runs

`--prompt` (`-p`) sends one message without the terminal interface and exits
when the agent's turn ends, for scripts, CI jobs and benchmark harnesses.
Standard input and output may be pipes; `--prompt -` reads the message from
standard input.

```sh
roomkit-code /path/to/project --mode yolo --prompt "Run the tests and fix what fails"
roomkit-code /path/to/project --mode yolo --json -p - < task.md > events.jsonl
```

The turn runs as in the terminal: the same agent, hooks, project instructions,
history and compaction, and the same tools but those below. Standard output
receives the agent's last message; with `--json`, it receives every event of the
conversation instead, one JSON object per line, in the terminal protocol's own
form (`run_started`, `item` for each tool call and its result,
`message_finalized`, `usage_updated`, `run_finished`…). The answer comes without
the blank lines a model may open or close it on. Notices, refusals and the exit report (session, resume command,
usage) go to standard error.

When standard error is a terminal and the events are not asked for, it also
shows the launch as it runs: the workspace, model, provider, mode and session
first, then each message the agent follows with a call, a line as each tool
call starts, and one when it ends refused, failed, cancelled, or after 3
seconds or more. Standard output still receives the agent's last message; a
message no call follows shows there alone. Pipes and logs get none of it. What standard error shows holds no
control character.

```text
roomkit-code · /path/to/project
model: qwen3 · provider: Local · mode: ask (approvals refused) · session: session-39c9…

  Run cargo test
  Run cargo test · failed · 12s
Two tests fail on date parsing; fixing them.
  Write src/lib.rs · refused
```

The exit status is 0 when the turn ended with its
answer, 1 when the message was refused or the turn ended without its answer (its
round limit, an error), and 130 when SIGINT or SIGTERM cut it: the signal
interrupts the turn, which stops the commands still running and records what it
did; what the agent had started in the background runs on, as after any
unattended launch (below). A second signal exits at once, without waiting for
that, and commands the turn started may then keep running. A message holds at most 64 KiB. For now, a turn that wrote
a message in an earlier round, then was cut by its round limit or a failure
right after a round that only called tools, still exits 0 with that message: the
engine records that end on the round's last call after the terminal protocol
reports the turn finished.

Nobody answers approvals: each one is refused, and the agent reads the refusal.
Ask, the default mode, therefore refuses every change and command; use
`--mode yolo` to let the agent work, in a workspace you can afford to give it.
Plan mode's tools are not offered, since leaving Plan needs a person's
approval. Nor are the plan tools, unless the launch is in Plan mode, nor this
guide's tools (`plan_*`, `docs_*`), whatever the agent selects, so that each
request is smaller. The system prompt says nothing of the tools left out, as for
any agent without them, its permission summary included. The web tools stay,
for a prompt that asks to look something up. The conversation is saved like any
other: `--session NAME` continues it, and `/resume` lists it in the terminal.

A shell command may run up to an hour (`timeout_ms` up to 3600000) instead of
the terminal's two minutes, and the system prompt states that maximum: nobody
waits for its answer, and a long build or test run need not move to a process
the agent polls. A command still stops after 10 seconds unless its call asks for
longer.

What the agent starts runs on past the launch: a job a command puts in the
background (`server &`) outlives the command, and a process started with
`process_start` outlives roomkit-code, which lists those as `Left N background
processes running:` (jobs a command put in the background are not listed). A
check that follows the launch, such as a benchmark's verifier, can then reach a
server the agent started. What they write after their command returned or the
launch ended goes to a drainer that discards it, so their writes never fail. A
command that times out or is cut still stops what it started. In the terminal,
both stop with their command and with the application.

In the terminal, a turn runs at most 50 rounds of tool calls before it stops
unfinished, and a message continues it. Unattended, nobody can, so a turn runs
until it answers. `--max-tool-rounds N` or `[agent] max_tool_rounds` in the
configuration file sets that bound for both; the flag is kept in the resume
command.

`--model`, `--context-window` and `--reasoning-effort` set up a new project's
first agent: a project that already has agents, its own or global ones, keeps
their saved settings. A benchmark that pins them gives each run a fresh
`--state-dir`.

## Terminal integrations

Web addresses and Markdown link labels in replies are native terminal links.
In iTerm2, Cmd-click opens them in the Mac's browser, including over SSH. Wrapped
links retain their complete destination. Terminal preferences control opening;
`--native-selection` delegates all mouse handling to the terminal.

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
conversation and host-confirmed Ask/Plan/Yolo mode. Saved profiles supply the provider
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

**/providers** manages connections shared across your projects. Add a name, provider type, API
base URL, timeout and authentication: no key, a stored key, or a named environment
variable. Stored keys live in a private local file (0600), not an OS keychain;
secret fields are masked, never prefilled, never echoed in diagnostics and never
stored in chat history. Leaving a stored-key field blank preserves that key.
Choosing another authentication mode removes the stored key from that profile.
Saving a new connection lists the models it serves, and choosing one opens the
form of an agent using it; **Test connection and list models** lists them again.
The listing is a bounded `GET /models` request (`/v1/models` for Anthropic) with
no redirects; it sends no conversation. Each list of models for a new agent ends
with **Type a model ID**, for a model it does not show. Some compatible servers do not implement
discovery: the connection is saved all the same, and the model's ID is typed in
the agent's form. Subscriptions have their own entries:
[OpenCode Go](#opencode-go) and [Sign in with ChatGPT](#sign-in-with-chatgpt).

**/model** lists the models of the current agent's connection and switches the
agent to the one you choose, in the conversation you are in; `/model <model-id>`
sets one by its ID, listed or not. The agent keeps its other settings, the
context capacity included, and the conversation. Its own model, which a new
conversation starts with, is set in its form in `/agents`.

In **/agents**, **+ Add agent** asks for the connection when there are several,
lists that connection's models, then opens the form of an agent using the one
you choose. An agent's **Change model** lists its connection's models and opens
its form with the one you choose, its other settings unchanged.

**/agents** manages the interlocutors. Each has a stable ID, display name,
provider, model, optional context capacity and output limit, additional instructions, selected
tools and a **Scope**. A **Global** agent, the default, is available in every
project: a new project opens with the global agent used last, ready to write.
A **This project** agent exists only in its project. Changing the scope moves
the agent. A project sees an ID once; an ID another project uses for its own
agent cannot become global, and a new agent's default ID avoids those. A global
agent's provider, model, instructions and tools are shared; its MCP servers and
enabled skills stay each project's choice, because servers are configured per
project. Deleting a global agent removes it from every project; a project that
was using it opens a new conversation with another agent it can use. A new
global agent never inherits the choices a project kept for an earlier one with
the same ID. The tools field accepts comma-separated names; an empty field
creates an agent without tools. Saving an agent also selects it. Selection and configuration
wait for the current execution to finish. The selected agent's
name/model appear in the footer, and only that agent receives the next message.
The conversation remains shared, with stable authorship for previous messages;
selecting a different provider can therefore send existing conversation context
to that provider. This version does not orchestrate simultaneous responders.

Each conversation keeps the agent selected in it. Selecting an agent changes the
conversation you are in, and the agent a new conversation opens with at the next
launch; it never changes another conversation. Resuming one, with `/resume` or at
launch, shows its own agent: its history does not reach an agent, or a provider,
selected elsewhere. `/new` starts with the agent of the conversation you leave.
A conversation stored before its agent was recorded keeps the agent that worked
in it last. If a conversation's agent was deleted or cannot be configured, the
conversation still opens and says why nothing can be sent, until you select an
agent there; no other agent takes its place. Deleting the agent of the
conversation you are in switches that conversation to the agent the confirmation
names.

A conversation also keeps the model and the reasoning effort its agent runs
with. `/model` and `/effort` change them in the conversation you are in and
nowhere else: not in the agent's other conversations, nor in the agent's own
model and effort, which a new conversation starts with and which its form in
`/agents` sets. Saving that form with another model or effort also applies them
to the conversation you save it from; the agent's other conversations keep
theirs. A conversation stored before it kept these settings keeps the model of
its agent's last answer there. If the model a conversation keeps cannot be
configured any more, the conversation says so and sends nothing until you choose
one with `/model`: it never answers on another model. A delegated task runs with
its agent's own model and effort.

Forms use **Tab / Shift+Tab** for fields, **Left / Right** for choices,
**Alt+Enter** for multiline instructions, **Ctrl+U** to clear a field,
**Ctrl+S** to save and **Esc** to cancel. Invalid values retain your draft.
Existing IDs cannot be renamed because they identify authors and references.

Connections and global agents are stored in `connections.json` and
`agents.json` under the user state directory; project agents, a project's MCP
and skill choices for global agents and its selected agent live in
`projects/<workspace-path-hash>/agents.json`. Agents created before global
agents existed stay in their project. Files are versioned, private and
replaced atomically; conflicting connection edits from another process are
refused instead of overwritten. To apply a connection or a global agent changed
by another project, reopen `/agents` and select the agent again. Profile files
are outside the source tree and are never loaded from project instructions or
model output.

### Delegation

An agent can hand a task to another agent of the project, project or global,
with `delegate_task`. Each turn's application instruction lists the agents it
can choose, by ID and label; a task for itself or for no configured agent is
refused. The other agent works in a conversation of its own, in the
background, while the first goes on; when it is done, the first agent says its
result, or that it failed, in a turn of its own. `task_status` lists the
conversation's tasks and `cancel_task` stops one. Each task sets its agent up
from its current connection, skills and MCP servers, so an edit since applies,
whether or not the agent was ever selected. The other agent cannot hand its
task on for now: what it handed on would come back to it after its own task
ended, never reaching the conversation. A `--prompt` launch offers no delegation:
it ends with its turn, before a task could end.

The other agent reads the task and the `context` it was handed, a JSON
object, not the conversation. It works under the conversation's mode: in Ask
what needs an approval asks you, in Yolo it can change files and run commands,
in Plan it plans. Its conversation belongs to the one it was handed the task
in: `/resume` does not list it.

An approval it needs comes to the view you are in, named after the agent. Esc
refuses that one action and the agent goes on; `o` opens its room. It waits
five minutes, like any card, and stopping the task withdraws it.

The status line says when a task starts and how it ended, and its end leaves a
line in the conversation that asked for it. The sidebar's
**Tasks** lists the conversation's tasks that run, then the latest ended,
with how long they ran and what they consumed. **/tasks**, a click on a task,
or the footer's **Tasks** status, which F6 opens while no process runs, show
the conversation's tasks, those their agents handed on included: every one
that runs, and the latest ended, those of earlier launches too. Open one to
see its agent, state and the task it was given, then its conversation as
text, refreshed each second while it runs: what its agent said, and each tool
it called with how the call ended or that it still runs. **Stop task** cancels
one that runs: its agent stops, and the agent that handed it over is told it
was cancelled. Closing the application stops the tasks that run, and Ctrl+D
says so first, as for processes.

**Open its conversation** in `/tasks`, or Alt+Right and Alt+Left with an empty
composer, move the view into a task's room and back. From a task's room, `/back`
or **Back to the conversation** in the sidebar or `/tasks` returns directly. Alt+arrows go around the
conversation and the tasks that run; the conversation's agent keeps working
meanwhile. There you follow the agent as it works, and Esc stops its task. What
you write while it works reaches it at its next step, and its result still
goes to the agent that handed the task over. Once its task ended you can keep
talking to it: what you say stays in its room, and the agent that handed the
task over is not told.

What a task consumed is read when it ends, at the price of its agent's own
model: the footer and the sidebar then show the conversation's own cost and
its total with its tasks, `≈ $0.42 · total ≈ $1.30`, and the exit report totals
the conversation before giving each agent's part. The total reads partial when
it lacks something: a task with no price or in another currency, one stopped
before its usage was whole, one a crash left unfinished, or at exit one still
running.

## Tools and persistence

Use **/tools** to browse the capabilities actually registered by the application,
with filtering and their tool descriptions. Selecting an entry only reports its
availability; it never invokes the tool or contacts the model. This catalogue
and the tools the model is given are derived from the same selected toolsets. Resuming keeps conversation history, but older assistant claims about
missing tools do not change the currently registered capabilities.

New agents initially select all 27 native tools:

| Purpose | Tools |
| --- | --- |
| Explore and understand | `code_outline`, `list`, `glob`, `grep`, `read` |
| Compare and change files | `diff`, `write`, `edit`, `patch` |
| Run commands and tests | `shell` |
| Public web sources | `web_search`, `web_fetch` |
| Persistent commands and stdin | `process_start`, `process_status`, `process_read`, `process_write`, `process_stop` |
| Recover long output | `output_read`, `output_forget` |
| Maintain plans | `plan_create`, `plan_read`, `plan_list`, `plan_update`, `plan_delete` |
| Read this guide | `docs_search`, `docs_read`, `docs_list` |

These reuse the native `roomkit-tools` implementations and their RoomKit adapters.
`/agents` can restrict each participant's comma-separated selection.

`docs_search`, `docs_read` and `docs_list` give agents this guide, for the
installed version. The README is embedded in the binary, split into one document
per `##` section and written once per content revision under
`<state-dir>/docs/<revision>/`; earlier revisions stay for other running
versions. Search matches literal terms, all required, and returns one excerpt
per document; a read page holds up to 16 KiB. The guide is read-only reference
data: its tools run without approval, also in Plan mode, and it never grants a
tool or a permission. Only this local guide is served for now; project
documentation is not indexed.

`web_search` uses **Exa's anonymous service**, independently of the model's
provider; no API key or model credential is sent to Exa. Availability and rate
limits belong to that service, with no silent fallback. `web_fetch` retrieves
bounded public HTTP(S) pages as Markdown/text without JavaScript (8 KiB content previews). Private
network destinations are refused by default, including redirects. Search queries
are sent to Exa; keep project secrets and private source out of queries. Retrieved
content is untrusted data. Web-service configuration in the UI is not yet exposed.

Returned pages and scans are bounded by the tools' defaults. A result past a
tenth of the agent's context capacity, between 5,000 and 16,000 estimated tokens
(four characters each), reaches the model as a preview of its first and last
lines; `read_stored_result` pages through the whole or searches it. A full
50 KiB page of `read` or `grep` passes whole from a capacity of 131,072 tokens;
an agent without a known capacity keeps the 5,000-token bound. Hidden files are
included and ignore files are not applied; choose a workspace appropriate to
share with the configured model. Filesystem tools check paths under the configured
root. Shell uses `/bin/sh -c` with your user permissions; its initial
workspace directory is **not a sandbox**. It has closed stdin, bounded
output (the latest 16 KiB per stream in the immediate result, where a command
reports how it ended; a stream cut short keeps its full output in an archive
`output_read` pages through), a default 10-second runtime and a maximum requested runtime of 120 seconds, one hour in a `--prompt` launch.
Each command runs in its own session, without the terminal: a command that
prompts on the terminal fails at once.

Shell commands, persistent processes and `!` commands receive only part of your
environment: `PATH`, `HOME`, the user, shell, locale, time zone and temporary
directory variables, plus those development tools look for: `SSH_AUTH_SOCK`,
the desktop session (`DISPLAY`, `XAUTHORITY`, `WAYLAND_DISPLAY`, D-Bus) and XDG
directories, `TERM`, `COLORTERM`, the editors, the proxies (whose URL may embed
a password), the certificate bundles (`SSL_CERT_FILE`, `SSL_CERT_DIR`,
`NODE_EXTRA_CA_CERTS`, `REQUESTS_CA_BUNDLE`), `DOCKER_HOST`, `KUBECONFIG`, and
`CARGO_HOME`, `RUSTUP_HOME`, `JAVA_HOME`, `ANDROID_HOME`, `NVM_DIR`,
`VIRTUAL_ENV`, `GOPATH`. Every other variable, such as an API key exported in
your shell, stays out of commands (`src/command_env.rs`).

Add the variables your environment needs, for example those direnv, a Nix shell
or conda export: `--inherit-env 'LD_LIBRARY_PATH,NIX_*'`, `ROOMKIT_INHERIT_ENV`
(handy in an `.envrc`), or `inherit_env` under `[shell]` in the `--config`
file. The flag replaces the variable, which replaces the file; a resume command
repeats only the flag, so a resumed session reads the variable anew. A name
ending in `*` passes every variable with that prefix that is set when RoomKit
Code starts, except names that read as credentials (containing `KEY`, `TOKEN`,
`SECRET`, `PASSWORD`, `PASSWD` or `CREDENTIAL`, such as `ROOMKIT_API_KEY`); an
exact name passes as written. A lone `*` is refused. Each added variable is
readable by the agent's commands, and so by the model.

Persistent processes use pipes, not a PTY, and survive tool calls and profile
changes while the app runs. They default to a five-minute lifetime (up to one
hour), with eight retained jobs per session/agent and 32 total. Read stdout and
stderr with independent byte cursors (4 KiB pages per stream); `process_write` sends exact stdin text,
including any explicitly supplied newline, and can deliver EOF. Inspect partial
writes before deciding what to do next. Stop unneeded jobs and set `forget` to
release their retained records. The application closes remaining processes on exit,
but for an unattended launch (`--prompt`), which leaves them running.

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
**do not survive restarting the app**, even when present in saved history. A
later run gives the same IDs again, so an agent's first turn in a resumed
conversation is told that the application restarted and that the processes and
archives of before cannot be reached; an agent without those tools is told
nothing.

**Ask is the default.** Each `write`, `edit`, applied `patch`, `shell`,
`process_start`, `process_write`, `web_search` and `web_fetch` call waits for an
explicit decision. The panel shows exact arguments and the kind of action;
use PgUp/PgDn for longer details, **Ctrl+Y** to allow once or **Ctrl+N** to refuse.
Read tools, `diff`, patch previews, process status/output, the engine's stored
results (`read_stored_result`), plan and guide tools, skill activation, stopping
owned jobs and releasing owned archives do not need approval. Requests expire
after five minutes; disconnecting refuses pending actions. Actions too large to show in full
(64 KiB of approval detail) are refused and must be split.

Approval requests use a filled decision card. Use Up/Down and Enter or click
**Allow once** / **Deny**; Ctrl+Y and Ctrl+N remain available. PageUp/PageDown
and the mouse wheel scroll the exact details, and Esc interrupts the run. The
composer draft is preserved while the card is open. Commands show the command
itself and its remaining parameters. A new card accepts an answer only after it
has been on screen for half a second, so keys typed for the composer never answer
it. A terminal too small to show the card asks to be enlarged.

A confirmed decision adds a compact history receipt naming the person, agent
and action. It is saved when the turn settles and returns with `/resume`.
Timeouts, disconnections and automatic Yolo actions never claim that a person
approved or denied something. Approval remains distinct from tool success.

An agent may ask you questions with `ask_user` when a decision is yours: up
to four at once, each with up to four options, and always a last row for your
own words. The card shows after any approval. **↑/↓** or a digit picks,
**Space** toggles on a question taking several answers, **Enter** confirms and
goes to the next question, then sends; **Tab** and **Shift+Tab** move between
questions, **Esc** closes them unanswered and **Ctrl+C** interrupts the run. As
on an approval, keys count once the card has been on screen for half a second.
The agent reads one line per question (`Database: Postgres`); closed
unanswered, it is told to go on with its best judgement. Asking needs no
approval, in Plan too. A launch nobody watches (`--prompt`) offers no such
tool, and a delegated agent puts its question in its task's result instead.
Herdr shows the session as blocked while a question waits.

Press **Shift+Tab** in the composer to cycle **Ask → Plan → Yolo → Ask**,
including while an agent is working. While an approval card is open, Shift+Tab
does not change the mode: decide first. The status line shows the host-confirmed
mode. `/mode ask`, `/mode plan` and `/mode yolo` select directly; `/mode` offers
choices. Changing modes preserves the draft and never starts a model turn. An
agent that is working reads the new mode's instruction at its next step.
Actions already dispatched are not revoked. Yolo permits subsequent selected
tool calls without asking; tool path, revision, timeout and size checks still
apply.

Each conversation keeps its mode: it opens in the mode it was left in, Yolo
included, at the next launch as when you come back to it with `/resume`. A new
conversation starts in the launch's mode: Ask, unless `--mode plan`,
`--mode yolo` or `[permissions] mode = "plan"` (or `"yolo"`) in an explicitly
loaded TOML file selects another. `--mode` on the command line also opens an
existing conversation in that mode, which it then keeps. A launch without the
terminal interface uses its own mode and leaves the conversation's as it was.
Project files never implicitly select a mode.

## Plans and progress

Switch to **Plan** and ask the agent to prepare a plan. This mode allows project
exploration, skill reading, patch previews, reading existing process output and
stored results, the five plan tools and this guide's three documentation tools. Web research still requires approval. File mutations,
arbitrary commands, process start/input/stop, archive cleanup and all MCP calls
are blocked by the host, regardless of model instructions. MCP tools cannot yet
be classified reliably as read-only. Already running jobs continue; you can
still stop them explicitly from `/processes`.

The sidebar follows the first plan automatically. It shows its title, completed
step count, and up to three active or blocked steps; otherwise it shows the next
pending step. Click the section or use **/plans** to inspect all steps and the
objective. With several plans, open one and choose **Follow in sidebar**. The
panel and sidebar update when a tool changes the saved record, including while
the agent works. Each step has a colored status and a description preview of up
to three lines; the detail panel retains its full text. Statuses represent the agent's report, not proof
that tests ran. Cancelled steps are counted separately from completed steps.

Switch back to Ask or Yolo and explicitly request implementation when ready.
The same plan remains available, and the agent is instructed to update it as
work progresses. Changing modes, viewing a plan or selecting one to follow
never executes its steps. Plans are useful in Ask/Yolo too; Plan mode is an
execution restriction, not a separate kind of document.

An agent that can write plans can also ask for these switches itself. Before
substantial or ambiguous work, `plan_mode_enter` asks you to move the
conversation to Plan, with its reason; in Yolo it moves without asking, like
any selected tool. Once a plan is ready, `plan_mode_exit` shows you the stored
plan, its title, objective and steps, not the agent's summary of it; with
several plans the agent must name one. Approving moves the conversation to
**Ask**, where the agent implements that plan and each change still asks for
approval; a plan changed while you were deciding is not approved. Refusing
keeps Plan. The agent never moves to Yolo, and never leaves Plan unless you
approve.

Plans and sidebar selection survive `/resume` and application restarts. They are
scoped to the project, conversation and agent; another agent or conversation
has its own plans. Records live in the project's private application state under
`plans/`, outside the workspace and Git. The backend uses atomic replacement,
private permissions and revision checks under the conversation's exclusive storage
lease. Updates and deletes require the current revision; no-op updates retain it.
On an interrupted call with an unknown outcome, read/list before retrying.
Limits are 64 retained plans per conversation/agent, 64 steps per plan and 64 KiB
per plan (5 MiB per scope file). Reaching a limit fails explicitly; nothing is
silently evicted. Finishing a plan does not delete it. Removing the followed plan
clears selection rather than choosing a different plan automatically.

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

Agents hand tasks to each other through the engine's delegation
([Delegation](#delegation)). Inter-agent messaging, a Python execution
interpreter and plugin discovery are not selected.
Model messages use the CLI's Markdown renderer, including colored code blocks.
Set `ui.markdown`, `ui.message_gap` and `ui.tool_preview_lines` in explicit TOML
configuration to control formatting and density. Message source and stored
tool results remain unchanged.

Your submitted messages use the input background without an author heading;
other participants keep their names. Set `ui.highlight_own_messages = false`
to show author headings for all messages instead.

History lives in the platform's per-user application data directory, under
`projects/<workspace-path-hash>/history.sqlite3`. `--state-dir` or
`ROOMKIT_CODE_STATE_DIR` overrides that location. The canonical workspace path
determines the project identity: symlink aliases share history, moving a project
starts a separate history. SQLite is shared by local instances; each open
conversation has an exclusive process lock. Short locks protect initialization
and shared configuration/log writes. Files are created with private permissions
on Linux and macOS.

The engine owns persistence and bounded context restoration. The terminal
adapter displays up to 500 stored events; the current provider uses the engine's
bounded message/tool history, not the entire on-screen conversation. Model
changes apply to future turns. An agent whose model's window is known compacts
its conversation ([Compaction](#compaction)). A remote-engine transport is not
implemented in this version.

`code_outline` uses Tree-sitter to locate Rust/Python definitions, signatures and
line ranges before a targeted `read`. It does not resolve types or index an
entire project. Outline cards show source ranges, syntax warnings and pagination;
expansion reveals more symbols only when that result contains them.

## Changed files

The sidebar's **Files** section lists the files this conversation's agents
created, modified or deleted, newest first, followed by how many files they only
read. Click it or use **/files** for the complete list. `write`, `edit` and
`patch` report their own changes. For a `shell` command, the application compares
the project's files before and after it, honoring `.gitignore`, `.ignore` and
global excludes even outside a Git repository; such changes are marked
`· shell`. Files up to 1 MiB rewritten with identical content are not changes
once their content is known; each comparison reads at most 64 MiB of new
content, so a large project learns it over several commands. A command editing
an ignore rule does not list the files it hides or reveals. Anything that
changes while a command runs is credited to it, including a background process,
an editor or another instance; between commands, background processes are not
followed. A cancelled or timed-out command is still compared, and quitting waits
up to ten seconds for those comparisons. A file created and then deleted leaves
the list. The file tools stage each write in a `.roomkit-write-…` file beside
its destination, then rename it: a comparison never lists that file.

A project with more than 50,000 files is not compared: the section then says
shell changes are not tracked. The latest 500 changed and 500 read files are
kept, and `/files` says when older ones were dropped. The list is stored with
the conversation and returns with `/resume`; if a stored list cannot be read, it
is left untouched and the section shows only what happened since.

## Context, consumption and request failures

Context shares the model footer when it fits, and uses a separate row in narrow
windows. It shows the last completed generation's measured context: input tokens
(including cached input) plus output tokens. Set the agent's **Context capacity** in `/agents` to the actual server capacity to see used and remaining
percentages. Without capacity the token count remains visible; without reported
counts the context stays unknown unless an earlier measurement is available.
While awaiting a new report, that earlier context remains visible with the
explicit `last report` label. This is the last measured exchange, not a
prediction including an unsent message or new tool definitions.

The sidebar's **Context** section splits that measurement. A stacked bar and
its legend show what every request of the selected agent carries before its
messages: the **System prompt** (application and agent instructions), the **Instructions** each turn reads (the mode rule and the project's
`AGENTS.md`), the native **Tools** schemas, the **Skills** manifest and tools,
and one **MCP** part per server with its instructions and tool schemas; beyond
sixteen parts, the smallest servers are summed into one. **Messages** is what
remains of the measurement, including the instructions of skills activated in
the conversation. The parts are estimates, at four characters per token, marked
`≈`; when they exceed the measurement the section says so. A server whose
schemas fill the window shows up at once. While an agent's MCP tools wait behind
`find_tools`, their server parts count only its instructions, **Tools** counts
the two discovery tools, and the schemas of the tools it finds fall in
**Messages**.

The separate **turn** counter totals all reported input/output tokens across
that execution's model calls. Cached input is counted once. Missing measurements
or a failed generation mark totals partial. These are neither session-wide
billing totals nor dollar costs. Resume restores applicable measurements; a
new execution clears its turn totals. Changing the model or conversation clears
the previous-context fallback too.

The connection's **Timeout** in `/providers` bounds how long a model request
waits for the endpoint between two reads of its answer: an
answer that keeps streaming is never cut. Connecting is bounded at 5 seconds.
The initial TOML/CLI timeout, context and output-limit flags
remain available when bootstrapping a new project. The application defaults to 600 seconds;
the library's own default is unchanged. Chat Completions requests send no
temperature, so the endpoint's default applies. OpenAI-compatible connections
send no output limit by default; each model has its own. Set the agent's **Output budget (advanced)** in `/agents` to cap how many
tokens one reply may write; it is sent as `max_tokens`, or as
`max_completion_tokens` to the OpenAI models that take it, and zero is refused.
Leave that field empty for **Auto**. Anthropic starts with 32,000 tokens for
reasoning and the answer together, bounded by known model limits and estimated
remaining context. If it exhausts that budget before producing text or a tool
call, Auto can double it once per turn, up to 64,000 tokens. Later rounds retain
the larger budget. A numeric limit, including an existing 8192 setting, stays
fixed; explicit compaction budgets also remain fixed. Its prompt caching,
usage, model prices and context compaction use the Anthropic provider.
Anthropic tool rounds preserve their signed thinking and original tool inputs.
A confirmed signature-prefix mismatch resets the turn's reasoning before one
retry, preserving text, tool calls and results. Anthropic may report internally
folded tool-turn indices, so this recovery does not guess a local block from the
error path. For a cryptographically invalid signature with a localizable path,
recovery removes that block and subsequent reasoning. Only one recovery is
allowed per turn, before the rejected request emits visible output; removed
blocks stay out of later requests. Other providers keep their existing
model-specific reasoning replay policy.
Recoveries appear in `/diagnostics` without raw reasoning or signatures. The usage
of an exhausted attempt is retained there even if the retry fails before any
tool ran; in that failure case the engine does not publish the usage to the UI.
ChatGPT sign-in connections send no limit, whatever the setting. Failures
explain timeout, access, rate limit, server and protocol errors without echoing
raw provider responses or keys. Native Anthropic failures distinguish credit, quota,
overload, model access, context capacity and rejected output or reasoning settings.
A response containing only reasoning, with no answer or tool call, reports that
no visible answer was returned. Retry, or adjust the agent’s output limit and
reasoning effort as the message suggests.
A request that fails before its answer begins
is tried 3 more times, 1, 2 then 4 seconds apart, before the turn fails, when the
failure may pass: a lost connection, a timeout, a rate limit (429), a server
error (5xx), 408 or 409. A timeout waits the full **Timeout** again on each try.
A refused request (an invalid key, a malformed request) is not retried, nor an
answer that has begun streaming. The screen does not show these retries.
Existing completed tool actions remain in the project and conversation.
The agent's next turn is told that its previous one ended without a final
answer and how: on an error, interrupted, at the limit of tool rounds, cut at
the output limit. After an error, within the same run of the application, it
also reads the explanation you were shown, never the provider's own text.

A request the model refuses as past its window, when compaction cannot make
room, says so: the notice names the context limit and what frees room
(disconnecting tools, compacting the conversation or starting a new one, or a
model with more capacity). The sidebar shows `Refused: over capacity` above the
last measurement, and the footer `context refused`, until the next turn: that
request was never measured. The **Context** breakdown still shows what each
request carries, so a server whose schemas fill the window stands out.

### Compaction

An agent whose model's window is known compacts its conversation by tiers, so
that its requests stay within the window and the provider's prompt cache keeps
them. The window is the agent's **Context capacity** in `/agents`, or the window
the engine's catalogue lists for its model; when the catalogue says the model's
price rises past a number of input tokens (272,000 for `gpt-6-astra`), that
number is the ceiling instead.

While a request stays under 80 % of the ceiling, the conversation is read as it
is, every earlier round of tool calls whole. The request is estimated at four
characters per token, or taken as the provider measured the agent's latest one
plus what came since, when that says more. Past it, the agent's own model writes a summary of the oldest messages,
carrying over the previous summary, and the latest messages are kept whole within
30 % of the ceiling, whole turns only. The summary, followed by the project's
`AGENTS.md` snapshot, then opens every request, and the history grows again from
there, unchanged, up to the next tier: one cache loss per compaction. While the
summary is written, the status line says `Compacting the conversation… 4 s`; then
a line stays in the conversation, for instance `Conversation compacted · 42
messages summarized · ≈180k → ≈40k tokens · 12 s`. It is shown to you only, never
to the agent, and comes back in its place when the conversation is resumed. The
summary is kept in the project's state, so a resumed conversation reads the same
one. A compaction made between turns sends no request of the agent's, so nothing
measures what it left: the context gauge then shows the size the line tells, as
an estimate marked `~`, live and after a relaunch, until the agent's next turn
ends.

**/compact** compacts at once whatever the tier, keeping only the latest turns
whole, four messages at least.
Sent while the agent works, it is queued and runs as soon as the turn ends; a
message you send while the summary is written waits for it, then the agent reads
the compacted conversation. A compaction only happens between turns: a turn whose
tool calls pass the window falls back on the engine's own compaction.

The summary reads the messages and, for each tool call of the turns it stands
for, one line saying what the call was and how it ended, such as
`tool: Run cargo test → exit_code=0`; never a call's result. It reads the
newest lines first, within half the ceiling. Beside what the model wrote, the
summary carries what the application recorded when it was written: the agent's
plans in the conversation, with their progress, and the files the conversation
changed, 40 named at most: those its file tools changed first, then, on a line
of their own, those found only around shell commands, such as the files of the
commits a merge brings. The summary's request states no output budget of its
own: it takes the
agent's, as its turns do, so a model that reasons has room for its reasoning and
for the summary, which it is asked to keep under 800 words. A summary cut at
that budget, or longer than about 3,000 tokens, is kept without its end: it says
so to the agent, and the status line and the conversation's line say so to you.
A summary the model does not write compacts nothing:
the status line says so and the next turn tries again. The summary's request is
not counted in the turn's tokens or the conversation's cost.

An agent without a known window reads its latest 50 messages instead, as before,
and each of its turns carries the project's `AGENTS.md` snapshot: a request past
the model's window is compacted by the engine, which keeps only the turn's input
whole. Since the engine replays each turn's instruction, its requests then hold
one copy of the snapshot per turn in that window; set the agent's **Context
capacity** to avoid it.

## Boundaries

| Repository | Responsibility |
| --- | --- |
| [roomkit-code](https://github.com/roomkit-live/roomkit-code) | Development instructions, selected tools, workspace, configuration, lifecycle |
| [roomkit-tui](https://github.com/roomkit-live/roomkit-tui) | Generic terminal interface, client protocol, local engine adapter |
| [roomkit-rs](https://github.com/roomkit-live/roomkit-rs) | Conversation engine, agents, providers, MCP client, policies, persistence |
| [roomkit-tools](https://github.com/roomkit-live/roomkit-tools) | Independently reusable tools and their RoomKit adapters |
| [roomkit-auth](https://github.com/roomkit-live/roomkit-auth) | OAuth, account identity, credential storage and renewal |

`src/agent/` selects capabilities and builds each agent's system prompt from
`src/agent/coding-agent.txt`, measuring what every request carries.
`src/permissions/` decides, from one table, what each tool may do in each mode,
and owns the hook that awaits actual approval.
`src/timeouts.rs` derives the host's deadlines from the limits they cover.
`src/application.rs` assembles and closes the engine.
`src/config.rs` and `src/storage.rs` own application settings and project state.
`src/processes.rs` owns scoped process monitoring and human controls.
`src/delegation/` serves the engine's delegation, records the tasks it runs
and owns `/tasks`.
`src/files.rs` records the files each conversation's agents change or read.
`src/plans/` supplies durable storage through `roomkit-tools-plans::PlanBackend`
and projects those records into generic CLI panels and sidebar entries. The
CRUD schemas and tool implementations remain in `roomkit-tools`.
`src/accounts.rs` owns account menus and browser launching; `src/profiles/` links
accounts, providers and agents without exposing credentials to the UI.
`src/profiles/mcp/` owns MCP configuration and exposure: it names each selected
tool `mcp__<server>__<tool>`, serves only the selected ones, and closes the
servers; the engine's `roomkit-mcp` crate owns protocol calls and each stdio
server's process group.
`src/sessions/` routes application commands and owns the project session catalogue.
It keeps each conversation's agent as the active agent of the room's RoomKit
conversation state, which the engine stores in the room's metadata: an
orchestration strategy installed on a room reads and writes the same record.
The conversation's mode, and the model and effort each agent runs with there,
are the application's own record, in the room's private state.
The TUI receives only its generic client connection, preserving the boundary
needed for a future remote host.

## Development

```sh
make check
make msrv
make audit # requires cargo-deny
```

Build and install from source:

```sh
make build      # production binary: target/release/roomkit-code
make debug      # development binary: target/debug/roomkit-code
make install    # build, then install it as a managed binary in ~/.local/bin
make uninstall  # remove that managed installation; application data is kept
make clean      # remove build outputs
```

`BIN_DIR=/your/bin` selects another directory for `install` and `uninstall`.
`make install` uses the release installer's managed layout: it never replaces an
unrelated `roomkit-code`, and keeps the previous binary for `--rollback`. Like a
release binary, it checks for newer stable releases, which `/update` can install.
`make uninstall` removes every managed version in that directory, and refuses to
touch an unrelated `roomkit-code`.

Tests use temporary projects, local HTTP fixtures and a stdio MCP fixture, with
no model service or credentials. They exercise real file tools, streaming, persistence and terminal
cleanup. See [CONTRIBUTING.md](CONTRIBUTING.md) for dependency and CI setup.

To opt into a paid Anthropic check, build with `cargo build --locked`, set
`ANTHROPIC_API_KEY`, and run `python3 scripts/check-anthropic-live.py`. It checks
native launch, file tools and session resume in a disposable project, then
removes its stored credential. `ANTHROPIC_TEST_MODEL` overrides its default
`claude-sonnet-5-5` model. This check is excluded from the normal gates and CI.

MIT licensed. This is an initial development version, not a published release.

## OpenCode Go

Open `/providers`, choose **Connect OpenCode Go**, and paste the API key from
your [OpenCode console](https://opencode.ai/console). The connection calls
`https://opencode.ai/inference/go/openai/v1` and nowhere else. Once saved, it
lists the paid models Go serves over Chat Completions; choose one to create an
agent. **+ Add agent** and **Change model** in `/agents`, and `/model`, list them
too. Free models answer only OpenCode's own client, and the
models Go serves over the Responses, Anthropic or Gemini APIs are not offered yet.

Every request carries the agent's cache key, the same for that agent in this
workspace, in the `x-opencode-session` header Go requires, and names RoomKit Code
as its client. The key
is stored like any other connection's key, in a private local file (0600);
editing the connection with an empty key field keeps it.

Some models, such as DeepSeek's, are hosted outside Europe and the United States.
By default OpenCode refuses them with an HTTP 400, which RoomKit Code reports as
a rejected request: select **Global** in the console's privacy settings to use
them. Usage counts against your Go plan's limits.

## Sign in with ChatGPT

Open `/providers`, choose **Sign in with ChatGPT**, then **Connect a ChatGPT
account**. The same screen shows a link to open in your browser and a masked
field for the return URL. This works with a browser on the same computer or
another one, including when RoomKit Code runs over SSH. In a compatible terminal,
Cmd-click the link on macOS or Ctrl-click on Linux to open your local browser.
The complete URL remains available to copy.

After consent, copy the complete return URL from the browser's address bar,
paste it into the field, and press Ctrl+S. If the browser shows a connection
error at `127.0.0.1`, copy that address anyway. A browser on the same computer
can also complete sign-in automatically. Esc cancels the attempt; it expires
after ten minutes. No port forwarding or copying of access tokens is needed.
Choose a model offered by the connected account, save the agent, and use
`/agents` to switch interlocutors.

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

Use `/effort` to select the reasoning effort the active agent runs with in the
conversation you are in, which keeps it; **Reasoning effort** in `/agents` sets
the agent's own, which a new conversation starts with. It applies to subsequent
requests, including tool rounds. `default` leaves the parameter out; other levels
must be supported by the chosen model. On OpenAI's own endpoint, some models take
the effort RoomKit's catalogue states on a turn with tools, whatever this setting
says. Changing effort keeps the current session. For a new project,
`--reasoning-effort` or `[provider] reasoning_effort` sets the first agent's
effort, as `--model` sets its model; an unattended run uses it to pin the effort.

`/diagnostics` shows recent operational records and the exact log location. Each
project's private state directory contains `diagnostics.jsonl` and, after rotation,
`diagnostics.previous.jsonl` (at most 1 MiB each). Records include an instance ID;
concurrent writers coordinate append and rotation. Startup records identify the
version, target and managed binary digest. Anthropic requests record start and end,
turn/request identifiers, effective output budget, effort, content-block counts,
duration, stop reason and numeric usage. Failed requests also record the HTTP
status, reasoning-error category, numeric block path, first reported mismatch path
when available, and exact recovery decision (for example `unsupported_error`, `target_not_thinking`, `already_repaired` or
`response_started`). Raw and combined-role target positions help diagnose API
validation differences; prefix-mismatch recovery resets reasoning without using
these positions. A successful recovery records whether it reset the turn's
reasoning or removed a suffix. Dropping an active stream records cancellation. These records do not require `--debug`.
The panel shows the current
instance's recent activity. Errors and tool outcomes are
recorded automatically using RoomKit's `ON_ERROR`, `ON_TOOL_CALL` and framework
events. Run `roomkit-code --debug` for additional lifecycle and delivery records.
Records include timestamps, room/event/channel identifiers, known tool names,
outcomes, safe error classifications and HTTP status codes when present. They
never include prompts, tool arguments/results, credentials, encrypted reasoning
or raw provider error bodies.
The panel shows recent records for the current launch; files also cover previous
launches. File write failures are shown in the panel and do not stop the agent.
An internal error (a panic) is recorded with where it happened and its kind of
failure, never its message, which can quote your text; the session goes on, and
the exit report says how many occurred and where the log is.
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
unavailable-image marker. This applies to native Anthropic Messages and OpenAI-compatible Chat Completions
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

### Writing a skill

A portable `SKILL.md` starts with YAML frontmatter delimited by `---` on its own
lines. Set `name` to the directory name (lowercase letters and digits separated
by hyphens, at most 64 characters) and `description` to what the skill does and
when to use it (at most 1024 characters), then write Markdown instructions.
`license` and `compatibility` are optional.

Put supplementary text directly in `references/<filename>`:
`read_skill_reference` takes the skill name and that plain filename. Not
supported here: nested references, tool-grant fields (`allowed_tools`,
`allowed-tools`, `requires`), other assistants' invocation fields
(`disable-model-invocation`, `user-invocable`, `context`, `agent`) and
`agents/openai.yaml` sidecars. Adapting an existing skill must not silently drop
its restrictions. Bundled scripts are not run automatically: an agent runs them
only with an available tool and the host's permission, from the skill package
directory.

A plain `skills/` directory at the workspace root and other assistants'
directories such as `~/.claude/skills` are not scanned; never guess another
installation directory, suggest moving a project skill to another assistant's
configuration, or edit the application's profile store to bypass this
selection. Creating a file does not prove a skill was discovered, enabled or
activated, each is a separate step. `/skills` lists skills and loading errors,
`/skills locations` the roots, `/skills reload` rereads them, `/skills enable
<name>` enables one for the selected agent and `/skills use <name> <task>` asks
for its use in the conversation. Enabling a skill grants no extra tools and does
not change Ask, Plan or Yolo. Without `activate_skill`, no skill is available to
the agent yet: that says nothing of what the application can manage.

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
execution tools subject to Ask/Plan/Yolo, with relative paths based on the package
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
read-only. **Plan** blocks MCP calls. **Yolo** executes selected tools without asking. Server instructions
are framed as external content by the engine and grant no permission. Tool
results use the CLI's generic expandable presentation. An MCP call that gets no
answer within 30 seconds fails: the engine's fixed bound, as in RoomKit's Python
implementation. **Esc** requests MCP
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

An agent's native tools are always declared to the model. Its selected MCP tools
are too, unless they would cost more than 8,000 tokens of schemas, more than 10%
of its **Context capacity** or, with no capacity set, count more than 20 tools.
Past that, they wait behind `find_tools` and `list_tools`: the model searches,
and the last twelve tools it found or called stay declared for its next turns. A
tool it found but never called is searched again after the agent is rebuilt, as
when a server connects. A search needs no approval; the tool it finds follows the
same rules as any MCP tool.

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

## Session sidebar

On wide terminals, a right sidebar displays the selected agent/model and provider
connection, context occupancy split by what fills it, the followed plan, the
files this conversation changed, configured MCP servers and running processes.
It uses the reusable sidebar protocol from roomkit-tui; this application supplies
its plan, file, MCP and process sections. Provider credentials and
endpoint settings are never included in the sidebar. Agent/model/effort appear
only in the sidebar while it is open; the mode and shortcuts share one line below
the composer when space permits. Shift+Tab still changes the mode without a
permanent shortcut label. Hiding the sidebar restores the complete footer.

F7 hides/shows it; F8 focuses its actions. Up/Down or Tab chooses an action,
Enter opens the existing detail/configuration view, and Escape returns to the
composer. Click an MCP server to inspect its tools, or a process to inspect its
output and stop it through the existing controls. Process exits remove entries
automatically. Scrolling over the sidebar leaves conversation history in place.

Configure `[ui] sidebar = false` or `sidebar_width = 34` in the explicit local
TOML file. Below 112 columns (or when fewer than 72 columns would remain for the
conversation), the sidebar hides and usage returns to the footer. Forms, menus
that require focus, and approval views retain their normal interactions.
No background refresh redraws an unchanged screen. MCP liveness is checked
every two seconds while servers are configured; process changes use the existing
lifecycle observer. Plans refresh from committed storage changes, without polling. Clicking configuration during a running turn remains subject
to the host's existing busy-state rules.

## Welcome screen

New empty conversations open with a centered RoomKit wordmark and composer.
One compact footer shows the mode, agent, model, effort and command shortcut.
It omits the technical provider protocol, project path and session identifier;
menus and command input replace it with contextual keyboard help.
Typing `/` still opens commands; multiline input, image attachments and Ask/Plan/Yolo
keep their normal behavior. The first exchange displays the conversation and
sidebar. `/new` restores welcome; `/resume` opens the selected history. Forms,
approvals and errors remain visible in their usual views.

The interface leaves one blank line at the bottom by default. Customize the
explicit TOML configuration with `[ui] welcome = false`, `welcome_width = 88`,
`bottom_margin = 1`, `welcome_logo = [...]` and `welcome_wordmark = [...]`.
`--no-logo` hides both welcome graphics and the compact header mark. Custom
application names fall back to plain lettering unless a wordmark is supplied.
Small windows use the regular conversation layout without discarding input.

The default mark is RoomKit's four blocks in three violets, the one the RoomKit
console draws: two columns of two rows in the header, larger on the welcome
screen. A custom `logo` is drawn as plain text in the theme's colors.
The welcome wordmark is generated from the website's Space Grotesk 600 font, in
Unicode quadrant blocks, with no image protocol or runtime font dependency. To
regenerate it during development, install Pillow in a local Python environment,
then run:

```sh
python scripts/generate-branding.py --font /path/to/space-grotesk-600-latin.woff2
cargo fmt --all
```

The generated Rust strings live in `src/branding.rs`; Python is not required to
build or run the application.

Tool activity is shown as a short public sentence in the conversation's language,
with a formatted preview of the current call below it. Consecutive calls replace
both the heading and preview, using the existing `ui.tool_preview_lines` limit;
an agent message starts a new sequence. F2 or a click opens the technical history,
and F2 returns to the conversation view. Approvals and unsuccessful outcomes remain
visible. Set `[ui] tool_activity = false` to keep individual tool cards instead.
The engine requests the intention with the ordinary tool call, without another
model request; the tools themselves receive only their business arguments.
The sentence is asked for on a shell command, a background process, a delegated
task and an MCP tool, whose arguments do not say what they are for, and agent
instructions have it carry routine progress, reserving assistant messages for
findings and explanations. Other calls read from their arguments (`Read src/main.rs`,
in the interface's language), or as the tool's name when the interface has no
reading for it. When the model supplies no intention on a call that asks for one,
the tool's name is shown.

RoomKit Code requires `_roomkit_intent` in those tools' schemas by default.
Set `[agent] require_tool_intent = false` in the explicit TOML configuration to
make it optional. `--require-tool-intent` and `--require-tool-intent=false` override
the file setting. The option applies to all agents in that application
instance, including after profile changes. It does not change tool-owned parameters
of the same name. Providers may ignore schema requirements: a missing intention
still uses the tool name, without blocking execution or triggering a retry.

## Command hooks

`/hooks` discovers global definitions in `<state-dir>/hooks.json` and project
ones in `.roomkit/hooks.json`. Both sources are additive, global first. No command
runs until you inspect it and choose **Enable this hook** in `/hooks`. Approval is
stored privately for this project and exact definition. Use **Reload configuration**
after editing; new or changed definitions need review again. Referenced script
contents are not pinned: only enable commands and scripts you maintain or trust.
Existing `.claude` and `.codex` configurations are not imported automatically.

For example, put this in `.roomkit/hooks.json`:

```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "^(write|edit|patch)$",
      "hooks": [{
        "type": "command",
        "command": "python3 .roomkit/hooks/review.py",
        "timeout": 10,
        "statusMessage": "Check proposed changes"
      }]
    }]
  }
}
```

Then create `.roomkit/hooks/review.py`:

```python
import json
import sys

request = json.load(sys.stdin)
print(json.dumps({
    "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "ask",
        "permissionDecisionReason": "Review this proposed change."
    }
}))
```

The hook requests human review even in Yolo. `deny` refuses the action; `allow`
continues the application's normal policy, including any approval it requires.
Hooks cannot override Plan restrictions. A `UserPromptSubmit` hook can instead
return `{"decision":"block","reason":"Explain what is missing"}`. Exit code 2
also blocks these two events, using stderr as the reason. Other nonzero exits,
timeouts, malformed output and unsupported decisions fail closed before input or
a tool. Successful stdout must be empty or supported JSON; use stderr for logs.
Observation failures are reported but do not undo an action that already happened.

| Event | Application behavior |
| --- | --- |
| `SessionStart` | First authorized attachment to a session in this application execution; `source` is `startup`, `new` or `resume`. |
| `SessionEnd` | Application shutdown, for each session opened in this execution; `reason` is `shutdown`. |
| `UserPromptSubmit` | Validated text/image prompt before entering engine history; `prompt` contains text, never image bytes or attachment references. |
| `TurnStart` | One accepted logical agent turn, before the first model request. |
| `TurnEnd` | After delivery, cancellation cleanup and the turn's tool hooks already started; `roomkit.outcome` is `completed`, `failed`, `cancelled` or `blocked`. |
| `PreToolUse` | Before an eligible agent tool, including MCP, and before the application's approval. Hard Plan restrictions take precedence. |
| `PostToolUse` | A successful tool outcome, observed asynchronously; `roomkit.outcome` is `served` and the tool result remains unchanged. |
| `PostToolUseFailure` | A refused, failed, blocked, unhandled or cancelled tool; `roomkit.outcome` is `refused`, `cancelled` or `failed`, which also covers a result a hook withheld and a call nothing served. |

RoomKit reports tool outcomes in the background. A `PostToolUse` or
`PostToolUseFailure` hook normally runs before its turn's `TurnEnd`, which waits
for those already started. An outcome reported late, such as a call cut at the
very end of an interruption, may run its hook after `TurnEnd`, or with the
agent's next turn in that conversation once that turn has begun, or not at all.

A message written while the agent works passes `UserPromptSubmit`. Sent to the
running turn, it starts no turn: it has no `TurnStart` or `TurnEnd`, and the
running turn's hooks cover the calls it leads to. Given a turn of its own, that
turn runs `TurnStart`, its tool hooks and `TurnEnd` as any turn. A turn that Ctrl+S
starts to answer such a message runs them too, under its own `turn_id`, with no
`UserPromptSubmit`: no new words came with it.

A session remains open when switching views or agents; those actions do not fire
`SessionEnd`. Returning to a session in the same execution does not fire another
`SessionStart`. Reopening it after application restart opens a new execution.
Enabling a hook never replays old events, including an already-fired session start.
Abrupt process termination cannot guarantee cleanup hooks. `TurnStart`/`TurnEnd`
are RoomKit extensions, not aliases for Claude/Codex `Stop` continuation.
Manual `!` commands are user actions and do not fire agent-turn or tool hooks.

Each command receives one JSON object on stdin, including `hook_event_name`,
`session_id`, `cwd`, `permission_mode` and a `roomkit` object with `version: 1`,
`execution_id`, `agent_id`, `origin` and, where applicable, `turn_id` and `hook_id`.
Tool events additionally carry `tool_name`, `tool_use_id`, `tool_input`, and after
execution `tool_response`. Matchers are case-sensitive regexes over canonical tool
names (`shell`, not `Bash`), or session source/end reason. Empty or `*` matches all.
Prompt and turn events accept only empty/`*` matchers. No payload translation for
another application's tool schemas is implied.

Handlers within an event run sequentially; a refusal stops a blocking chain.
Independent tool outcomes may be observed concurrently. Configuration is fixed for
a turn, and changing enabled hooks is refused while it runs. In Plan mode, commands
are skipped unless their definition explicitly includes
`"roomkit": {"run_in_plan": true}`. That option allows the script itself to run with
your user permissions; it is not a read-only sandbox. Commands inherit the host's
environment and run at the project root, also exposed as `ROOMKIT_PROJECT_DIR`.

The reusable `crates/roomkit-hooks` component bounds stdin to 1 MiB, each output
stream to 32 KiB and each command to 1–30 seconds (default 10). Each source allows
at most 60 seconds of handlers per event. Timeout, cancellation and normal exit
clean up the command's Unix process group, including remaining background members.

The sidebar shows activity and the latest result; `/hooks` shows bounded recent
output for explicit inspection. Private diagnostic logs store correlated identifiers,
status and duration, not commands, prompts, tool arguments/results or script output.
The last 100 executions are retained in memory; raw output is not restored on resume.

This first version deliberately rejects unsupported events/fields: `Stop`
continuation, `PermissionRequest` auto-approval, input rewriting, context injection,
asynchronous command mode, HTTP/MCP handlers and prompt/agent handlers come later.

## Project instructions: AGENTS.md

RoomKit Code automatically supplies global and project instructions to the
selected agent. Create `AGENTS.md` at the workspace root for shared conventions,
and place another in a subdirectory for rules scoped to that directory and its
descendants. More specific rules take precedence on conflicts; direct user
requests and application permissions remain authoritative.

Discovery order is:

1. `<state-dir>/AGENTS.override.md`, or `<state-dir>/AGENTS.md`.
2. `<workspace>/AGENTS.override.md`, or `<workspace>/AGENTS.md`.
3. The corresponding files along an explored path, from parent to child.

A nonempty `AGENTS.override.md` replaces `AGENTS.md` in the same directory;
an empty override falls back to `AGENTS.md`. The explicitly selected workspace
is the boundary, even when it is inside a larger Git repository. Ancestors above
that boundary, sibling directories and other assistants' configuration directories
are not searched. Names are case-sensitive; `CLAUDE.md`, `AGENTS.local.md` and
remote includes are not aliases in this version.

Global and root files are read before each agent turn, including after resume
or switching agents. They enter an application instruction after the user's
message, preserving the system prompt and the user's original text and images.
The engine keeps each turn's instruction as the agent's own row and replays it in
the agent's later turns where it was read, so that a request extends the one
before it and a provider's prompt cache keeps it. A turn of an agent whose
conversation compacts therefore carries the mode rule each time, but the files'
snapshot only when it changed since the latest one the agent still reads, which a
compaction's summary carries; a newer snapshot supersedes the earlier ones. An
agent reading its latest 50 messages carries the snapshot every turn (see
[Compaction](#compaction)). The host does not
rewrite earlier conversation rows. Nested rules are loaded
progressively alongside `read`, `code_outline`, `list`, `grep`, `glob` and `diff`.
Only the supplied target paths' ancestor chains are inspected, not every search
match or every directory under the project. A `patch` dry-run also loads scopes
for every source and destination, including both sides of moves.

Exploration results carry a `project_instructions` field beside the tool's text
when a rule or a loading error applies along the explored path. Directories
without rules add nothing, and the tool's text reaches the model as it is.
Unchanged scope snapshots are deduplicated within a turn, separately for each
session and agent; each new turn carries them again. Changed rules are reported
on the next exploration, and so are removed ones, in a later turn too: the agent
is told once that the rules it was given no longer apply. The conversation keeps,
for each agent, the directories whose rules it was given, so a conversation
resumed after a restart takes back the rules removed meanwhile and adds nothing
elsewhere. A conversation from before that record may hold rules removed since:
each directory's first exploration reports once, even when it finds none, over
all its later launches. So does a conversation whose record cannot be read, which
`/diagnostics` notes as `instruction_record_not_read`; a record that cannot be
saved is noted as `instruction_record_not_kept` and saved again with the next
change or turn. Both name the room, the agent and whether the store or the
record's form failed, never the error's text. The agent is instructed to read existing
files, inspect the nearest existing parent before creating files, and preview
patches before applying them. These are model instructions, not an enforced
proof of compliance: an arbitrary shell command, running process or remote MCP
tool can affect paths the application cannot infer. The agent must inspect those
scopes first. No file instruction grants tools, changes Ask/Plan/Yolo or executes
a hook automatically.

Use **/instructions** to inspect global/root sources, precedence and loading
errors, or **/instructions src/api/file.rs** for one file or directory. Paths
are workspace-relative; spaces are accepted without shell quoting. The panel
reads current files and does not modify a running turn. No reload command or
restart is needed; edits to global/root instructions take effect on the next
turn, while nested edits are picked up on the next exploration of their scope.

Files must be regular UTF-8 files, at most 16 KiB each, with a 32 KiB budget per
loaded chain (including source metadata). Oversized or unreadable files produce
diagnostics instead of silently truncated rules. A failed override does not
silently fall back. Symlinks below the configured roots are not followed, nor
are `..`, absolute target paths, or paths deeper than 32 components. Content is
sent to the configured model, and nested snapshots are retained with tool results
in the session history; keep credentials out of instruction files.


## Binary installation and updates

Releases provide Linux x86-64/ARM64 (glibc 2.35 or newer) and macOS Intel/Apple
Silicon binaries (macOS 12 or newer). Rust and Cargo are not needed to use them.
The release workflow validates each native platform before publishing all four
binaries together. macOS builds have an ad-hoc signature, not an Apple Developer
ID signature or notarization. Distribution builds do not contain local profiles,
provider credentials, conversations or private planning files.

While this repository is private, use GitHub CLI with repository **Contents:
read** access. Download the installer from the release before running it:

```sh
gh auth login
gh release download --repo roomkit-live/roomkit-code --pattern install.sh --dir /tmp/roomkit-code-install
sh /tmp/roomkit-code-install/install.sh
```

For a public release, the installer can also download anonymously with curl.
A specific stable version can be selected with `sh install.sh 0.1.0`. The
installer uses `sha256sum` on Linux or `shasum` on macOS, verifies the binary's
SHA-256 and version, and installs in `~/.local/bin`. Override this with
`ROOMKIT_CODE_BIN_DIR=/your/bin`. It never invokes sudo or edits shell profiles;
add the displayed directory to PATH if necessary. An existing unrelated binary
is not overwritten. Checksums detect corruption; they are published in the same
GitHub release and are not an independent signature or proof of authorship.

Start `roomkit-code /path/to/project`. Configure a provider with `/providers`
and an agent with `/agents`. The welcome screen shows the running version.
Managed distribution installations check GitHub at most once per day in the
background, with a local cache; failures do not prevent conversations. No project,
conversation or provider information is sent. `/update` checks immediately and
presents the exact version, platform and destination before confirmation.
Installation never restarts or interrupts a conversation. Exit and relaunch to
use the new binary. `/update rollback` reviews and restores the previous binary.

Equivalent terminal commands:

```sh
roomkit-code --version
roomkit-code --update-check
roomkit-code --self-update
roomkit-code --rollback
```

Use `--yes` to confirm an explicit self-update/rollback without a prompt.
Disable automatic checks with `--no-update-check`, `ROOMKIT_NO_UPDATE_CHECK=true`,
or `[updates] check = false` in an explicitly selected TOML config. Manual
`/update` remains available. Private downloads reuse `gh auth token` locally, or
use `ROOMKIT_UPDATE_TOKEN` with Contents: read. Tokens are not saved by the
updater, sent to providers, or forwarded to the redirected asset host. The
shell installer requires `gh` for private downloads; it accepts the same token.

The managed entry point is a symlink to `.roomkit-code/current/roomkit-code`
inside the chosen bin directory. Version directories are immutable; activation
uses an atomic symlink replacement and an installation lock. The previous pointer
allows rollback. Each update removes every version other than the current and
previous ones, so the installation holds two binaries at most; a process still running an
older one keeps its open binary until it exits. Do not edit managed files
manually. Cargo/source builds and package-manager binaries
are never self-modified; `/update` explains how to install a managed release.
Rollback restores the binary only: it preserves application state and cannot undo
an incompatible data migration in a future release. Back up state before such a
migration as directed by that release's notes.

### Publishing a release

Bump the application version and lockfile, update the changelog, run the required
checks, and push the source to main. Tag that reviewed commit with `vX.Y.Z`, matching
Cargo.toml. The `Release` workflow runs checks, terminal scenarios, minimum Rust
and the installer on all four native platforms, plus dependency audit, before
building/publishing. The assets are raw executables named
`roomkit-code-vX.Y.Z-<target>`, individual `.sha256` files, installer and license.
An existing release is never overwritten. A workflow stopped after draft creation
leaves a draft, which must be inspected before finishing publication. It is never
visible to automatic update checks until published.

Private dependencies require `ROOMKIT_DEPS_READ_CI` as for CI. Release jobs use
the workflow token only to publish this repository's release. Failed or unavailable
runners leave the release unpublished; rerun the workflow for the same existing
tag after correcting the infrastructure. The updater follows stable releases only.
