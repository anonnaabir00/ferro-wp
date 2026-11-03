#!/usr/bin/env bash
# Build ferro binaries and install them into ~/.ferro/bin.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release

mkdir -p "$HOME/.ferro/bin"
cp target/release/ferro target/release/ferrod target/release/ferro-shim "$HOME/.ferro/bin/"

# The shim is a multi-call binary: link it under the tool names.
for tool in wp php composer mysql; do
  ln -sf "$HOME/.ferro/bin/ferro-shim" "$HOME/.ferro/bin/$tool"
done

# Space-free copies of the phars (WP-CLI re-launches itself unescaped;
# the app-data path contains a space). See wp_cli_phar in ferro-core.
for phar in wp-cli.phar composer.phar; do
  src="$HOME/Library/Application Support/ferro/bin/$phar"
  [[ -f "$src" ]] && cp "$src" "$HOME/.ferro/bin/$phar"
done

echo "installed ferro, ferrod and shims (wp, php, composer, mysql) to ~/.ferro/bin"
if [[ ":$PATH:" != *":$HOME/.ferro/bin:"* ]]; then
  echo "note: ~/.ferro/bin is not on your PATH — add this to ~/.zshrc:"
  echo '  export PATH="$HOME/.ferro/bin:$PATH"'
fi
echo "next: ferro daemon install && ferro daemon start && ferro doctor"
