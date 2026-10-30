# Contributing

Use the toolchain pinned in `rust-toolchain.toml`. Rust 1.92 is the minimum
supported version. Run `make check`, `make msrv` and `make audit` before delivery.
Install cargo-deny with `cargo install cargo-deny --locked` for the audit gate.
CI runs on Linux and macOS; tests must not require a real model or credentials.

Keep generic engine, tools and UI behavior in their owning repositories.
Application-specific choices belong here. Dependencies use immutable Git
revisions and a committed lockfile. All consumers must use the same engine URL
and revision to avoid duplicate, incompatible RoomKit types. A checkout must
build without sibling directories. Review the matching integration tests when
updating a source pin.

Public code, documentation and fixtures are in English and contain only generic
examples. Local `docs/`, agent configuration, `*.local.toml`, `.env*`, databases
and build output are ignored. Cargo's package allowlist and `make package-check`
also exclude local planning and credentials. Do not force-add ignored files.

## Private dependency access in CI

Until dependencies are public, set a repository Actions secret named
`ROOMKIT_DEPS_READ_CI`. Use a fine-grained GitHub token with **Contents: read** on
`roomkit-live/roomkit-rs`, `roomkit-live/roomkit-cli` and
`roomkit-live/roomkit-tools` and `roomkit-live/roomkit-auth`. No write permission is needed. An organization may
require approval for its use. Add it using the hidden interactive prompt:

```sh
gh secret set ROOMKIT_DEPS_READ_CI --repo roomkit-live/roomkit-code
```

Never place the token in source, shell arguments, logs or an issue. Secrets on
another repository are not inherited. Fork pull requests cannot access this
secret; do not run untrusted PR code in a privileged `pull_request_target` job.

Use conventional commit subjects such as `feat(agent): add a review command`.
Document user-visible behavior under `Unreleased` in `CHANGELOG.md`.
