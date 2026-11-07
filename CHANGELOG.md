# Changelog

## Unreleased

- Follows roomkit-rs 3b885524, roomkit-tui 4eed1d9 and roomkit-tools 5d05c29
  (RMK-1460). A later turn's first request now opens with the previous turn's last
  one: every earlier round of tool calls replays whole, as its turn sent it
  (RMK-1370), and each turn's application instruction replays where the turn read
  it (RMK-1407).

- Follows roomkit-rs ee5d724, roomkit-tui cf0840e and roomkit-tools c7be169
  (RMK-1460). A `<think>` tag in the middle of an answer no longer swallows the rest
  of it (RMK-1327); a fenced block's closing tag is read whatever its separator
  (RMK-1320); compacting memories leave the turn its room (RMK-450). A tool's
  outcome past 5,000 estimated tokens reaches the model as a preview, stored whole
  for the engine's `read_stored_result`, which every agent now declares and the
  context breakdown counts (RMK-1228).

- A ChatGPT account's requests carry a prompt cache key, one per agent and
  workspace, so that OpenAI routes them to one prompt cache: in a conversation of
  43 turns, a turn's last request, which extends the one before it, read 13% of
  its input from the cache (RMK-1481).

- The footer and the sidebar show what a turn read from the prompt cache and wrote
  there, and the conversation's cost at its model's list price: `≈ $0.42`, every
  turn of the conversation, earlier sessions included; the exit summary ends with
  it too. The kit's catalogue prices OpenAI's models; `[provider.pricing]` prices
  the configured server's model, per million tokens (roomkit-tui c879190; RMK-1461).

- A ChatGPT account shows its model's API list price too: what the conversation
  would cost per token, though a plan bills it (RMK-1461).

- The exit writes how to reopen the conversation, its usage and its cost before
  closing begins, then the processes it stopped: a close that took long, or that a
  second Ctrl+C cut, printed nothing at all. Each step of the close is recorded in
  the diagnostics, so that one that never ends shows where (RMK-1478).

- Follows roomkit-rs f86cbb1, roomkit-tui 23b1e6a, roomkit-tools 5d9096c and
  roomkit-auth 3bf770c (RMK-1460). A prompt's cache writes, which the kit now
  reports apart from its input, count in the exit summary's input, the turn's
  totals and the context counter (RMK-1457).

- A first turn with no text at all, stopped while its call runs, keeps its measure
  after a resume, where it read `context ?`, and the exit summary counts it: its
  record rides its last row (roomkit-rs 6f94ec6, roomkit-tools 3771f26, roomkit-tui
  7319908).

- The model's reasoning stays out of its requests on Chat Completions, which keeps a
  server's prompt cache from one step of a turn to the next: `roomkit-rs` no longer
  sends it back as `<think>` (RMK-1369).

- The context counter follows a long turn step by step, and keeps its last measure
  after Esc, on the first turn too and after a resume; the turn's totals and the
  exit summary count a stopped turn, `(partial)` when a generation was still running
  (roomkit-rs dcaf505, roomkit-tools 46713bd, roomkit-tui 9388992).

- A click on the model's reasoning opens that block alone, where it opened every
  call below it too (roomkit-tui c6c066e).

- MCP tools past 8,000 tokens of schemas, 10% of the agent's context capacity or,
  with no capacity set, 20 tools wait behind `find_tools` and `list_tools`, so a
  heavy server no longer fills the window before the first message. Native tools
  stay declared, and the context breakdown counts what a request carries.

- The model's reasoning shows as it comes, then folds to one line that F2 or a click
  opens, with the technical history. It is not stored.

- Follows roomkit-rs c30375c, roomkit-tui b489546 and roomkit-tools 23b25be. An agent keeps telling the model of its native tools,
  and a chosen reasoning effort still reaches its tool turns behind a `base_url`,
  as the kit's new defaults would hide the first and drop the second. On OpenAI's
  own endpoint, the kit now sends the effort its catalogue states for a tagged
  model on a turn with tools, whatever `/effort` says.

- A message sent to the running turn stays below what the turn writes without
  it, and its line says `read by the turn` once the turn passed the step that
  reads it (roomkit-tui 64236ec). The agent's preamble now asks it to take up such a
  message in its answer, and names the mark of the application's instructions,
  which a model took for a prompt injection and did not follow, leaving the mode
  and the project's instructions unread. A project's file can no longer close its
  `<instructions>` block and speak as the application from outside it.

