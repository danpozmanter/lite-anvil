#!/usr/bin/env bash
# Build a local macOS .app bundle for the host architecture.
# Produces:
#   dist/LiteAnvil.app                                 (codesigned ad-hoc, xattrs cleared)
#   dist/lite-anvil-${VERSION}-macos-${ARCH}.zip       (release archive)
#
# No sudo required. Codesign is ad-hoc (`-`); xattr -cr clears the quarantine bit
# and any leftover extended attributes that would otherwise trip Gatekeeper locally.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

[ "$(uname)" = "Darwin" ] || { echo "error: build-local-mac.sh must be run on macOS" >&2; exit 1; }

HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
    arm64)  ARCH_LABEL="aarch64"; RUST_TARGET="aarch64-apple-darwin" ;;
    x86_64) ARCH_LABEL="x86_64";  RUST_TARGET="x86_64-apple-darwin"  ;;
    *) echo "error: unsupported host arch: $HOST_ARCH" >&2; exit 1 ;;
esac

VERSION="$(awk -F'"' '
    /^\[workspace\.package\]$/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && $1 ~ /^version = / { print $2; exit }
' Cargo.toml)"

[ -n "$VERSION" ] || { echo "error: could not read version from Cargo.toml" >&2; exit 1; }

DIST_DIR="dist"
APP="$DIST_DIR/LiteAnvil.app"
ARCHIVE="$DIST_DIR/lite-anvil-${VERSION}-macos-${ARCH_LABEL}.zip"

die() { echo "error: $*" >&2; exit 1; }

command -v otool >/dev/null 2>&1 || die "otool is required (install Xcode command line tools)"
command -v install_name_tool >/dev/null 2>&1 || die "install_name_tool is required"
command -v codesign >/dev/null 2>&1 || die "codesign is required"

macos_list_deps() {
    otool -L "$1" | tail -n +2 | awk '{print $1}'
}

