# Lite-Anvil

[![Build](https://github.com/danpozmanter/lite-anvil/actions/workflows/release.yml/badge.svg)](https://github.com/danpozmanter/lite-anvil/actions/workflows/release.yml)

A lightweight code editor built in Rust, with Lua for user plugins.

Lite-Anvil is a fork of [Lite XL](https://github.com/lite-xl/lite-xl), rewritten from the ground up in Rust.

## Purpose & Forking

This project exists partially as an experiment, and partially as something I just wanted for myself.

**No Support**

I do not intend to maintain or support this in any way, but wanted to share the code so anyone interested can freely use, learn from, or fork this project into something new.

## Features

- **Mostly native Rust** — all core modules, views, commands, and bundled plugins are pure Rust via mlua. User plugins and config are Lua
- **Built-in LSP** with diagnostics, inline diagnostics, semantic highlighting, completion, hover, go-to-definition, references, rename, symbols, code actions, formatting, snippets, and signature help
- **Embedded PTY terminal** with ANSI colors, scrollback, color schemes, and configurable placement
- **Project-wide search, replace, and swap** plus native single-file find and replace
- **Git integration** — branch/status in UI, tree highlighting, status view, diff views
- **Multi-cursor editing**, command palette, project file picker, split panes
- **Find in selection** — limit search to the selected region (`Alt+S` to toggle)
- **Minimap** — optional code overview sidebar with syntax-colored blocks, click/drag to scroll (`minimap:toggle`)
- **Tab reordering** — drag tabs within a pane to reorder
- **Smart undo** — consecutive typing merges into a single undo entry; Ctrl+Z undoes the entire run
- **Session restore** — open files, active tab, line wrapping preference, font scale, and terminal state persist across restarts
- **JSON-backed color themes** (`data/assets/themes/*.json`) — editable without recompiling
- **48 built-in syntax grammars** including Rust, Go, Python, TypeScript, TSX, Vue, Svelte, Zig, Haskell, Julia, Lisp, OCaml, PowerShell, and more
- **"Open With" file associations** on Linux, macOS, and Windows for all supported file types
- **Remote SSH editing** via `sshfs`
- **Config-driven** UI theming, fonts, syntax colors, and behavior tuning through [`config.lua`](PLUGINS_GUIDE.md)

## Editing Workflows

### Autocomplete modes

`config.plugins.autocomplete.mode`:

- `lsp` (default): LSP completion items, triggered by `.`, `::`, etc.
- `in_document`: symbols from the current document only
- `totally_on`: symbols from all open documents plus syntax keywords
- `off`: disables suggestions

### Multi-cursor editing

- `Ctrl+D` / `Cmd+D`: add next occurrence of selection
- `Ctrl+Shift+L` / `Cmd+Shift+L`: select all occurrences
- `Ctrl+Alt+L` / `Cmd+Option+L`: after Find, turn matches into multi-cursors

### Remote SSH editing

Mount a remote path with `sshfs`, then open it as a project.

1. Command palette → `Remote Ssh Open Project` or `Remote Ssh Add Project`
2. Enter `user@host:/absolute/path`
3. Edit normally. Mount is cleaned up when the project is removed.

Requires `sshfs` installed and working SSH authentication.

### Useful shortcuts

| Key | Action |
|-----|--------|
| `Ctrl+P` | Command palette |
| `Ctrl+Shift+O` | Open file from project |
| `Ctrl+Alt+O` | Open project folder |
| `Ctrl+T` | Document symbols (LSP) |
| `Ctrl+Alt+T` | Workspace symbols (LSP) |
| `Ctrl+=` / `Ctrl+-` | Font zoom in / out |
| `F10` | Toggle line wrapping |
| `Ctrl+F` | Find in file |
| `Alt+S` | Toggle find-in-selection (while Find is open) |
| `Ctrl+H` | Replace in file |

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
including macOS, Windows, packaging, and file association setup.

## Plugins & Configuration

Lite-Anvil is extensible via Lua plugins in `~/.config/lite-anvil/plugins/`.
See [PLUGINS_GUIDE.md](PLUGINS_GUIDE.md) for the full API reference, config
options, and copy-paste recipes.

## License

MIT — see [LICENSE](LICENSE).

Font: [Lilex](https://github.com/mishamyrt/Lilex)
