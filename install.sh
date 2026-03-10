#!/usr/bin/env bash
# Install lite-anvil from a local release build.
# Usage: ./install.sh [--system]
#   --system  Install system-wide to /usr/local (Linux only; requires sudo)
#   Default:  Install to ~/.local (Linux) or /Applications (macOS)
set -euo pipefail

cargo build --release

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

macos_list_deps() {
    otool -L "$1" | tail -n +2 | awk '{print $1}'
}

macos_should_bundle_dep() {
    case "$1" in
        /System/Library/*|/usr/lib/*|@executable_path/*|@loader_path/*)
            return 1
            ;;
        /*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

macos_resolve_dep() {
    local dep="$1"
    local dep_name candidate
    local matches=()

    if [ -f "$dep" ]; then
        printf '%s\n' "$dep"
        return 0
    fi

    dep_name="$(basename "$dep")"

    shopt -s nullglob
    for candidate in $dep; do
        [ -f "$candidate" ] && matches+=("$candidate")
    done

    if [ "${#matches[@]}" -gt 0 ]; then
        printf '%s\n' "${matches[0]}"
        shopt -u nullglob
        return 0
    fi

    for candidate in \
        "/opt/homebrew/lib/$dep_name" \
        /opt/homebrew/opt/*/lib/"$dep_name" \
        "/usr/local/lib/$dep_name" \
        /usr/local/opt/*/lib/"$dep_name"
    do
        if [ -f "$candidate" ]; then
            printf '%s\n' "$candidate"
            shopt -u nullglob
            return 0
        fi
    done
    shopt -u nullglob

    return 1
}

bundle_macos_dylibs() {
    local app="$1"
    local binary="$app/Contents/MacOS/lite-anvil"
    local frameworks_dir="$app/Contents/Frameworks"
    local processed_list="$frameworks_dir/.bundled-dylibs"
    local queued_list="$frameworks_dir/.bundled-queue"

    command -v otool >/dev/null 2>&1 || die "otool is required on macOS"
    command -v install_name_tool >/dev/null 2>&1 || die "install_name_tool is required on macOS"

    mkdir -p "$frameworks_dir"
    : > "$processed_list"
    : > "$queued_list"

    bundle_macos_binary_deps() {
        local target="$1"
        local dep
        while IFS= read -r dep; do
            macos_should_bundle_dep "$dep" || continue
            bundle_macos_dep "$target" "$dep"
        done < <(macos_list_deps "$target")
    }

    bundle_macos_dep() {
        local target="$1"
        local source_dep="$2"
        local resolved_dep dep_name dest_dep

        resolved_dep="$(macos_resolve_dep "$source_dep")" \
            || die "missing dynamic library: $source_dep"

        dep_name="$(basename "$resolved_dep")"
        dest_dep="$frameworks_dir/$dep_name"

        install_name_tool -change "$source_dep" "@executable_path/../Frameworks/$dep_name" "$target"

        if ! grep -Fxq "$resolved_dep" "$processed_list"; then
            printf '%s\n' "$resolved_dep" >> "$processed_list"

            cp -Lf "$resolved_dep" "$dest_dep"
            chmod 755 "$dest_dep"
            install_name_tool -id "@loader_path/$dep_name" "$dest_dep"

            printf '%s\n' "$dest_dep" >> "$queued_list"
        fi
    }

    bundle_macos_binary_deps "$binary"

    local queue_index=1
    local queued_dep
    while queued_dep="$(sed -n "${queue_index}p" "$queued_list")" && [ -n "$queued_dep" ]; do
        [ -n "$queued_dep" ] || continue

        local nested_dep nested_name
        while IFS= read -r nested_dep; do
            macos_should_bundle_dep "$nested_dep" || continue

            local resolved_nested_dep
            resolved_nested_dep="$(macos_resolve_dep "$nested_dep")" \
                || die "missing nested dynamic library: $nested_dep"

            nested_name="$(basename "$resolved_nested_dep")"

            install_name_tool -change "$nested_dep" "@loader_path/$nested_name" "$queued_dep"

            if ! grep -Fxq "$resolved_nested_dep" "$processed_list"; then
                printf '%s\n' "$resolved_nested_dep" >> "$processed_list"
                cp -Lf "$resolved_nested_dep" "$frameworks_dir/$nested_name"
                chmod 755 "$frameworks_dir/$nested_name"
                install_name_tool -id "@loader_path/$nested_name" "$frameworks_dir/$nested_name"
                printf '%s\n' "$frameworks_dir/$nested_name" >> "$queued_list"
            fi
        done < <(macos_list_deps "$queued_dep")
        queue_index=$((queue_index + 1))
    done

    rm -f "$processed_list" "$queued_list"
}

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
    FRAMEWORKS_DIR="$APP/Contents/Frameworks"

    rm -rf "$FRAMEWORKS_DIR"
    mkdir -p "$MACOS_DIR" "$FRAMEWORKS_DIR"
    cp "$BINARY" "$MACOS_DIR/lite-anvil"
    chmod 755 "$MACOS_DIR/lite-anvil"

    rm -rf "$MACOS_DIR/data"
    cp -r "$DATA_SRC" "$MACOS_DIR/data"

    bundle_macos_dylibs "$APP"

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
    <string>0.2.2</string>
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
