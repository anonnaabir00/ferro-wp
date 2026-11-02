#!/usr/bin/env bash
# Fetch the latest WP-CLI phar and verify its SHA-512 checksum.
set -euo pipefail

BIN_DIR="$HOME/Library/Application Support/ferro/bin"
PHAR="$BIN_DIR/wp-cli.phar"
BASE="https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar"

mkdir -p "$BIN_DIR"
echo "== wp-cli -> $PHAR"
curl -fSL --retry 3 -o "$PHAR" "$BASE/wp-cli.phar"
curl -fSL --retry 3 -o "$PHAR.sha512" "$BASE/wp-cli.phar.sha512"
expected="$(awk '{print $1}' "$PHAR.sha512")"
actual="$(shasum -a 512 "$PHAR" | awk '{print $1}')"
if [ "$expected" != "$actual" ]; then
  echo "CHECKSUM MISMATCH for wp-cli.phar" >&2
  echo "  expected: $expected" >&2
  echo "  actual:   $actual" >&2
  rm -f "$PHAR"
  exit 1
fi
echo "checksum ok"
ls -lh "$PHAR"
