#!/usr/bin/env bash
# Fetch MySQL 8.4 LTS (arm64 tarball from Oracle's CDN) into the ferro bin dir.
set -euo pipefail

VERSION="${1:-8.4.8}"
URL="https://dev.mysql.com/get/Downloads/MySQL-8.4/mysql-$VERSION-macos15-arm64.tar.gz"
BIN_DIR="$HOME/Library/Application Support/ferro/bin"
DEST="$BIN_DIR/mysql"

if [ -x "$DEST/bin/mysqld" ]; then
  echo "mysql already present at $DEST"
  "$DEST/bin/mysqld" --version
  exit 0
fi

echo "== mysql $VERSION -> $DEST"
mkdir -p "$BIN_DIR"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fSL --retry 3 -o "$tmp/mysql.tar.gz" "$URL"
tar xzf "$tmp/mysql.tar.gz" -C "$tmp"
rm -rf "$DEST"
mv "$tmp/mysql-$VERSION-macos15-arm64" "$DEST"
xattr -dr com.apple.quarantine "$DEST" 2>/dev/null || true
"$DEST/bin/mysqld" --version
echo "done: $DEST"
