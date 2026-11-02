#!/usr/bin/env bash
# Fetch Adminer (single-file build) and install the ferro plugin files.
# Adminer does not publish checksums; the download is verified by PHP
# lint + a content sanity check (documented in docs/decisions.md).
set -euo pipefail

VERSION="${1:-6.2.0}"
URL="https://github.com/vrana/adminer/releases/download/v$VERSION/adminer-$VERSION-mysql-en.php"
DIR="$HOME/Library/Application Support/ferro/adminer"
REPO="$(cd "$(dirname "$0")/.." && pwd)"

mkdir -p "$DIR"
echo "== adminer $VERSION -> $DIR"
curl -fSL --retry 3 -o "$DIR/adminer.php" "$URL"

PHP="$HOME/Library/Application Support/ferro/bin/php/8.3/php"
[ -x "$PHP" ] || PHP="$(command -v php || true)"
if [ -z "$PHP" ]; then
  echo "no php available to verify the download" >&2
  exit 1
fi
"$PHP" -l "$DIR/adminer.php" >/dev/null
grep -q "Adminer" "$DIR/adminer.php"
size=$(stat -f%z "$DIR/adminer.php")
if [ "$size" -lt 200000 ]; then
  echo "adminer.php looks too small ($size bytes)" >&2
  exit 1
fi

cp "$REPO/assets/adminer/index.php" "$DIR/index.php"
cp "$REPO/assets/adminer/ferro-bridge.php" "$DIR/ferro-bridge.php"
cp "$REPO/assets/adminer/ferro-plugin.php" "$DIR/ferro-plugin.php"
cp "$REPO/assets/adminer/adminer.css" "$DIR/adminer.css"
for f in index.php ferro-bridge.php ferro-plugin.php; do
  "$PHP" -l "$DIR/$f" >/dev/null
done
echo "$VERSION" > "$DIR/VERSION"
echo "done: adminer $VERSION installed (verified by php -l + size check)"
