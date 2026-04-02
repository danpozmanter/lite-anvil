---
title: Installation
description: How to install Lite-Anvil from prebuilt binaries or build from source on Linux, macOS, and Windows.
---

# Installation

## Prebuilt Binaries

Download the latest release for your platform from [GitHub Releases](https://github.com/danpozmanter/lite-anvil/releases).

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

1. Download the `.app` bundle or build one (see below).
2. Move `LiteAnvil.app` to `/Applications`.
3. Sign the bundle so macOS doesn't block it:

```bash
codesign --force --deep --sign - --timestamp=none /Applications/LiteAnvil.app
```

If macOS still refuses to open it (Gatekeeper quarantine), remove the quarantine attribute:

```bash
sudo xattr -dr com.apple.quarantine /Applications/LiteAnvil.app
```

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

Lua 5.4 is **not** required -- it is vendored by the `mlua` crate.

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
cargo deb --no-build -p forge-core
```

## Configuration

User config location:

| Platform | Path |
|----------|------|
| Linux | `~/.config/lite-anvil/` |
| macOS | `~/Library/Application Support/lite-anvil/` |
| Windows | `%APPDATA%\lite-anvil\` |

Key files:

- `config.lua` -- editor settings, keybindings, plugin config
- `init.lua` -- user initialization script
- `plugins/` -- user Lua plugins (see [Plugins](guide.md#plugins))
- `lsp.json` -- custom LSP server configurations (see [LSP Support](guide.md#lsp-support))
