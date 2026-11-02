#!/usr/bin/env bash
# Fetch static PHP builds (static-php-cli "bulk" flavor — the "common"
# flavor lacks mysqli, see docs/spike.md) into the ferro bin dir.
#
# Usage: scripts/fetch-php.sh [version ...]   e.g. scripts/fetch-php.sh 8.3 8.4
# Default: all supported versions.
set -euo pipefail

BASE="https://dl.static-php.dev/static-php-cli/bulk"
BIN_DIR="$HOME/Library/Application Support/ferro/bin"

# Pinned full versions per minor (update when bumping).
pin_for() {
  case "$1" in
    8.1) echo "8.1.34" ;;
    8.2) echo "8.2.32" ;;
    8.3) echo "8.3.32" ;;
    8.4) echo "8.4.23" ;;
    *) return 1 ;;
  esac
}

versions=("$@")
if [ ${#versions[@]} -eq 0 ]; then
  versions=(8.1 8.2 8.3 8.4)
fi

for minor in "${versions[@]}"; do
  full="$(pin_for "$minor")" || {
    echo "unknown PHP version '$minor' (known: 8.1 8.2 8.3 8.4)" >&2
    exit 1
  }
  dest="$BIN_DIR/php/$minor"
  mkdir -p "$dest"
  for sapi in cli fpm; do
    url="$BASE/php-$full-$sapi-macos-aarch64.tar.gz"
    echo "== php $full ($sapi) -> $dest"
    curl -fSL --retry 3 -o "$dest/$sapi.tar.gz" "$url"
    tar xzf "$dest/$sapi.tar.gz" -C "$dest"
    rm "$dest/$sapi.tar.gz"
  done
  # The cli tarball contains `php`, the fpm tarball `php-fpm`.
  xattr -d com.apple.quarantine "$dest/php" "$dest/php-fpm" 2>/dev/null || true
  "$dest/php" -v | head -1
done
echo "done: $BIN_DIR/php"
