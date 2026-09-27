#!/usr/bin/env bash
# Sign macOS update archives and generate Sparkle's appcast.
#
# Usage: scripts/appcast-macos.sh <updates-directory>
#
# E1_SPARKLE_DOWNLOAD_URL_PREFIX is required. SPARKLE_PRIVATE_KEY is optional:
# when absent, Sparkle reads its default signing key from the login keychain.

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: scripts/appcast-macos.sh <updates-directory>" >&2
  exit 2
fi

script_directory=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
project_root=$(cd "$script_directory/.." && pwd)
updates_directory=$(cd "$1" && pwd)
download_prefix=${E1_SPARKLE_DOWNLOAD_URL_PREFIX:?set E1_SPARKLE_DOWNLOAD_URL_PREFIX}
sparkle_version="2.9.4"
sparkle_bin="${SPARKLE_BIN:-${E1_BUILD_CACHE_DIR:-$project_root/.e1-cache}/sparkle/$sparkle_version/bin}"
generator="$sparkle_bin/generate_appcast"

if [[ ! -x "$generator" ]]; then
  echo "generate_appcast is missing; run scripts/bundle-macos.sh first" >&2
  exit 1
fi

arguments=(
  --download-url-prefix "$download_prefix"
  --release-notes-url-prefix "$download_prefix"
)

if [[ -n "${SPARKLE_PRIVATE_KEY:-}" ]]; then
  printf '%s\n' "$SPARKLE_PRIVATE_KEY" | \
    "$generator" "${arguments[@]}" --ed-key-file - "$updates_directory"
else
  "$generator" "${arguments[@]}" "$updates_directory"
fi

appcast="$updates_directory/appcast.xml"
if [[ ! -f "$appcast" ]]; then
  echo "generate_appcast did not create $appcast" >&2
  exit 1
fi

read -r enclosures unsigned < <(
  awk '
    /<enclosure[[:space:]]/ {
      enclosures += 1
      if ($0 !~ /sparkle:edSignature=/) unsigned += 1
    }
    END { print enclosures + 0, unsigned + 0 }
  ' "$appcast"
)
if (( enclosures == 0 )); then
  echo "the generated appcast contains no update enclosure" >&2
  exit 1
fi
if (( unsigned != 0 )); then
  echo "the generated appcast contains $unsigned unsigned enclosure(s)" >&2
  exit 1
fi

echo "Wrote signed appcast: $appcast"
