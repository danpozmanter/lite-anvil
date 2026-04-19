---
title: Installation - Lite Anvil
description: Install Lite Anvil from prebuilt binaries or build from source on Linux, macOS, and Windows. Requires Rust 1.85+, SDL3, FreeType2, PCRE2.
---

# Installation

## Prebuilt Binaries

Download the latest release for your platform from [GitHub Releases](https://github.com/danpozmanter/lite-anvil/releases).

Release archives contain three binaries: `lite-anvil` (full editor), `nano-anvil` (minimal single-file editor), and `note-anvil` (markdown note-taking app). Install whichever you need, or all three.

### Linux

Pick whichever format matches your distro:

The `.deb` and `.rpm` install both `lite-anvil` and `nano-anvil` together.

- **Debian / Ubuntu**: download `lite-anvil_*.deb` and install with:
  ```bash
  sudo apt install ./lite-anvil_*_amd64.deb
  ```
- **Fedora / RHEL / openSUSE**: download `lite-anvil-*.rpm` and install with:
  ```bash
  sudo dnf install ./lite-anvil-*.x86_64.rpm
  ```
- **Anywhere else (Arch, NixOS, Gentoo, ...)**: download `lite-anvil-*-x86_64.AppImage`, make it executable, and run it:
  ```bash
  chmod +x lite-anvil-*-x86_64.AppImage
  ./lite-anvil-*-x86_64.AppImage
  ```
- **Manual / portable**: extract `lite-anvil-*-linux-x86_64.tar.gz` and copy the binary + `data/` directory to wherever you like. Desktop entry + icon are included in `resources/linux/` and `resources/icons/`.

### macOS

Download `lite-anvil-*-macos-{x86_64,aarch64}.zip` for your architecture, extract it, and from a Terminal in the extracted folder run:

```bash
bash install-mac.sh
```

This copies `LiteAnvil.app` + `NanoAnvil.app` to `/Applications`, clears the download quarantine bit, and creates `lite-anvil` / `nano-anvil` CLI symlinks in `/usr/local/bin` and `/opt/homebrew/bin`.

Running the script via `bash` is what lets the quarantine clear actually take effect — macOS Sequoia's Gatekeeper no longer honors the right-click → *Open* bypass for unsigned apps, so a double-click install would fail.

### Windows

Download `LiteAnvil-*-x86_64-setup.exe` and run it. The installer bundles both `lite-anvil.exe` and `nano-anvil.exe`, creates Start Menu shortcuts, and offers optional file-association and *Add to PATH* tasks. A SmartScreen warning appears the first time (the build is unsigned) — click *More info* → *Run anyway*.

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
