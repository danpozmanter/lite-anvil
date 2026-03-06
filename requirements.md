# Lite-Anvil: Build Requirements

> **Target platform:** Linux Mint 22 (Ubuntu 24.04 Noble base)
> **Build system:** Cargo (Rust)

---

## 1. Rust Toolchain

Install via `rustup` (do not use the distro `rustc` package — it is outdated):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup default stable
rustup update
```

Verify:
```bash
rustc --version   # must be >= 1.82.0
cargo --version
```

---

## 2. System Packages

```bash
sudo apt update && sudo apt install -y \
  build-essential \
  pkg-config \
  cmake \
  clang \
  libfreetype6-dev \
  libpcre2-dev \
  liblua5.4-dev \
  libfontconfig1-dev \
  libx11-dev \
  libwayland-dev \
  libxkbcommon-dev \
  libgl1-mesa-dev \
  libgles2-mesa-dev
```

### Package versions (Ubuntu 24.04 Noble, verified 2026-03-04)

| Package | Version |
|---------|---------|
| `build-essential` | 12.10ubuntu1 |
| `pkg-config` | 1.8.1-2build1 |
| `cmake` | 3.28.3-1build7 |
| `clang` | 1:18.0-59~exp2 (→ clang-18) |
| `libfreetype6-dev` | 2.13.2+dfsg-1build3 |
| `libpcre2-dev` | 10.42-4ubuntu2 |
| `liblua5.4-dev` | 5.4.6-3build2 |
| `libfontconfig1-dev` | 2.15.0-1.1ubuntu2 |
| `libx11-dev` | 2:1.8.7-1build1 |
| `libwayland-dev` | 1.22.0-2.1build1 |
| `libxkbcommon-dev` | 1.6.0-1build1 |
| `libgl1-mesa-dev` | 25.2.8-0ubuntu0.24.04.1 |
| `libgles2-mesa-dev` | 25.2.8-0ubuntu0.24.04.1 |

> **Note on `liblua5.4-dev`:** This is only required if `mlua` is built without the `vendored`
> feature. The recommended `Cargo.toml` uses `mlua = { features = ["lua54", "vendored"] }`,
> which compiles Lua from source via the `lua-src` crate and does not require this package.
> It is listed here as a fallback.

---

## 3. SDL3

**SDL3 is not available in Ubuntu 24.04 apt repositories** (it is available from Ubuntu 25.04+).
There are two approaches — choose one:

### Option A: Bundled via `sdl3-sys` (recommended)

Add the `build-from-source` feature to `sdl3-sys` in `Cargo.toml`:

```toml
sdl3-sys = { version = "0.6.1", features = ["build-from-source"] }
```

This downloads and compiles SDL3 3.4.2 automatically during `cargo build`. No system SDL3 is
required. `cmake` must be installed (already listed above).

### Option B: System install from source

Build SDL3 3.4.2 manually:

```bash
git clone https://github.com/libsdl-org/SDL.git -b release-3.4.2
cd SDL
cmake -B build \
  -DCMAKE_BUILD_TYPE=Release \
  -DSDL_TESTS=OFF \
  -DSDL_EXAMPLES=OFF
cmake --build build -j$(nproc)
sudo cmake --install build
sudo ldconfig
```

Verify:
```bash
pkg-config --modversion sdl3   # should print 3.4.2
```

---

## 4. Cargo Developer Tools

```bash
cargo install cargo-nextest   # faster test runner
cargo install cargo-audit     # dependency security audit
cargo install cargo-flamegraph # profiling (optional)
cargo install cargo-deb       # .deb packaging (Phase 12)
```

---

## 5. Rust Crate Dependencies

These are managed automatically by Cargo. Listed here for reference only — no manual installation required.

| Crate | Version | Purpose |
|-------|---------|---------|
| `mlua` | 0.11.6 | Lua 5.4 VM embedding |
| `sdl3` | 0.17.3 | Window, input, graphics |
| `sdl3-sys` | 0.6.1+SDL-3.4.2 | Raw SDL3 bindings |
| `fontdue` | 0.9.3 | Font rasterization (pure Rust) |
| `freetype` | 0.7.2 | FreeType2 bindings (fallback) |
| `notify` | 8.2.0 | Cross-platform filesystem watching |
| `pcre2` | 0.2.11 | PCRE2 regex bindings |
| `libc` | 0.2.182 | C type definitions |
| `bitflags` | 2.11.0 | Bitflag types |
| `log` | 0.4.29 | Logging facade |
| `env_logger` | 0.11.9 | Logging implementation |
| `thiserror` | 2.0.18 | Custom error derive macros |
| `anyhow` | 1.0.102 | Application error handling |
| `once_cell` | 1.21.3 | Lazy initialization |
| `parking_lot` | 0.12.5 | Fast mutex/rwlock |
| `crossbeam-channel` | 0.5.15 | MPMC channels for threading |

---

## 6. Verification Checklist

After all installations, run:

```bash
rustc --version
cargo --version
pkg-config --libs freetype2
pkg-config --libs pcre2
cmake --version
# If using system SDL3:
pkg-config --modversion sdl3

# Build the project:
cargo build
./target/debug/forge-core
```

---

*Last updated: 2026-03-04*
