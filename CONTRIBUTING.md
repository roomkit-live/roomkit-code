# Contributing

Use the toolchain pinned in `rust-toolchain.toml`. Rust 1.94 is the minimum
supported version. Run `make check`, `make msrv` and `make audit` before delivery.
Install cargo-deny with `cargo install cargo-deny --locked` for the audit gate.
CI runs on Linux and macOS; tests must not require a real model or credentials.
For native image paste on Linux, `make clipboard-check` additionally uses Xvfb
and xclip on a dedicated display; it never touches the desktop clipboard.

Keep generic engine, tools and UI behavior in their owning repositories.
Application-specific choices belong here. Dependencies use immutable Git
revisions and a committed lockfile. All consumers must use the same engine URL
and revision to avoid duplicate, incompatible RoomKit types. A checkout must
build without sibling directories. Review the matching integration tests when
updating a source pin.

`make audit` refuses two versions of one crate. `deny.toml` admits an older
version only through the third-party crate that still holds it, so a RoomKit
crate that brings it fails too. When a RoomKit repository moves a shared
dependency (reqwest, sha2, base64…), align the others rather than widening
that list. HTTP clients take `crate::http_client::builder()`: reqwest is built
without a crypto provider and panics without that configuration.

Public code, documentation and fixtures are in English and contain only generic
examples. Local `docs/`, agent configuration, `*.local.toml`, `.env*`, databases
and build output are ignored. Cargo's package allowlist and `make package-check`
also exclude local planning and credentials. Do not force-add ignored files.

## Dependency access in CI

Public dependencies need no GitHub token, including on fork pull requests.
For a fork that uses private dependencies, trusted workflows can use the optional
repository Actions secret named
`ROOMKIT_DEPS_READ_CI`. Use a fine-grained GitHub token with **Contents: read** on
those private repositories. No write permission is needed. An organization may
require approval for its use. Add it using the hidden interactive prompt:

```sh
gh secret set ROOMKIT_DEPS_READ_CI --repo roomkit-live/roomkit-code
```

Never place the token in source, shell arguments, logs or an issue. Secrets on
another repository are not inherited. Fork pull requests cannot access this
secret; do not run untrusted PR code in a privileged `pull_request_target` job.

Use conventional commit subjects such as `feat(agent): add a review command`.
Document user-visible behavior under `Unreleased` in `CHANGELOG.md`.

## Documentation and releases

Keep the README focused on installation and first use. The full user guide is
`USER_GUIDE.md`; it is embedded in the binary and served by the documentation
tools, so retain its topic headings when moving content. Local `docs/` is
ignored and is not part of the public repository.

Follow [RELEASING.md](RELEASING.md) for version changes, immutable tags, native
builds and reviewed GitHub draft releases.
