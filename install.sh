#!/usr/bin/env bash
# Install lite-anvil from a local release build.
# Usage: ./install.sh [--system]
#   --system  Install system-wide to /usr/local (Linux only; requires sudo)
#   Default:  Install to ~/.local (Linux) or /Applications (macOS)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINARY="$SCRIPT_DIR/target/release/lite-anvil"
DATA_SRC="$SCRIPT_DIR/data"
ICON_SRC="$SCRIPT_DIR/resources/icons/lite-anvil.png"
DESKTOP_SRC="$SCRIPT_DIR/resources/linux/com.lite_anvil.LiteAnvil.desktop"

SYSTEM=0
for arg in "$@"; do
    case "$arg" in
        --system) SYSTEM=1 ;;
        *) echo "error: unknown argument: $arg" >&2; exit 1 ;;
    esac
done

die() { echo "error: $*" >&2; exit 1; }

[ -f "$BINARY" ] || die "binary not found at $BINARY — run 'cargo build --release' first"
[ -d "$DATA_SRC" ] || die "data directory not found at $DATA_SRC"

install_linux() {
    if [ "$SYSTEM" -eq 1 ]; then
        BIN_DIR=/usr/local/bin
        SHARE_DIR=/usr/local/share/lite-anvil
        APP_DIR=/usr/share/applications
        ICON_DIR=/usr/share/icons/hicolor/256x256/apps
        SUDO=sudo
    else
        BIN_DIR="$HOME/.local/bin"
        SHARE_DIR="$HOME/.local/share/lite-anvil"
        APP_DIR="$HOME/.local/share/applications"
        ICON_DIR="$HOME/.local/share/icons/hicolor/256x256/apps"
        SUDO=
    fi

    $SUDO mkdir -p "$BIN_DIR" "$SHARE_DIR" "$APP_DIR" "$ICON_DIR"

    $SUDO cp "$BINARY" "$BIN_DIR/lite-anvil"
    $SUDO chmod 755 "$BIN_DIR/lite-anvil"

    # Sync data directory; remove stale files from a previous install.
    $SUDO rsync -a --delete "$DATA_SRC/" "$SHARE_DIR/" 2>/dev/null \
        || { $SUDO rm -rf "$SHARE_DIR"; $SUDO cp -r "$DATA_SRC/." "$SHARE_DIR/"; }

    $SUDO cp "$DESKTOP_SRC" "$APP_DIR/lite-anvil.desktop"
    $SUDO cp "$ICON_SRC" "$ICON_DIR/lite-anvil.png"

    if command -v update-desktop-database &>/dev/null; then
        ${SUDO:-} update-desktop-database "$APP_DIR" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache &>/dev/null; then
        ${SUDO:-} gtk-update-icon-cache -f -t \
            "${ICON_DIR%/256x256/apps}" 2>/dev/null || true
    fi

    echo "Installed to $BIN_DIR/lite-anvil"

    if [ "$SYSTEM" -eq 0 ] && [[ ":${PATH}:" != *":$HOME/.local/bin:"* ]]; then
        echo "Note: $HOME/.local/bin is not in PATH — add it to your shell profile."
    fi
}

install_macos() {
    APP=/Applications/LiteAnvil.app
    MACOS_DIR="$APP/Contents/MacOS"

    mkdir -p "$MACOS_DIR"
    cp "$BINARY" "$MACOS_DIR/lite-anvil"
    chmod 755 "$MACOS_DIR/lite-anvil"

    rm -rf "$MACOS_DIR/data"
    cp -r "$DATA_SRC" "$MACOS_DIR/data"

    cat > "$APP/Contents/Info.plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>LiteAnvil</string>
    <key>CFBundleDisplayName</key>
    <string>Lite-Anvil</string>
    <key>CFBundleIdentifier</key>
    <string>com.lite_anvil.LiteAnvil</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleExecutable</key>
    <string>lite-anvil</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

    # CLI symlink — /usr/local/bin may need sudo on some systems.
    CLI_LINK=/usr/local/bin/lite-anvil
    if [ -L "$CLI_LINK" ] || [ -f "$CLI_LINK" ]; then
        sudo rm -f "$CLI_LINK"
    fi
    sudo mkdir -p /usr/local/bin
    sudo ln -sf "$MACOS_DIR/lite-anvil" "$CLI_LINK"

    echo "Installed to $APP"
    echo "CLI symlink: $CLI_LINK"
}

OS="$(uname)"
case "$OS" in
    Linux)  install_linux ;;
    Darwin) install_macos ;;
    *)      die "unsupported OS: $OS" ;;
esac
