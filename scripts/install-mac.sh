#!/usr/bin/env bash
# Install Lite-Anvil and Nano-Anvil to /Applications.
#
# Usage: ./install-mac.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { echo "error: $*" >&2; exit 1; }

install_app() {
    local src="$1"
    local name
    name="$(basename "$src")"
    local dest="/Applications/$name"

    [ -d "$src" ] || die "$name not found at $src"

    echo "Installing $name..."
    rm -rf "$dest"
    cp -R "$src" "$dest"
    xattr -dr com.apple.quarantine "$dest" 2>/dev/null || true
    codesign --force --deep --sign - --timestamp=none "$dest" >/dev/null 2>&1 || true
    echo "  Installed to $dest"
}

install_app "$SCRIPT_DIR/LiteAnvil.app"

if [ -d "$SCRIPT_DIR/NanoAnvil.app" ]; then
    install_app "$SCRIPT_DIR/NanoAnvil.app"
fi

echo ""
echo "Done. Launch from /Applications or run:"
echo "  /Applications/LiteAnvil.app/Contents/MacOS/lite-anvil"
echo "  /Applications/NanoAnvil.app/Contents/MacOS/nano-anvil"
