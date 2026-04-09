# Building Lite-Anvil

## Requirements

### Rust toolchain

Rust 1.85 or later. Install via [rustup](https://rustup.rs):

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### System libraries

| Library | Ubuntu/Debian | Fedora | Arch | macOS (Homebrew) |
|---------|--------------|--------|------|------------------|
| SDL3 | `libsdl3-dev` | `SDL3-devel` | `sdl3` | `sdl3` |
| FreeType2 | `libfreetype6-dev` | `freetype-devel` | `freetype2` | `freetype` |
| PCRE2 | `libpcre2-dev` | `pcre2-devel` | `pcre2` | `pcre2` |

On **Windows**, dependencies are resolved via vcpkg (see the CI workflow).

## Build

```
cargo build --release
```

The binary is `target/release/lite-anvil`.

For a headless (no SDL) build used in CI:

```
cargo build --no-default-features
```

## Install

### Linux

```
cp target/release/lite-anvil ~/.local/bin/
cp -r data ~/.local/share/lite-anvil/
```

To register for "Open With" on supported file types:

```
cp resources/linux/com.lite_anvil.LiteAnvil.desktop ~/.local/share/applications/
cp resources/icons/lite-anvil.png ~/.local/share/icons/hicolor/128x128/apps/
update-desktop-database ~/.local/share/applications/
```

### macOS

Build, then create the app bundle:

```
mkdir -p LiteAnvil.app/Contents/MacOS
cp target/release/lite-anvil LiteAnvil.app/Contents/MacOS/
cp -r data LiteAnvil.app/Contents/MacOS/
cp resources/macos/Info.plist LiteAnvil.app/Contents/
```

Move `LiteAnvil.app` to `/Applications`. The Info.plist registers Lite-Anvil
for "Open With" on all supported file types.

Sign the bundle so macOS doesn't block it:

```bash
codesign --force --deep --sign - --timestamp=none LiteAnvil.app
```

If the app was downloaded or copied in a way that adds Gatekeeper quarantine
and macOS refuses to open it, remove the quarantine attribute:

```bash
sudo xattr -dr com.apple.quarantine /Applications/LiteAnvil.app
```

### Windows

Copy `lite-anvil.exe` and the `data/` directory wherever you like, then
register file associations:

```powershell
powershell -ExecutionPolicy Bypass -File resources\windows\install-file-associations.ps1
```

To remove associations:

```powershell
powershell -ExecutionPolicy Bypass -File resources\windows\uninstall-file-associations.ps1
```

## Debian package

```
cargo install cargo-deb
cargo deb --no-build -p forge-core
```

The `.deb` is written to `target/debian/`. It includes the `.desktop` file
for file associations.

## CI / lint

```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

## Data directory resolution

The binary locates `data/` by walking up from its own path until it finds a
directory containing `data/fonts/Lilex-Regular.ttf`. In release installs the
data is copied to the platform-appropriate location and the binary finds it
there.