- Cut a turn for a message already sent to it: the message's line says
  `Ctrl+S to cut it now` while the turn runs. Ctrl+S on an empty composer cuts the
  turn, and a turn of its own answers, with the mode's and the project's
  instructions, `TurnStart` and `TurnEnd`, and no `UserPromptSubmit`. A message
  written while the model answers now stays below what it had written
  (roomkit-tui 5669b0f).

- A mode changed with `/mode` or Shift+Tab while the agent works reaches its
  running turn, which reads the new instruction at its next step. It kept
  working to the old one while the permissions followed the new one.

- Send a message now: Ctrl+S, or Ctrl+Enter where the terminal reports it,
  interrupts whatever runs and sends the message once nothing runs any more
  (roomkit-tui, roomkit-rs 9830bc4, roomkit-tools 0.1.0-alpha.16). In terminals
  with the kitty keyboard protocol, Shift+Enter inserts a line break, and
  Ctrl+[, Ctrl+M and Ctrl+I no longer stand for Esc, Enter and Tab.

- Write to the agent while it works: Enter sends the message to the running
  turn, which reads it at its next step, or gives it a turn of its own, with
  its own `TurnStart` and `TurnEnd`, when the turn's last answer is already
  complete (roomkit-tui 48054c9).

- Refuse a `timeout_ms` above the maximum, 120000 ms for `shell` and one hour
  for `process_start`, with a message that gives the maximum, instead of
  shortening it. In Ask mode the refusal comes after you approve the call: the
  card shows it denied, hooks get `PostToolUseFailure` with the outcome
  `refused`, and the model can retry with a smaller value (roomkit-tools
  6b59222).

- Change tool results as roomkit-tools 6b59222 does, for the model and for
  hooks, which receive them as `tool_response`: `process_status` returns
  `{"processes": [...]}` instead of a bare list; `shell` and process results
  give their `cwd` relative to the project, `.` for its root, as the process
  monitor now shows it; `docs_list` and `docs_search` offsets count from 1,
  and an offset of 0 or past the last result is refused. `process_status`
  results stored before this version show as raw text.

- Bound a model request's wait between two reads of its answer by the
  connection's timeout, not the whole request; connecting takes at most 5
  seconds (roomkit-rs 93e757c).

- Limit how many tokens one reply may write, per agent: **Max output tokens**
  in `/agents`, or `max_output_tokens` and `--max-output-tokens` for a new
  project's first agent. Without it, requests carry no limit, as before, and
  still no temperature; ChatGPT sign-in connections send none either way. An
  `agents.json` that sets it cannot be read by an earlier version.

- Ask the model again, once, when a round of tool calls is followed by an
  empty answer.

- Fail an MCP tool call after 30 seconds without an answer, the engine's fixed
  bound, as in RoomKit's Python implementation, instead of the tool-call bound.

- Report a tool's outcome to command hooks with four `roomkit.outcome` values:
  `served`, `refused`, `cancelled` or `failed`. `failed` now also covers a
  result a hook withheld and a call nothing served, which were `blocked` and
  `unserved`.

- A `PostToolUse` or `PostToolUseFailure` hook for an outcome RoomKit reports
  late, such as a call cut at the very end of an interruption, may run after
  its turn's `TurnEnd`, with the agent's next turn in that conversation, or
  not at all; `TurnEnd` still waits for the tool hooks already started.

- Name the provider in the footer as the engine reports its usage,
  `OpenAIAIProvider`, instead of `openai`.

- Pass more host variables to shell commands when you ask for them:
  `--inherit-env 'LD_LIBRARY_PATH,NIX_*'`, `ROOMKIT_INHERIT_ENV` or
  `[shell] inherit_env`. A trailing `*` names a prefix, which never passes a
  name that reads as a credential (`KEY`, `TOKEN`, `SECRET`…). `DOCKER_HOST`
  and `KUBECONFIG` join the built-in list.

- Show what a file search left out: `grep`, `list` and `glob` cards now count
  names that are not UTF-8 and lines too long to search, as `grep` already
  counted binary files, with the total in the card's header, and mark a match
  clipped to fit its page.

- Compile one HTTP stack and one SHA-256 implementation: roomkit-rs,
  roomkit-auth and this application move to reqwest 0.13 and sha2 0.11 with
  roomkit-tools, and keep rustls with the ring provider and the webpki roots.
  `make audit` now fails on a crate compiled twice unless `deny.toml` names
  the third-party crate responsible.

- Give shell commands, processes and `!` commands only the variables
  development tools need: the defaults of roomkit-tools (`PATH`, `HOME`, user,
  locale…) plus the SSH agent, desktop session, terminal, editors, proxies,
  certificate bundles and toolchain homes. Other variables, such as API keys
  exported in your shell, no longer reach them. Each command now runs detached
  from the terminal, so one that prompts there fails at once instead of waiting
  for its timeout (roomkit-tools 8d96ec3).

- Name files relative to the project in `read`, `list`, `glob`, `grep` and
  `code_outline` results instead of by their absolute path (roomkit-tools
  8d96ec3). `list`, `glob` and `grep` results stored before this version show
  as raw text.

- Update roomkit-tui to e8c1167: the terminal keeps only a mode it accepts and
  refuses a replacement view whose modes do not fit.

- Let an agent that writes plans ask to enter Plan mode (`plan_mode_enter`) and,
  once its plan is written, ask you to approve it (`plan_mode_exit`): the card
  shows the stored plan, and approving moves the conversation to Ask. The agent
  never moves to Yolo.

- Add `/rename NAME` to name a conversation, also while a response runs: the
  name leads the header and the Herdr pane title, labels the `/resume` list
  and shows in the exit report; the session ID still reopens it
  (roomkit-tui 2ebb703).

- Copy an answer longer than the screen with the mouse: keep the button held
  and turn the wheel, or hold the pointer above or below the conversation, and
  the selection follows while it scrolls; the copy holds the lines no longer
  on screen. Copied text stays highlighted until the next click or key. The
  wheel used to clear the selection, so only what fit on screen could be
  copied.

- Show a tool call as running from its start, and as pending only while a
  decision of its turn is unanswered: every call used to read "pending" until
  it ended, even while it ran.

- Keep only the current and previous versions in a managed installation:
  each update used to add up to 256 MiB that was never removed.

- Drop compatibility with state nobody runs anymore: profile schemas 1–3,
  the first MCP configuration format, and the project-wide lock of binaries
  from before concurrent sessions. Schema 4 profiles still move to schema 5.

- Record internal errors (panics) in the diagnostic log, with their place and
  kind of failure but never their message, and say at exit how many occurred
  and where the log is: the terminal's screen used to hide them.

- State in the system prompt, from the permission rules themselves, which
  tools run without approval in Ask and which Plan permits; the hand-written
  list had omitted the plan and skill tools.

- Add global agents, the default scope in `/agents`: created once and available
  in every project, which opens with the global agent used last. MCP servers and
  skills stay per project. Existing agents stay in their project; changing the
  scope moves an agent. An ID another project uses for its own agent cannot
  become global. Project profiles move to schema 5.

- Keep the welcome screen in a project without an agent: the setup notice now
  shows above its composer instead of switching to the conversation layout
  (roomkit-tui fa0988f).

- Split the sidebar's context gauge into what every request carries: the system
  prompt, per-turn instructions, tool schemas, skills and each MCP server,
  estimated at four characters per token, with the measured remainder as
  messages. Requires roomkit-tui fdae52a.

- List the files this conversation's agents created, modified, deleted or read
  in a sidebar **Files** section and in `/files`. File tools report their own
  changes; shell changes are found by comparing the project before and after
  each command, also when it is cancelled, honoring `.gitignore` and ignoring
  identical rewrites. The list returns with `/resume`.

- Warn before exiting while processes run: Ctrl+D asks for a second press
  instead of detaching at once, and the exit hint states how many processes
  closing will stop, on every screen. The count covers every conversation,
  not only the current view.

- Move to roomkit-tui, formerly roomkit-cli, and its public API: constructors for
  its non-exhaustive types, session accessors and `Rejection` for refused
  commands. Notices now name their conversation. An approval card accepts an
  answer only after half a second on screen, and Shift+Tab no longer changes the
  mode while a card is open. Approval receipts saved by earlier versions no
  longer appear in restored history.

- Print an exit report once the view closes: the command that reopens the
  displayed conversation, its token usage summed over every turn and launch,
  and the background processes that closing stopped. Herdr and the report share
  one resume command builder.

- Add `make build`, `debug`, `clean`, `install` and `uninstall`. Installation
  activates the production binary in the managed layout of `~/.local/bin`.

- Allow multiple local instances on one project with separate conversations.
  Mark occupied sessions in `/resume`, retain ownership through engine shutdown,
  and coordinate draft selection, startup, hook preferences and diagnostic rotation.

- Simplify update messages and handle an empty release catalogue without an error.
  Keep network failures distinct, with HTTP status codes in local diagnostics.
- Release installer locks explicitly so a concurrently starting subprocess cannot
  cause a false "update is running" error.

- Give agents this application's guide through `docs_search`, `docs_read` and
  `docs_list`. The README is embedded, split per `##` section and published
  read-only under `<state-dir>/docs/<revision>/`. The tools run without approval,
  including in Plan mode. Complete schema-3 tool catalogues gain them once;
  profiles are saved in schema 4.
- Add managed Linux/macOS installation, checksummed GitHub releases, optional daily update notices, `/update` with version-specific confirmation, and binary rollback.
- Add native four-platform release checks and a portable installer; Cargo executables are never replaced.

- Discover scoped AGENTS.md and AGENTS.override.md instructions, with global/root
  refresh per turn and progressive nested loading through file exploration and
  patch previews. Preserve system prompts and user messages; deduplicate unchanged
  scopes within each agent turn. Add /instructions to inspect sources and errors.

- Add reviewed global/project command hooks and `/hooks`, with live sidebar
  activity, bounded output inspection and correlated operational diagnostics.
  Cover session start/end, prompt admission, complete agent turns and tool
  outcomes. Hooks may deny or require review even in Yolo; they never bypass
  the application's permissions. Plan execution requires an explicit hook opt-in.
- Isolate hook configuration, subprocess execution and decision parsing in a
  reusable `roomkit-hooks` crate, without engine or terminal dependencies.

- Keep approval receipts with their tool calls when resuming a conversation,
  instead of appending them after the final answer. Restore older receipts
  within their original agent turn.

- Add native persistent plan tools and `/plans`, with a live sidebar showing the
  followed plan, progress and active or blocked steps. Keep plans and selection
  across restarts, isolated by project, conversation and agent.
- Add Plan to Shift+Tab and `/mode`: allow exploration and plan updates while
  blocking project mutations, commands and MCP calls; web research retains
  explicit approval. Mode changes preserve the draft and never execute a plan.
- Upgrade complete default agent catalogues to include the five plan tools;
  preserve custom tool selections. Project profiles now use schema 3.

- Give sent messages the same prompt arrow as the input, with a filled blank
  row above and below the text for a three-row minimum.

- Show submitted messages on the input-colored surface, without the technical
  local author label. Preserve other participants' names; configure this with
  `ui.highlight_own_messages`.

- Require public tool intentions in model-facing schemas by default, configurable
  through `[agent] require_tool_intent` and `--require-tool-intent[=false]` across
  agent reconstruction.
  Tool-owned parameters and missing-intent execution remain unchanged.

- Simplify agent guidance to put routine progress in tool-call intentions and
  reserve messages for findings and conversation.

- Restore formatted tool previews below activity headings while retaining
  consecutive-call replacement and the full history behind F2 or click.

- Ask agents explicitly for a public activity sentence on supported tool calls
  and use it for routine progress. Show the actual tool name when no intention
  is supplied, with full output available in the details view.

- Show public intentions as updating activity lines instead of accumulating
  technical tool cards. Messages separate activity sequences; F2 or a click
  reveals technical details. Preserve approvals, failures and parallel work.
  Enable engine intentions for all assembled agents, including edited profiles.

- Generate both header and welcome symbols from the same RoomKit SVG, using a
  compact seven-column, four-row mark in conversation headers instead of the
  library default. Preserve the four cells while reducing the header's footprint.

- Align engine, CLI and tools dependencies on the consolidated RoomKit main API.
  Raise the minimum Rust version to 1.94 to match the current tools library.

- Simplify welcome to one footer with mode, agent, model, effort and command help.
  Generate the terminal symbol and wordmark from the RoomKit SVG and website font.

- Make human approvals selectable by keyboard or mouse, with readable command
  details and one-time decision receipts retained when the turn settles.
- Remove the technical provider protocol from the sidebar agent/model section.

- Open empty conversations with a centered RoomKit symbol, wordmark and composer.
  Restore the conversation/sidebar on the first exchange, preserve commands and
  image input, and add a configurable blank row below the interface.

- Combine Ask/Yolo and keyboard hints on one line with the sidebar open; remove
  the permanent Shift+Tab label while preserving mode cycling.

- Avoid duplicating agent/model/effort in the footer while the sidebar displays
  them; keep the mode below the composer and restore full status when hidden.

- Show a configurable session sidebar with agent/model, provider connection,
  live context usage, MCP connection/tool status and active processes. F7 toggles
  visibility, F8 navigates actions, and narrow terminals retain the full-width
  conversation. Process details and MCP configuration reuse existing controls.

- Saving an MCP server now connects it and enables tools for the current agent
  automatically. Connected servers can be edited directly and reconnect on save.
  Existing selections and explicit disabling are preserved, with bounded default
  activation, cancellable setup and cleanup for both stdio and HTTP.

- Configure remote MCP Streamable HTTP servers from `/mcp`, with optional bearer
  and header environment references, explicit per-agent selection, Ask approvals,
  cancellable connections and session cleanup. Existing stdio configurations are
  preserved and upgraded when saved.

- Add `/mcp` for private project stdio connections: configure executables and
  environment references, connect/disconnect, inspect and select tools per agent.
- Keep MCP tool names isolated from built-in tools; apply Ask/Yolo permissions
  to every MCP call. Interrupt startup or calls with Esc and join server cleanup
  at application exit. Retain selections without automatically starting servers.


- Explain project skill creation, actual discovery roots and reload/enable/use
  commands to every agent, including before its first skill is available.

- Add per-agent skills with `/skills`: inspect provenance and loading failures,
  enable/disable packages, request their use, and reload local changes.
- Reuse RoomKit's progressive activation, reference reads and session recovery;
  keep skill execution subject to the existing tool permissions.
- Display skill activation and reference results with compact expandable details.

- Paste images with Ctrl+V, remove them with Alt+Backspace, and send them to
  vision-enabled agents. Enable Image input in /agents. Validated PNG attachments
  are private session data outside the workspace and remain available on resume.
- Emit optional OSC 7501 terminal status for Herdr App/Core and other compatible
  terminals, independently of the Herdr TUI adapter.
- Report activity, permission waits, conversation titles and resumable sessions
  to Herdr TUI when its environment is present. Resume the confirmed conversation
  and Ask/Yolo mode with the original workspace, storage and appearance options.
  Keep reports ordered across restarts, tolerate unavailable reporters and release
  the pane after application cleanup. Disable each integration independently with
  `--no-terminal-status` / `--no-herdr` or the corresponding TOML settings.

- Remove finished processes from the live list and hide the footer when none are
  running. Keep command results in conversation history; retained output remains
  accessible explicitly with `/processes ID` or in an already open detail panel.

- Replace the composer arrow with `!` while entering a shell command, displaying
  the prefix only once and restoring the normal prompt outside command mode.

- Execute `!command` or `/shell command` directly from the composer, without a
  model request. Preserve multiline shell input and show stdout/stderr, elapsed
  time and exit status using the existing supervised process manager.
- Interrupt manual commands with Esc, inspect them through F6, and retain bounded
  output in session history for the next agent turn. Retry identities prevent
  duplicate execution across restarts. Explicit commands work in Ask and Yolo,
  including before provider configuration.

- Interrupt active generations and tool calls with Esc while preserving drafts;
  menus and panels close first. Wait for cleanup before allowing the next turn.
- Clear the composer with Ctrl+C and require a second press within two seconds
  to exit, with a visible temporary hint.

- List and manage agent/provider profiles through one menu per profile, with
  explicit deletion confirmation and `list` / `delete <id>` command aliases.
- Keep conversation history when deleting agents, select a remaining agent or
  return to setup, and never reimport an explicitly emptied project on restart.
- Prevent deleting providers referenced by any saved project; serialize profile
  edits and recheck references when deletion is confirmed. Removing a ChatGPT
  provider leaves account sign-out as a separate operation.

- Configure reasoning effort per agent using `/effort` or the agent form, with a
  provider-default option and persistent settings.
- Record private, rotating diagnostic logs from engine errors and tool audit
  hooks; `/diagnostics` shows recent records and the log location, and `--debug`
  adds framework events without logging payloads or credentials.
- Fix Responses completion and tool execution with empty terminal output and
  omitted optional namespaces; align account-based usage with the engine's provider
  identity.

- Add Sign in with ChatGPT through reusable `roomkit-auth`: browser consent,
  separate accounts, automatic credential refresh, account-specific models and
  sign-out. Agents use stateless Responses with namespaced function tools.


- Expand the coding catalogue to 19 tools, adding anonymous Exa search, public
  page fetching, persistent processes and paged ephemeral output archives.
- Apply Ask/Yolo to web requests and process start/input, with explicit action
  details; keep owned reads, stops and archive cleanup available directly.
- Preserve scoped process/output resources across agent and provider changes,
  share file mutation locking across agents, and clean up resources on exit.
- Show sources, process states, stream cursors and archive completeness in tool cards.
- Release project/profile file locks explicitly so concurrent process spawning
  cannot delay reopening a closed session.
- Upgrade legacy complete ten-tool profiles once while preserving custom subsets
  and subsequent selections; retain explicit `patch` in the registered inventory.
- Update tools to support writing files in newly created parent directories and
  retain presentation of historical write results.

- Configure reusable OpenAI-compatible connections with `/providers`, including
  local or environment credentials, timeouts, testing and model discovery.
- Configure project agents with `/agents`: select their provider, model, context
  capacity, instructions and tools; switch interlocutors in one shared history.
- Persist profiles privately and atomically, retain the selected agent across
  launches, and allow first-run setup entirely through the terminal.
- Apply profile changes between turns and preserve existing credentials when
  their masked edit field is left blank.

- Hide unused sessions from `/resume` and reuse app-owned empty drafts across
  launches and repeated `/new` commands, preserving all existing history.
- Keep context tokens and remaining percentage visible while awaiting the next
  measurement, with an explicit `last report` label.
- Show context beside the model when it fits, with a separate row in narrow
  windows to preserve used and remaining percentages.

- Reduce terminal CPU use by avoiding idle redraws and reusing formatted message
  and tool layouts, while preserving streaming, timers and interactive controls.

- Add Rust/Python structural outlines with colored signatures and line ranges,
  available in both permission modes and in the host capability catalogue.
- Display measured context and separate latest-execution token consumption;
  configure server context capacity and model request deadlines explicitly.
- Explain provider failures in the terminal and allow slower model generations
  with a 600-second default request timeout.


- Derive the model's current capability inventory and a filterable `/tools`
  catalogue from the toolsets actually registered by the application.
- Clarify that resumed assistant claims do not override current capabilities,
  and require actual tool results before reporting attempts or failures.

- Add write, edit, diff, structured patch and foreground shell tools, reusing the
  existing RoomKit adapters and a shared mutation workspace.
- Default to Ask for each mutation and shell call, with explicit allow/deny,
  scoped one-time approvals and refusal on timeout or terminal disconnect.
- Add host-confirmed Ask/Yolo modes, Shift+Tab cycling, `/mode`, `--mode` and
  explicit TOML configuration; pending approvals remain explicit across changes.
- Present edits/diffs, patch previews and applied effects, and shell exit/output
  separately from tool-call completion.

- Start a fresh conversation on normal launch; retain explicit `--session NAME`
  behavior and access to existing histories, including `main`.
- Add `/new` and a filterable `/resume` selector scoped to the project, showing
  first-message previews, dates and identifiers without invoking the model.
- Preserve current history on failed or busy switches and deduplicate retried
  `/new` operations, including after restart.
- Adopt the framed four-tile RoomKit terminal logo.

- Hide unnecessary detail links on empty or fully visible filesystem results.

- Add a clickable Back to bottom control and Ctrl+End history navigation.
- Use a filled three-row composer that grows with the draft, with configurable
  height, padding, optional placeholder and prompt marker through explicit TOML.

- Present file listings, reads and search matches with compact summaries,
  paths, source line numbers and colored code, with F2/click details.
- Render model Markdown and configure transcript spacing and preview height
  through the reusable terminal interface.

- Assemble a development agent from RoomKit's engine, tool adapters and TUI.
- Explore a selected project with read-only listing, globbing, search and reads.
- Configure an OpenAI-compatible endpoint and appearance explicitly.
- Persist conversations per project and named session outside the source tree.
- Add reproducible dependency pins, Linux/macOS CI and offline integration checks.