macos_should_bundle_dep() {
    case "$1" in
        /System/Library/*|/usr/lib/*|@executable_path/*|@loader_path/*)
            return 1
            ;;
        /*|@rpath/*)
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

macos_bundled_dep_relpath() {
    local resolved_dep="$1"

    if [[ "$resolved_dep" == *.framework/* ]]; then
        local framework_dir framework_name
        framework_dir="${resolved_dep%/*.framework/*}.framework"
        framework_name="$(basename "$framework_dir")"
        printf '%s/%s\n' "$framework_name" "${framework_name%.framework}"
    else
        basename "$resolved_dep"
    fi
}

macos_bundled_dep_ref() {
    local target="$1"
    local resolved_dep="$2"
    local relpath
    relpath="$(macos_bundled_dep_relpath "$resolved_dep")"

    case "$target" in
        */Contents/MacOS/*)
            printf '@executable_path/../Frameworks/%s\n' "$relpath"
            ;;
        */Contents/Frameworks/*.framework/*)
            printf '@loader_path/../%s\n' "$relpath"
            ;;
        */Contents/Frameworks/*)
            printf '@loader_path/%s\n' "$relpath"
            ;;
        *)
            die "unsupported macOS dependency target: $target"
            ;;
    esac
}

bundle_macos_dylibs() {
    local app="$1"
    local binary="$app/Contents/MacOS/lite-anvil"
    local frameworks_dir="$app/Contents/Frameworks"
    local processed_list="$frameworks_dir/.bundled-dylibs"
    local queued_list="$frameworks_dir/.bundled-queue"

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

        if [[ "$resolved_dep" == *.framework/* ]]; then
            local framework_dir framework_name
            framework_dir="${resolved_dep%/*.framework/*}.framework"
            framework_name="$(basename "$framework_dir")"

            dep_name="$framework_name"
            dest_dep="$frameworks_dir/$framework_name"
            install_name_tool -change "$source_dep" "$(macos_bundled_dep_ref "$target" "$resolved_dep")" "$target"

            if ! grep -Fxq "$framework_dir" "$processed_list"; then
                printf '%s\n' "$framework_dir" >> "$processed_list"

                cp -R "$framework_dir" "$dest_dep"
                chmod -R u+w "$dest_dep"
                chmod 755 "$dest_dep/${framework_name%.framework}"
                install_name_tool -id "@loader_path/$framework_name/${framework_name%.framework}" "$dest_dep/${framework_name%.framework}"

                printf '%s\n' "$dest_dep/${framework_name%.framework}" >> "$queued_list"
            fi
            return 0
        fi

        dep_name="$(basename "$resolved_dep")"
        dest_dep="$frameworks_dir/$dep_name"

        install_name_tool -change "$source_dep" "$(macos_bundled_dep_ref "$target" "$resolved_dep")" "$target"

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

        local nested_dep
        while IFS= read -r nested_dep; do
            macos_should_bundle_dep "$nested_dep" || continue
            bundle_macos_dep "$queued_dep" "$nested_dep"
        done < <(macos_list_deps "$queued_dep")
        queue_index=$((queue_index + 1))
    done

    rm -f "$processed_list" "$queued_list"
}

sign_macos_app() {
    local app="$1"

    xattr -cr "$app" 2>/dev/null || true

    codesign --force --sign - --timestamp=none "$app/Contents/MacOS/lite-anvil"

    if [ -f "$app/Contents/MacOS/nano-anvil" ]; then
        codesign --force --sign - --timestamp=none "$app/Contents/MacOS/nano-anvil"
    fi

    if [ -f "$app/Contents/MacOS/note-anvil" ]; then
        codesign --force --sign - --timestamp=none "$app/Contents/MacOS/note-anvil"
    fi

    if [ -d "$app/Contents/Frameworks" ]; then
        local item
        while IFS= read -r item; do
            codesign --force --sign - --timestamp=none "$item"
        done < <(find "$app/Contents/Frameworks" \
            \( -name "*.dylib" -o -name "*.framework" \) \
            -print | sort -r)
    fi

    codesign --force --deep --sign - --timestamp=none "$app"
    codesign --verify --deep --strict --verbose=2 "$app"
}

cargo build --release --workspace --target "$RUST_TARGET"

BINARY="target/$RUST_TARGET/release/lite-anvil"
[ -f "$BINARY" ] || die "binary not found at $BINARY"

# Ensure binaries have @executable_path RPATHs for .app bundle layout.
# This is necessary because cargo's build.rs RPATH may not survive
# across cached builds.
for bin in "$BINARY" "target/$RUST_TARGET/release/nano-anvil" "target/$RUST_TARGET/release/note-anvil"; do
    [ -f "$bin" ] || continue
    install_name_tool -add_rpath @executable_path/../Frameworks "$bin" 2>/dev/null || true
    install_name_tool -add_rpath @executable_path "$bin" 2>/dev/null || true
done

mkdir -p "$DIST_DIR"
rm -rf "$APP" "$ARCHIVE"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks"

cp "$BINARY" "$APP/Contents/MacOS/lite-anvil"
chmod 755 "$APP/Contents/MacOS/lite-anvil"

cp -r data "$APP/Contents/MacOS/data"

bundle_macos_dylibs "$APP"

sed "s/0\.19\.3/${VERSION}/g" resources/macos/Info.plist > "$APP/Contents/Info.plist"

# Build NanoAnvil.app as a separate bundle.
NANO_BINARY="target/$RUST_TARGET/release/nano-anvil"
NANO_APP="$DIST_DIR/NanoAnvil.app"
if [ -f "$NANO_BINARY" ]; then
    rm -rf "$NANO_APP"
    mkdir -p "$NANO_APP/Contents/MacOS" "$NANO_APP/Contents/Frameworks"
    cp "$NANO_BINARY" "$NANO_APP/Contents/MacOS/nano-anvil"
    chmod 755 "$NANO_APP/Contents/MacOS/nano-anvil"
    cp -r data "$NANO_APP/Contents/MacOS/data"

    # Rewrite bundle_macos_dylibs for nano-anvil binary.
    bundle_macos_dylibs_nano() {
        local app="$1"
        local binary="$app/Contents/MacOS/nano-anvil"
        local frameworks_dir="$app/Contents/Frameworks"
        local processed_list="$frameworks_dir/.bundled-dylibs"
        local queued_list="$frameworks_dir/.bundled-queue"

        mkdir -p "$frameworks_dir"
        : > "$processed_list"
        : > "$queued_list"

        bundle_macos_binary_deps "$binary"

        local queue_index=1
        local queued_dep
        while queued_dep="$(sed -n "${queue_index}p" "$queued_list")" && [ -n "$queued_dep" ]; do
            local nested_dep
            while IFS= read -r nested_dep; do
                macos_should_bundle_dep "$nested_dep" || continue
                bundle_macos_dep "$queued_dep" "$nested_dep"
            done < <(macos_list_deps "$queued_dep")
            queue_index=$((queue_index + 1))
        done

        rm -f "$processed_list" "$queued_list"
    }
    bundle_macos_dylibs_nano "$NANO_APP"

    # NanoAnvil Info.plist.
    cat > "$NANO_APP/Contents/Info.plist" << PLISTEOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Nano Anvil</string>
    <key>CFBundleDisplayName</key>
    <string>Nano Anvil</string>
    <key>CFBundleIdentifier</key>
    <string>com.nano-anvil.NanoAnvil</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleExecutable</key>
    <string>nano-anvil</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLISTEOF

    sign_macos_app "$NANO_APP"
fi

# Build NoteAnvil.app as a separate bundle.
NOTE_BINARY="target/$RUST_TARGET/release/note-anvil"
NOTE_APP="$DIST_DIR/NoteAnvil.app"
if [ -f "$NOTE_BINARY" ]; then
    rm -rf "$NOTE_APP"
    mkdir -p "$NOTE_APP/Contents/MacOS" "$NOTE_APP/Contents/Frameworks"
    cp "$NOTE_BINARY" "$NOTE_APP/Contents/MacOS/note-anvil"
    chmod 755 "$NOTE_APP/Contents/MacOS/note-anvil"
    cp -r data "$NOTE_APP/Contents/MacOS/data"

    bundle_macos_dylibs_note() {
        local app="$1"
        local binary="$app/Contents/MacOS/note-anvil"
        local frameworks_dir="$app/Contents/Frameworks"
        local processed_list="$frameworks_dir/.bundled-dylibs"
        local queued_list="$frameworks_dir/.bundled-queue"

        mkdir -p "$frameworks_dir"
        : > "$processed_list"
        : > "$queued_list"

        bundle_macos_binary_deps "$binary"

        local queue_index=1
        local queued_dep
        while queued_dep="$(sed -n "${queue_index}p" "$queued_list")" && [ -n "$queued_dep" ]; do
            local nested_dep
            while IFS= read -r nested_dep; do
                macos_should_bundle_dep "$nested_dep" || continue
                bundle_macos_dep "$queued_dep" "$nested_dep"
            done < <(macos_list_deps "$queued_dep")
            queue_index=$((queue_index + 1))
        done

        rm -f "$processed_list" "$queued_list"
    }
    bundle_macos_dylibs_note "$NOTE_APP"

    cat > "$NOTE_APP/Contents/Info.plist" << PLISTEOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Note Anvil</string>
    <key>CFBundleDisplayName</key>
    <string>Note Anvil</string>
    <key>CFBundleIdentifier</key>
    <string>com.note-anvil.NoteAnvil</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleExecutable</key>
    <string>note-anvil</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLISTEOF

    sign_macos_app "$NOTE_APP"
fi

sign_macos_app "$APP"

cp "$ROOT_DIR/scripts/install-mac.sh" "$DIST_DIR/install-mac.sh"
(cd "$DIST_DIR" && zip -qry "$(basename "$ARCHIVE")" LiteAnvil.app NanoAnvil.app NoteAnvil.app install-mac.sh)

echo "Built archive: $ARCHIVE"
echo "App bundles:   $APP, $NANO_APP, $NOTE_APP"
