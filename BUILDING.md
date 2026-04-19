# Building Lite Anvil

## Requirements

### Rust toolchain

Rust 1.85 or later. Install via [rustup](https://rustup.rs):

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### System libraries

| Tool | Ubuntu/Debian | Fedora | Arch | macOS (Homebrew) |
|---------|--------------|--------|------|------------------|
| CMake + C compiler | `cmake build-essential` | `cmake gcc` | `cmake base-devel` | `cmake` (via Xcode CLT) |

That's it — no library dev packages required. SDL3, FreeType, and
PCRE2 are all compiled from vendored source by their respective `-sys`
crates during `cargo build` and statically linked into the resulting
binaries. This keeps the editor's subsystems (no GPU / no camera / no
joystick / etc.) and regex semantics identical across NixOS, Arch,
Debian, Homebrew Mac, and Windows — there is no system-SDL3 or
system-libpcre2 variance to worry about.

On **Linux**, still install the X11 dev headers SDL3 needs at compile
time: `libx11-dev libxext-dev libxcursor-dev libxinerama-dev libxi-dev
libxrandr-dev libxkbcommon-dev` (Ubuntu) — or add
`wayland-protocols libwayland-dev` if you want Wayland available as a
fallback.

### Wayland-only hosts

The editor defaults to X11 + software framebuffer (via `SDL_VIDEO_DRIVER=x11,wayland`,
`SDL_FRAMEBUFFER_ACCELERATION=0`, `SDL_RENDER_DRIVER=software`) to keep
OpenGL / libGL out of the process on NVIDIA-heavy setups. On a host
with no X server and no Xwayland, launch will fail with
`SDL3 init failed: x11 not available`.

Any of our defaults can be overridden by setting the matching SDL
environment variable before launch — the editor only applies its
default when the env var is unset. So on a Wayland-only machine:

```bash
# One-shot.
SDL_VIDEO_DRIVER=wayland lite-anvil

# Or in your shell profile.
export SDL_VIDEO_DRIVER=wayland
```

This requires SDL3 itself to have been built with the Wayland backend
(`wayland-protocols libwayland-dev libxkbcommon-dev` at SDL build
time — sdl3-sys picks them up automatically when they're visible to
`pkg-config`). If your `sdl3-sys` cache predates installing those
packages, force a rebuild with `cargo clean -p sdl3-sys && cargo build
--release`.

To go all the way back to SDL's accelerated OpenGL renderer (useful
for comparing performance or sidestepping the whole software path on
GPU-only hosts), export `SDL_FRAMEBUFFER_ACCELERATION=1`.

On **Windows**, the MSVC toolchain (Visual Studio Build Tools) ships
cmake and a C/C++ compiler; nothing else to install.

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

Move `LiteAnvil.app` to `/Applications`. The Info.plist registers Lite Anvil
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
