# Lite-Anvil

A lightweight text editor written in Lua with a Rust core.

Lite-Anvil is a fork of [Lite XL](https://github.com/lite-xl/lite-xl) that replaces the
original C backend with Rust.

## Purpose & Forking

This project exists partially as an experiment, and partially as something I just wanted for myself.

**No Support**

I do not intend to maintain or support this in any way, but wanted to share the code so anyone interested can freely use, learn from, or fork this project into something new.

There will be a tag "InitialPort" for the initial port into Rust, before I begin altering this further to suit my own ergonomics.

## Features

- Full Lua 5.4 plugin API — all Lite XL plugins work without modification
- SDL3 window and input handling
- FreeType2 font rendering with subpixel antialiasing
- PCRE2 regex engine
- Cross-platform filesystem monitoring via the `notify` crate
- No system Lua required — Lua 5.4 is vendored via `mlua`

## Building

### Dependencies

```
# Ubuntu / Debian
apt install libsdl3-dev libfreetype6-dev libpcre2-dev

# Fedora
dnf install SDL3-devel freetype-devel pcre2-devel

# Arch
pacman -S sdl3 freetype2 pcre2
```

Rust 1.85+ is required. Install via [rustup](https://rustup.rs).

**Note** You may need to build sdl3 yourself on some systems. I did on Linux Mint 22.2.

### Build

```
cargo build --release
```

The binary is placed at `target/release/lite-anvil`. See `BUILDING.md` for full
install and packaging instructions.

### Run

```
./target/release/lite-anvil [path]
```

## Install

```
make install          # installs to /usr/local by default
make install PREFIX=/usr
```

## License

MIT — see [LICENSE](LICENSE).
