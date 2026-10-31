#!/bin/sh
# Install a published, checksummed release; no Rust, sudo or shell-profile edits.
set -eu
repo=roomkit-live/roomkit-code
fail() { printf '%s\n' "$*" >&2; exit 1; }
case "$(uname -s)" in Linux) os=unknown-linux-gnu ;; Darwin) os=apple-darwin ;; *) fail 'Supported platforms: Linux and macOS' ;; esac
case "$(uname -m)" in x86_64|amd64) arch=x86_64 ;; aarch64|arm64) arch=aarch64 ;; *) fail 'Unsupported CPU architecture' ;; esac
use_gh=false
if command -v gh >/dev/null 2>&1; then
    if [ -n "${ROOMKIT_UPDATE_TOKEN:-}" ]; then GH_TOKEN=$ROOMKIT_UPDATE_TOKEN; export GH_TOKEN; fi
    if GH_HOST=github.com gh auth status --hostname github.com >/dev/null 2>&1; then use_gh=true; fi
fi
version=${1:-}
if [ -z "$version" ]; then
    if "$use_gh"; then
        version=$(GH_HOST=github.com gh api "repos/$repo/releases/latest" --jq .tag_name)
    else
        command -v curl >/dev/null 2>&1 || fail 'Install curl or GitHub CLI (gh) first'
        version=$(curl --proto '=https' -fsS --connect-timeout 5 --max-time 15 "https://api.github.com/repos/$repo/releases/latest" | sed -n 's/.*"tag_name"[ ]*:[ ]*"\([^"]*\)".*/\1/p')
    fi
fi
version=${version#v}
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' || fail 'No stable release found. For private releases, run gh auth login with repository read access.'
asset="roomkit-code-v$version-$arch-$os"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
if "$use_gh"; then
    GH_HOST=github.com gh release download "v$version" --repo "$repo" --pattern "$asset" --pattern "$asset.sha256" --dir "$work"
else
    for name in "$asset" "$asset.sha256"; do
        curl --proto '=https' --proto-redir '=https' -fsSL --connect-timeout 5 --max-time 180 --max-filesize 268435456 "https://github.com/$repo/releases/download/v$version/$name" -o "$work/$name" || fail 'Release download failed. Private repository: use gh auth login first.'
    done
fi
read -r expected filename < "$work/$asset.sha256"
[ "$filename" = "$asset" ] || fail 'Checksum filename mismatch'
[ "${#expected}" = 64 ] || fail 'Invalid checksum'
case "$expected" in *[!0-9a-f]*) fail 'Invalid checksum' ;; esac
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$work/$asset" | cut -d ' ' -f 1)
elif command -v shasum >/dev/null 2>&1; then actual=$(shasum -a 256 "$work/$asset" | cut -d ' ' -f 1)
else fail 'Install sha256sum or shasum first'; fi
[ "$actual" = "$expected" ] || fail 'Checksum verification failed; nothing installed'
chmod 755 "$work/$asset"
[ "$("$work/$asset" --version)" = "roomkit-code $version" ] || fail 'Release binary version mismatch'
destination=${ROOMKIT_CODE_BIN_DIR:-"$HOME/.local/bin"}
"$work/$asset" --install-dir "$destination"
case ":$PATH:" in *":$destination:"*) ;; *) printf 'Add %s to your PATH.\n' "$destination" ;; esac
printf 'Run roomkit-code in your project. Updates: /update or roomkit-code --self-update.\n'
