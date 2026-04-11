---
title: Installation - Lite-Anvil
description: Install Lite-Anvil from prebuilt binaries or build from source on Linux, macOS, and Windows. Requires Rust 1.85+, SDL3, FreeType2, PCRE2.
---

# Installation

## Prebuilt Binaries

Download the latest release for your platform from [GitHub Releases](https://github.com/danpozmanter/lite-anvil/releases).

Release archives contain both the `lite-anvil` (full editor) and `nano-anvil` (minimal single-file editor) binaries. Install whichever you need, or both.

### Linux

Extract the archive, copy the binary and data directory:

```bash
cp lite-anvil ~/.local/bin/
cp -r data ~/.local/share/lite-anvil/
```

Optional -- register for "Open With" on supported file types:

```bash
cp resources/linux/com.lite_anvil.LiteAnvil.desktop ~/.local/share/applications/
cp resources/icons/lite-anvil.png ~/.local/share/icons/hicolor/128x128/apps/
update-desktop-database ~/.local/share/applications/
```

### macOS

Extract the zip and run the included install script:

```bash
./install-mac.sh
```

This copies LiteAnvil.app and NanoAnvil.app to `/Applications`, clears quarantine, and codesigns both.

### Windows

Extract the archive. Copy `lite-anvil.exe` and the `data/` directory wherever you like.

Optional -- register file associations:

```powershell
powershell -ExecutionPolicy Bypass -File resources\windows\install-file-associations.ps1
```

## Building from Source

### Requirements

- **Rust 1.85+** via [rustup](https://rustup.rs)
- System libraries:

| Library | Ubuntu/Debian | Fedora | Arch | macOS (Homebrew) |
|---------|--------------|--------|------|------------------|
| SDL3 | `libsdl3-dev` | `SDL3-devel` | `sdl3` | `sdl3` |
| FreeType2 | `libfreetype6-dev` | `freetype-devel` | `freetype2` | `freetype` |
| PCRE2 | `libpcre2-dev` | `pcre2-devel` | `pcre2` | `pcre2` |

### Build & Run

```bash
git clone https://github.com/danpozmanter/lite-anvil.git
cd lite-anvil
cargo build --release
./target/release/lite-anvil [path]
```

### macOS App Bundle (from source)

```bash
mkdir -p LiteAnvil.app/Contents/MacOS
cp target/release/lite-anvil LiteAnvil.app/Contents/MacOS/
cp -r data LiteAnvil.app/Contents/MacOS/
cp resources/macos/Info.plist LiteAnvil.app/Contents/
codesign --force --deep --sign - --timestamp=none LiteAnvil.app
```

### Debian Package

```bash
cargo install cargo-deb
cargo deb --no-build
```

## Configuration

User config location:

| Platform | Path |
|----------|------|
| Linux | `~/.config/lite-anvil/` |
| macOS | `~/Library/Application Support/lite-anvil/` |
| Windows | `%APPDATA%\lite-anvil\` |

Key files:

- `config.toml` -- editor settings (see [Configuration](guide.md#configuration))
