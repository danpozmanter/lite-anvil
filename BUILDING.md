# Building Lite-Anvil

## Requirements

### Rust toolchain

Rust 1.85 or later. Install via [rustup](https://rustup.rs):

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### System libraries

| Library | Ubuntu/Debian | Fedora | Arch |
|---------|--------------|--------|------|
| SDL3 | `libsdl3-dev` | `SDL3-devel` | `sdl3` |
| FreeType2 | `libfreetype6-dev` | `freetype-devel` | `freetype2` |
| PCRE2 | `libpcre2-dev` | `pcre2-devel` | `pcre2` |

Lua 5.4 is **not** required — it is vendored by the `mlua` crate.

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

```
make install                    # /usr/local/bin + /usr/share/lite-anvil/
make install PREFIX=/usr        # /usr/bin + /usr/share/lite-anvil/
```

## Debian package

```
cargo install cargo-deb
cargo deb --no-build -p forge-core
```

The `.deb` is written to `target/debian/`.

## CI / lint

```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

## Data directory resolution

The binary locates `data/` by walking up from its own path until it finds a
directory containing `data/core/init.lua`. In release installs the data is
copied to `$PREFIX/share/lite-anvil/` and the binary finds it there.
