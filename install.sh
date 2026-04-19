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

    local note_binary="$stage_dir/note-anvil"
    if [ -f "$note_binary" ]; then
        $sudo_cmd cp "$note_binary" "$bin_dir/note-anvil"
        $sudo_cmd chmod 755 "$bin_dir/note-anvil"
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
    if [ -f "$stage_dir/com.note_anvil.NoteAnvil.desktop" ]; then
        $sudo_cmd cp "$stage_dir/com.note_anvil.NoteAnvil.desktop" "$app_dir/note-anvil.desktop"
    fi
    $sudo_cmd cp "$stage_dir/lite-anvil.png" "$icon_dir/lite-anvil.png"
    if [ -f "$stage_dir/nano-anvil.png" ]; then
        $sudo_cmd cp "$stage_dir/nano-anvil.png" "$icon_dir/nano-anvil.png"
    fi
    if [ -f "$stage_dir/note-anvil.png" ]; then
        $sudo_cmd cp "$stage_dir/note-anvil.png" "$icon_dir/note-anvil.png"
    fi
    # Force a fresh mtime so any desktop env that watches dirs notices.
    $sudo_cmd touch "$icon_dir/lite-anvil.png" \
        "$icon_dir/nano-anvil.png" \
        "$icon_dir/note-anvil.png" 2>/dev/null || true

    # If we previously installed to the *other* prefix (system vs user),
    # the older PNG may still win the theme lookup. Drop both old paths
    # so the freshly-installed one is the only candidate.
    if [ "$SYSTEM" -eq 0 ]; then
        # User install — wipe any old system-prefix icons. (No sudo: only
        # cleans up if the user has perms.)
        rm -f /usr/local/share/icons/hicolor/256x256/apps/lite-anvil.png \
              /usr/local/share/icons/hicolor/256x256/apps/nano-anvil.png \
              /usr/local/share/icons/hicolor/256x256/apps/note-anvil.png \
              /usr/share/icons/hicolor/256x256/apps/lite-anvil.png \
              /usr/share/icons/hicolor/256x256/apps/nano-anvil.png \
              /usr/share/icons/hicolor/256x256/apps/note-anvil.png 2>/dev/null || true
    fi

    if command -v update-desktop-database >/dev/null 2>&1; then
        ${sudo_cmd:-} update-desktop-database "$app_dir" 2>/dev/null || true
    fi

    # Refresh the icon cache. `gtk-update-icon-cache -t` quietly bails
    # without an `index.theme`, which most user-installed hicolor roots
    # lack — so blow away any stale cache file first as a fallback so
    # GTK falls back to per-file scanning and picks up our new PNG.
    local icon_root="${icon_dir%/256x256/apps}"
    $sudo_cmd rm -f "$icon_root/icon-theme.cache" 2>/dev/null || true
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        ${sudo_cmd:-} gtk-update-icon-cache -f -t "$icon_root" 2>/dev/null || true
    fi

    echo "Installed lite-anvil, nano-anvil, and note-anvil to $bin_dir/"

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

    local note_app=/Applications/NoteAnvil.app
    local built_note_app="$SCRIPT_DIR/dist/NoteAnvil.app"
    if [ -d "$built_note_app" ]; then
        rm -rf "$note_app"
        cp -R "$built_note_app" "$note_app"
        xattr -cr "$note_app" 2>/dev/null || true
        codesign --force --deep --sign - --timestamp=none "$note_app" >/dev/null 2>&1 || true
    fi

    local note_cli=/usr/local/bin/note-anvil
    if [ -L "$note_cli" ] || [ -f "$note_cli" ]; then
        sudo rm -f "$note_cli"
    fi
    if [ -d "$note_app" ]; then
        sudo ln -sf "$note_app/Contents/MacOS/note-anvil" "$note_cli"
    fi

    local version
    version="$(app_version)"
    echo "Installed Lite Anvil ${version:-?} to $app"
    if [ -d "$nano_app" ]; then
        echo "Installed Nano Anvil ${version:-?} to $nano_app"
    fi
    if [ -d "$note_app" ]; then
        echo "Installed Note Anvil ${version:-?} to $note_app"
    fi
    echo "CLI symlinks: $cli_link, $nano_cli, $note_cli"

    # On stock macOS `/usr/local/bin` is wired into the default PATH via
    # `/etc/paths`, but Apple Silicon setups where the user has rewritten
    # PATH (e.g. to prefer Homebrew under `/opt/homebrew/bin`) often drop
    # it. Detect that and point the user at the fix rather than silently
    # leaving `lite-anvil` / `nano-anvil` un-runnable from the shell.
    if [[ ":${PATH}:" != *":/usr/local/bin:"* ]]; then
        local shell_rc
        case "${SHELL##*/}" in
            zsh)  shell_rc="$HOME/.zshrc" ;;
            bash) shell_rc="$HOME/.bash_profile" ;;
            fish) shell_rc="$HOME/.config/fish/config.fish" ;;
            *)    shell_rc="your shell profile" ;;
        esac
        echo
        echo "Note: /usr/local/bin is not in your PATH, so 'lite-anvil'"
        echo "and 'nano-anvil' won't be runnable directly. Add it to"
        echo "$shell_rc — for zsh or bash:"
        echo
        echo "    export PATH=\"/usr/local/bin:\$PATH\""
        echo
    fi
}

OS="$(uname)"
case "$OS" in
    Linux)  install_linux ;;
    Darwin) install_macos ;;
    *)      die "unsupported OS: $OS" ;;
esac
