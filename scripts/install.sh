#!/usr/bin/env bash
# Build ferro binaries and install them into ~/.ferro/bin.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release

mkdir -p "$HOME/.ferro/bin"
cp target/release/ferro target/release/ferrod "$HOME/.ferro/bin/"

echo "installed ferro + ferrod to ~/.ferro/bin"
if [[ ":$PATH:" != *":$HOME/.ferro/bin:"* ]]; then
  echo "note: ~/.ferro/bin is not on your PATH — add this to ~/.zshrc:"
  echo '  export PATH="$HOME/.ferro/bin:$PATH"'
fi
echo "next: ferro daemon install && ferro daemon start && ferro doctor"
