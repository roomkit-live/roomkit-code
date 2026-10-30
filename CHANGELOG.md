# Changelog

## Unreleased

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
