#!/usr/bin/env bash
set -euo pipefail

# Public dependencies and fork pull requests need no repository secret.
if [[ -z "${ROOMKIT_DEPS_READ_CI:-}" ]]; then
    echo 'Using anonymous dependency access.'
    exit 0
fi
umask 077
roomkit_credential_dir="$RUNNER_TEMP/roomkit-code-dependency-access"
mkdir -p "$roomkit_credential_dir"
printf 'https://x-access-token:%s@github.com\n' "$ROOMKIT_DEPS_READ_CI" > "$roomkit_credential_dir/credentials"
git config --global credential.helper "store --file=$roomkit_credential_dir/credentials"
printf 'CARGO_NET_GIT_FETCH_WITH_CLI=true\n' >> "$GITHUB_ENV"
