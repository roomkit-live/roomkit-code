# Changelog

## Unreleased

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
