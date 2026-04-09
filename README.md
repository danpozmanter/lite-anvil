# Lite-Anvil

[![Build](https://github.com/danpozmanter/lite-anvil/actions/workflows/release.yml/badge.svg)](https://github.com/danpozmanter/lite-anvil/actions/workflows/release.yml)

**[Documentation](https://danpozmanter.github.io/lite-anvil/)** | **[Releases](https://github.com/danpozmanter/lite-anvil/releases)**

A fast and lightweight code editor built in Rust with SDL3.

Lite-Anvil is a fork of [Lite XL](https://github.com/lite-xl/lite-xl), rewritten from the ground up in Rust.

## Purpose & Forking

This project exists partially as an experiment, and partially as something I just wanted for myself.

**No Support**

I do not intend to maintain or support this in any way, but wanted to share the code so anyone interested can freely use, learn from, or fork this project into something new.

## Features

- **Built-in LSP** with diagnostics, completion, hover, go-to-definition, references, inlay hints
- **Embedded PTY terminal** with ANSI colors, scrollback, and multi-terminal support
- **Indent guides** at each indentation level
- **Line sorting** on selected lines
- **Project-wide search** (Ctrl+Shift+F) with grep-based results
- **Git gutter markers** showing added, modified, and deleted lines
- **Code folding** with indent-based fold detection
- **Native file watching** via inotify for external-change detection
- **Single-file find and replace**
- **Minimap** with syntax-colored blocks, click to scroll
- **Session restore** -- open files, active tab, font scale persist across restarts
- **50 built-in syntax grammars** including Rust, Go, Python, TypeScript, C, C++, Java, and more
- **JSON-backed color themes** (`data/assets/themes/*.json`) with runtime cycling (Ctrl+Shift+P)
- **Keyboard-navigated file/folder open** with filesystem autocomplete

## Shortcuts

| Key | Action |
|-----|--------|
| `Ctrl+P` | Command palette |
| `Ctrl+O` | Open file (autocomplete navigator) |
| `Ctrl+Shift+O` | Open project folder |
| `Ctrl+Shift+F` | Project-wide search |
| `Ctrl+Shift+P` | Cycle color theme |
| `Ctrl+=` / `Ctrl+-` | Font zoom in / out |
| `Ctrl+F` | Find in file |
| `Ctrl+H` | Replace in file |
| `Ctrl+M` | Toggle minimap |
| `Alt+Z` | Toggle line wrapping |
| `Ctrl+B` | Toggle sidebar |
| `F5` | Toggle terminal |
| `F12` | Go to definition (LSP) |
| `Ctrl+K` | Hover info (LSP) |
| `Ctrl+Shift+[` | Fold code block |
| `Ctrl+Shift+]` | Unfold code block |
| `Ctrl+W` | Close tab |
| `Ctrl+Tab` | Next tab |

## Building

### Quick start

```bash
# Ubuntu / Debian
apt install libsdl3-dev libfreetype6-dev libpcre2-dev

# Build
cargo build --release

# Run
./target/release/lite-anvil [path]
```

Rust 1.85+ required. See [BUILDING.md](BUILDING.md) for full instructions
including macOS, Windows, and packaging.

## License

MIT -- see [LICENSE](LICENSE).

Font: [Lilex](https://github.com/mishamyrt/Lilex)
