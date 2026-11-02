#!/usr/bin/env bash
# Fetch Mailpit (static binary, darwin-arm64) into the ferro bin dir.
# Verifies the download against the sha256 digest GitHub publishes for
# the release asset.
set -euo pipefail

BIN_DIR="$HOME/Library/Application Support/ferro/bin"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSL https://api.github.com/repos/axllent/mailpit/releases/latest -o "$TMP/release.json"
read -r tag expected < <(python3 - "$TMP/release.json" <<'PY'
import json, sys
release = json.load(open(sys.argv[1]))
digest = ""
for asset in release.get("assets", []):
    if asset.get("name") == "mailpit-darwin-arm64.tar.gz":
        digest = asset.get("digest", "")
        break
print(release.get("tag_name", ""), digest.removeprefix("sha256:"))
PY
)
[ -n "$tag" ] && [ -n "$expected" ] || {
  echo "could not resolve the latest mailpit release + digest" >&2
  exit 1
}

curl -fsSL "https://github.com/axllent/mailpit/releases/download/$tag/mailpit-darwin-arm64.tar.gz" \
  -o "$TMP/mailpit.tar.gz"
actual="$(shasum -a 256 "$TMP/mailpit.tar.gz" | awk '{print $1}')"
if [ "$expected" != "$actual" ]; then
  echo "checksum mismatch for mailpit (expected $expected, got $actual)" >&2
  exit 1
fi

tar -xzf "$TMP/mailpit.tar.gz" -C "$TMP" mailpit
mkdir -p "$BIN_DIR"
mv "$TMP/mailpit" "$BIN_DIR/mailpit"
chmod +x "$BIN_DIR/mailpit"
echo "installed mailpit $tag to $BIN_DIR/mailpit"
