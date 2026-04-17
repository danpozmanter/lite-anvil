#!/usr/bin/env bash
# Build and install lite-anvil for the host platform.
# Delegates building to scripts/build-local-{linux,mac}.sh.
#
# Usage: ./install.sh [--system]
#   --system  Install system-wide to /usr/local (Linux only; requires sudo)
#   Default:  Install to ~/.local (Linux) or /Applications (macOS)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

SYSTEM=0
for arg in "$@"; do
    case "$arg" in
        --system) SYSTEM=1 ;;
        *) echo "error: unknown argument: $arg" >&2; exit 1 ;;
    esac
done

die() { echo "error: $*" >&2; exit 1; }

app_version() {
    awk -F'"' '
        /^\[workspace\.package\]$/ { in_section = 1; next }
        /^\[/ { in_section = 0 }
        in_section && $1 ~ /^version = / { print $2; exit }
    ' "$SCRIPT_DIR/Cargo.toml"
}

install_linux() {
    bash "$SCRIPT_DIR/scripts/build-local-linux.sh"

    local version stage_dir binary data_src
    version="$(app_version)"
    [ -n "$version" ] || die "could not determine version from Cargo.toml"
    stage_dir="$SCRIPT_DIR/dist/lite-anvil-${version}-linux-x86_64"
    binary="$stage_dir/lite-anvil"
    data_src="$stage_dir/data"

    [ -f "$binary" ] || die "binary not found at $binary"
    [ -d "$data_src" ] || die "data directory not found at $data_src"

    local bin_dir share_dir app_dir icon_dir sudo_cmd
    if [ "$SYSTEM" -eq 1 ]; then
        bin_dir=/usr/local/bin
        share_dir=/usr/local/share/lite-anvil
        app_dir=/usr/share/applications
        icon_dir=/usr/share/icons/hicolor/256x256/apps
        sudo_cmd=sudo
    else
        bin_dir="$HOME/.local/bin"
        share_dir="$HOME/.local/share/lite-anvil"
        app_dir="$HOME/.local/share/applications"
        icon_dir="$HOME/.local/share/icons/hicolor/256x256/apps"
        sudo_cmd=
    fi

    $sudo_cmd mkdir -p "$bin_dir" "$share_dir" "$app_dir" "$icon_dir"

    $sudo_cmd cp "$binary" "$bin_dir/lite-anvil"
    $sudo_cmd chmod 755 "$bin_dir/lite-anvil"

    local nano_binary="$stage_dir/nano-anvil"
    if [ -f "$nano_binary" ]; then
        $sudo_cmd cp "$nano_binary" "$bin_dir/nano-anvil"
        $sudo_cmd chmod 755 "$bin_dir/nano-anvil"
    fi

    # Sync data directory; remove stale files from a previous install.
    $sudo_cmd rsync -a --delete "$data_src/" "$share_dir/" 2>/dev/null \
        || { $sudo_cmd rm -rf "$share_dir"; $sudo_cmd cp -r "$data_src/." "$share_dir/"; }

    local data_nano_src="$stage_dir/data-nano"
    if [ -d "$data_nano_src" ]; then
        local share_nano_dir
        if [ "$SYSTEM" -eq 1 ]; then
            share_nano_dir=/usr/local/share/nano-anvil
        else
            share_nano_dir="$HOME/.local/share/nano-anvil"
        fi
        $sudo_cmd mkdir -p "$share_nano_dir"
        $sudo_cmd rsync -a --delete "$data_nano_src/" "$share_nano_dir/" 2>/dev/null \
            || { $sudo_cmd rm -rf "$share_nano_dir"; $sudo_cmd cp -r "$data_nano_src/." "$share_nano_dir/"; }
    fi

    # SDL3 is statically linked — no libSDL3 to install next to the binary.

    $sudo_cmd cp "$stage_dir/com.lite_anvil.LiteAnvil.desktop" "$app_dir/lite-anvil.desktop"
    if [ -f "$stage_dir/com.nano_anvil.NanoAnvil.desktop" ]; then
        $sudo_cmd cp "$stage_dir/com.nano_anvil.NanoAnvil.desktop" "$app_dir/nano-anvil.desktop"
    fi
    $sudo_cmd cp "$stage_dir/lite-anvil.png" "$icon_dir/lite-anvil.png"
    if [ -f "$stage_dir/nano-anvil.png" ]; then
        $sudo_cmd cp "$stage_dir/nano-anvil.png" "$icon_dir/nano-anvil.png"
    fi

    if command -v update-desktop-database >/dev/null 2>&1; then
        ${sudo_cmd:-} update-desktop-database "$app_dir" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        ${sudo_cmd:-} gtk-update-icon-cache -f -t \
            "${icon_dir%/256x256/apps}" 2>/dev/null || true
    fi

    echo "Installed lite-anvil and nano-anvil to $bin_dir/"

    if [ "$SYSTEM" -eq 0 ] && [[ ":${PATH}:" != *":$HOME/.local/bin:"* ]]; then
        echo "Note: $HOME/.local/bin is not in PATH — add it to your shell profile."
    fi
}

install_macos() {
    bash "$SCRIPT_DIR/scripts/build-local-mac.sh"

    local built_app="$SCRIPT_DIR/dist/LiteAnvil.app"
    [ -d "$built_app" ] || die ".app bundle not found at $built_app"

    local app=/Applications/LiteAnvil.app
    rm -rf "$app"
    cp -R "$built_app" "$app"

    # Re-stamp ad-hoc signature after the copy so the install location matches the signed bundle.
    xattr -cr "$app" 2>/dev/null || true
    codesign --force --deep --sign - --timestamp=none "$app" >/dev/null 2>&1 || true

    local cli_link=/usr/local/bin/lite-anvil
    if [ -L "$cli_link" ] || [ -f "$cli_link" ]; then
        sudo rm -f "$cli_link"
    fi
    sudo mkdir -p /usr/local/bin
    sudo ln -sf "$app/Contents/MacOS/lite-anvil" "$cli_link"

    local nano_app=/Applications/NanoAnvil.app
    local built_nano_app="$SCRIPT_DIR/dist/NanoAnvil.app"
    if [ -d "$built_nano_app" ]; then
        rm -rf "$nano_app"
        cp -R "$built_nano_app" "$nano_app"
        xattr -cr "$nano_app" 2>/dev/null || true
        codesign --force --deep --sign - --timestamp=none "$nano_app" >/dev/null 2>&1 || true
    fi

    local nano_cli=/usr/local/bin/nano-anvil
    if [ -L "$nano_cli" ] || [ -f "$nano_cli" ]; then
        sudo rm -f "$nano_cli"
    fi
    if [ -d "$nano_app" ]; then
        sudo ln -sf "$nano_app/Contents/MacOS/nano-anvil" "$nano_cli"
    fi

    local version
    version="$(app_version)"
    echo "Installed Lite-Anvil ${version:-?} to $app"
    if [ -d "$nano_app" ]; then
        echo "Installed Nano-Anvil ${version:-?} to $nano_app"
    fi
    echo "CLI symlinks: $cli_link, $nano_cli"
}

OS="$(uname)"
case "$OS" in
    Linux)  install_linux ;;
    Darwin) install_macos ;;
    *)      die "unsupported OS: $OS" ;;
esac
