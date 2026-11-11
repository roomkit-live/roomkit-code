# Security

## Reporting a vulnerability

Please use [GitHub's private vulnerability reporting](https://github.com/roomkit-live/roomkit-code/security/advisories/new).
If private reporting is unavailable, open an issue asking for a private contact
without including vulnerability details. Do not post credentials, conversation
history, private source code or exploit details in a public issue.

Include the affected version and platform, a minimal reproduction using generic
data, the expected behavior and the impact. The latest stable release is the
supported version; development snapshots may contain changes not yet released.

## Runtime boundaries

Ask, Plan and Yolo control which actions the agent may request and which need
approval. They are not an operating-system sandbox. Shell commands, enabled
hooks and local MCP servers run with the permissions of the account launching
RoomKit Code. Use a separate account or container when you need isolation.

The selected provider receives the context sent to the model, which can include
messages, images and files returned by tools. Conversation state and credentials
are stored locally. Treat the state directory, backups and diagnostic exports
as private. Do not put API keys in prompts or shell commands that are retained
in conversation history.

## Release integrity

Published binaries have SHA-256 checksums verified by the installer and updater.
These detect corruption; a checksum served by the same release is not an
independent signature. macOS binaries currently use ad-hoc code signing, without
Apple Developer ID signing or notarization. Download only from this repository's
GitHub releases.

Updates are activated only after explicit confirmation. A binary rollback does
not undo application-data migrations; follow the release notes before updating.
