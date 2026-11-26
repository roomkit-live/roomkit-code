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

Each published binary has a SHA-256 checksum file, signed with the project's
[minisign](https://jedisct1.github.io/minisign/) key where the release is built.
The key's public half is `src/updates/release-key.pub`; its secret half is in no
release. The updater embeds the public key and installs nothing whose checksum
the key did not sign. `install.sh` checks the signature when `minisign` is
installed, and says so when it is not: it then only detects corruption, as a
checksum served beside the binary proves nothing about its origin. macOS
binaries currently use ad-hoc code signing, without
Apple Developer ID signing or notarization. Download only from this repository's
GitHub releases.

`install.sh` itself comes from the release it installs and is not signed: who
could replace a binary could replace it too. To trust only the key, download
the binary, its `.sha256` file and its `.sha256.minisig` file from the release,
then check them before running anything:

```sh
minisign -Vm roomkit-code-vX.Y.Z-TARGET.sha256 -P RWSxO7fsDP+u4csK1pHZ2lwcpWCmOmBhgT0Wu5OlCxx6aEwsEe1FDtlK
sha256sum -c roomkit-code-vX.Y.Z-TARGET.sha256
```

A signature proves where a release comes from, not that it is the latest: a
signature does not expire and a withdrawn release cannot be revoked. The
updater refuses a release older than the installed one, but an older signed
release that is still newer, served as the latest, would be offered.

Updates are activated only after explicit confirmation. A binary rollback does
not undo application-data migrations; follow the release notes before updating.
