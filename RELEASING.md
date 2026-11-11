# Releasing RoomKit Code

An annotated `vX.Y.Z` tag starts the **Release** workflow. All four native builds
and the dependency audit must pass before it creates a GitHub **draft release**.
Review that draft, then publish it. Published stable releases are discovered by
the installer and updater; drafts and prereleases are not.

## Prepare the source

1. Change the application version in `Cargo.toml` and `Cargo.lock`. The hooks
   crate has its own version and need not change with the application.
2. Add a `## [X.Y.Z]` section to `CHANGELOG.md` with user-facing release notes,
   supported platforms and any migration or rollback limitations.
3. Update the README and user guide when behavior or installation changes.
4. Run `make check`, `make msrv` and `make audit`, review the changes and push
   the tested commit to `main`. Tests use local fixtures, never model accounts.

The first public binary release uses **v0.1.1**. The existing v0.1.0
tag predates the current fixes and must not be moved or replaced.

## Build the draft

From the reviewed commit on `main`:

```sh
git tag -a v0.1.1 -m 'RoomKit Code 0.1.1'
git push origin v0.1.1
```

The workflow validates the tag, manifest version, release notes and membership
in `main` before starting builds. Every job checks out the same resolved commit;
the tag is checked again before the draft is created.

| Runner | Binary target | Minimum system |
| --- | --- | --- |
| Ubuntu 22.04 x86-64 | `x86_64-unknown-linux-gnu` | glibc 2.35 |
| Ubuntu 22.04 ARM64 | `aarch64-unknown-linux-gnu` | glibc 2.35 |
| macOS Intel | `x86_64-apple-darwin` | macOS 12 |
| macOS Apple Silicon | `aarch64-apple-darwin` | macOS 12 |

Each platform runs `make check` and `make msrv`, builds a distribution binary
with `ROOMKIT_RELEASE_BUILD=1`, and exercises its installer. macOS builds receive
an ad-hoc signature before checksums are calculated. Developer ID signing and
notarization are not configured; do not describe these binaries as notarized.

Assets are named `roomkit-code-vX.Y.Z-<target>`, with one `.sha256` file per binary.
The draft also includes `install.sh`, `LICENSE`, `README.md`, `USER_GUIDE.md` and
`SECURITY.md`. Extra files, missing targets, empty binaries or invalid checksums
prevent draft creation. No local configuration or build directory is uploaded.

## Publish

Review the workflow results, notes and all four binary/checksum pairs. Check
installation on Linux and macOS and confirm that the documented minimum systems
and signing limitations still apply. Publish the draft in GitHub, or explicitly:

```sh
gh release edit v0.1.1 --repo roomkit-live/roomkit-code --draft=false --latest
```

Do not replace published binaries or move a published tag. Ship a new version
for corrections. The updater keeps the previous binary for rollback, but cannot
undo application-data migrations.

## Failures and retries

An unavailable runner, failed check or failed audit leaves the release
unpublished. Fix the underlying issue, then rerun the workflow for the existing
tag using **Run workflow**, or:

```sh
gh workflow run release.yml --repo roomkit-live/roomkit-code -f tag=v0.1.1
```

If a draft already exists, inspect it before proceeding. The workflow refuses
to overwrite an existing release, including a draft; finish publication only
after verifying its contents. For a source correction, use a new version/tag.

Public dependency builds need no GitHub secret. While any dependency remains
private, trusted workflows need `ROOMKIT_DEPS_READ_CI` with read-only access to
it. Fork pull requests never receive that credential and cannot compile private
dependencies. Keep them on `pull_request`; never use `pull_request_target` to
run untrusted source with credentials. Private repositories also require an
available GitHub Actions allowance before native builds can run.
