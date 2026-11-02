#!/bin/bash
# Build the SwiftUI menu bar app and wrap it as ~/Applications/Ferro.app
# (ad-hoc signed; the menu bar app talks to ferrod over the Unix socket).
set -euo pipefail
cd "$(dirname "$0")/../macos/FerroMenuBar"

swift build -c release 2>&1 | tail -3

APP="$HOME/Applications/Ferro.app"
BIN="$(swift build -c release --show-bin-path)/FerroMenuBar"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/FerroMenuBar"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Ferro</string>
    <key>CFBundleDisplayName</key><string>Ferro WP</string>
    <key>CFBundleIdentifier</key><string>dev.ferro.menubar</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleExecutable</key><string>FerroMenuBar</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSUIElement</key><true/>
</dict>
</plist>
PLIST

codesign --force --sign - "$APP" 2>/dev/null || true
echo "installed $APP — launch with: open \"$APP\""
