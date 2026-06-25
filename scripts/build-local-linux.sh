#!/usr/bin/env bash
# Build a local Linux x86_64 release artifact matching the GitHub Actions release output.
# Produces:
#   dist/lite-anvil-${VERSION}-linux-x86_64/         (staging directory)
#   dist/lite-anvil-${VERSION}-linux-x86_64.tar.gz   (release archive)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VERSION="$(awk -F'"' '
    /^\[workspace\.package\]$/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && $1 ~ /^version = / { print $2; exit }
' Cargo.toml)"

[ -n "$VERSION" ] || { echo "error: could not read version from Cargo.toml" >&2; exit 1; }

ARCHIVE_BASE="lite-anvil-${VERSION}-linux-x86_64"
DIST_DIR="dist"
STAGE_DIR="$DIST_DIR/$ARCHIVE_BASE"
ARCHIVE="$DIST_DIR/${ARCHIVE_BASE}.tar.gz"

cargo build --release --workspace

rm -rf "$STAGE_DIR" "$ARCHIVE"
mkdir -p "$STAGE_DIR"

cp target/release/lite-anvil "$STAGE_DIR/"
cp target/release/nano-anvil "$STAGE_DIR/"
cp target/release/note-anvil "$STAGE_DIR/"
cp -r data "$STAGE_DIR/"
# SDL3 is statically linked via sdl3-sys — nothing to bundle under lib/.
cp resources/linux/com.lite_anvil.LiteAnvil.desktop "$STAGE_DIR/"
cp resources/linux/com.nano_anvil.NanoAnvil.desktop "$STAGE_DIR/"
cp resources/linux/com.note_anvil.NoteAnvil.desktop "$STAGE_DIR/"
# Install the hicolor theme icons under their dashless reverse-DNS app IDs.
# The freedesktop icon lookup strips a name's trailing "-component" as a
# fallback, so a dashed name like "note-anvil" resolves to the generic
# "note" icon shipped by Breeze before the app's own PNG is consulted. The
# reverse-DNS IDs contain no dashes, so the fallback never fires and each
# app's icon resolves to itself. The source filenames stay dashed; only
# the installed theme name matters.
cp resources/icons/lite-anvil.png "$STAGE_DIR/com.lite_anvil.LiteAnvil.png"
cp resources/icons/nano-anvil.png "$STAGE_DIR/com.nano_anvil.NanoAnvil.png"
cp resources/icons/note-anvil.png "$STAGE_DIR/com.note_anvil.NoteAnvil.png"

tar -C "$DIST_DIR" -czf "$ARCHIVE" "$ARCHIVE_BASE"

echo "Built archive: $ARCHIVE"
echo "Staging dir:   $STAGE_DIR"
