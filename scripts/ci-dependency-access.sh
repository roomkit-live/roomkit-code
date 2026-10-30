#!/usr/bin/env bash
set -euo pipefail

# Repository secrets do not propagate from our private dependencies.
if [[ -z "${ROOMKIT_DEPS_READ_CI:-}" ]]; then
    echo 'Set ROOMKIT_DEPS_READ_CI with Contents: read on roomkit-rs, roomkit-cli, roomkit-tools and roomkit-auth.' >&2
    exit 1
fi
umask 077
roomkit_credential_dir="$RUNNER_TEMP/roomkit-code-dependency-access"
mkdir -p "$roomkit_credential_dir"
printf 'https://x-access-token:%s@github.com\n' "$ROOMKIT_DEPS_READ_CI" > "$roomkit_credential_dir/credentials"
git config --global credential.helper "store --file=$roomkit_credential_dir/credentials"
printf 'CARGO_NET_GIT_FETCH_WITH_CLI=true\n' >> "$GITHUB_ENV"
