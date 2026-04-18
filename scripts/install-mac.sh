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

# Create CLI symlinks so `lite-anvil` / `nano-anvil` work from the shell.
# We symlink into every likely bin dir to cover both Homebrew layouts:
#   /usr/local/bin        — Intel Homebrew, and what /etc/paths ships with
#   /opt/homebrew/bin     — Apple Silicon Homebrew (prepended to PATH by brew)
# Installing into both guarantees the command resolves regardless of whether
# the user's shell has been rewritten to prefer one over the other.
install_cli_symlink() {
    local binary="$1"
    local linkname="$2"
    local target_dir="$3"

    [ -f "$binary" ] || return 0

    if [ ! -d "$target_dir" ]; then
        sudo mkdir -p "$target_dir" 2>/dev/null || return 0
    fi

    sudo rm -f "$target_dir/$linkname" 2>/dev/null || true
    sudo ln -sf "$binary" "$target_dir/$linkname" 2>/dev/null || return 0
    echo "  CLI: $target_dir/$linkname"
}

echo ""
echo "Installing CLI symlinks (may prompt for sudo)..."
for bin_dir in /usr/local/bin /opt/homebrew/bin; do
    install_cli_symlink "/Applications/LiteAnvil.app/Contents/MacOS/lite-anvil" lite-anvil "$bin_dir"
    install_cli_symlink "/Applications/NanoAnvil.app/Contents/MacOS/nano-anvil" nano-anvil "$bin_dir"
done

echo ""
echo "Done. Launch from /Applications or run:"
echo "  lite-anvil"
echo "  nano-anvil"

# Warn if neither bin dir is on PATH — the symlinks exist but the shell
# won't find them. Tell the user exactly what to add to their rc file.
path_has() {
    case ":${PATH}:" in *":$1:"*) return 0 ;; *) return 1 ;; esac
}
if ! path_has /usr/local/bin && ! path_has /opt/homebrew/bin; then
    case "${SHELL##*/}" in
        zsh)  shell_rc="$HOME/.zshrc" ;;
        bash) shell_rc="$HOME/.bash_profile" ;;
        fish) shell_rc="$HOME/.config/fish/config.fish" ;;
        *)    shell_rc="your shell profile" ;;
    esac
    echo ""
    echo "Note: neither /usr/local/bin nor /opt/homebrew/bin is in your PATH,"
    echo "so 'lite-anvil' and 'nano-anvil' won't resolve in the shell."
    echo "Add one of them to $shell_rc — for zsh or bash:"
    echo ""
    echo "    export PATH=\"/usr/local/bin:\$PATH\""
    echo ""
fi
