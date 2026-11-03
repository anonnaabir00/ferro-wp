#!/usr/bin/env bash
# Fetch the latest stable composer.phar and verify its SHA-256 checksum.
set -euo pipefail

BIN_DIR="$HOME/Library/Application Support/ferro/bin"
PHAR="$BIN_DIR/composer.phar"

mkdir -p "$BIN_DIR"
echo "== composer -> $PHAR"
curl -fSL --retry 3 -o "$PHAR" "https://getcomposer.org/download/latest-stable/composer.phar"
expected="$(curl -fsSL "https://getcomposer.org/download/latest-stable/composer.phar.sha256" | tr -d '[:space:]')"
actual="$(shasum -a 256 "$PHAR" | awk '{print $1}')"
if [ "$expected" != "$actual" ]; then
  echo "CHECKSUM MISMATCH for composer.phar" >&2
  rm -f "$PHAR"
  exit 1
fi
echo "checksum ok"
# Space-free copy next to the shims (see wp_cli_phar in ferro-core).
mkdir -p "$HOME/.ferro/bin"
cp "$PHAR" "$HOME/.ferro/bin/composer.phar"
