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

- Full Lua 5.4 plugin API, preserving Lite XL plugin compatibility while replacing the C core with Rust
- Native Rust core for tokenization, document buffers and undo/redo, search and replace, project scanning, tree/file state, Git status, terminal emulation, picker ranking, and LSP transport/state hot paths
- Built-in LSP with diagnostics, inline diagnostics, semantic highlighting, completion, hover, go-to-definition, references, rename, symbols, code actions, formatting, and signature help
- Embedded PTY terminal with ANSI colors, scrollback, rename, color schemes, auto-close on exit, and configurable open placement
- Project-wide search, regex search, replace, and swap operations, plus native single-file find and replace
- Git integration with branch/status in the UI, repo-aware tree highlighting, status view, and diff views
- Multi-cursor editing, command palette, project file picker, split panes, and session restore for files and terminals
- Config-driven UI theming, fonts, syntax colors, and behavior tuning through `config.lua`
- Broad built-in syntax highlighting, including PowerShell, TSX, Vue, Svelte, Zig, Haskell, Julia, Lisp, OCaml, PostgreSQL, and more
- Remote SSH project editing via `sshfs`

## Editing Workflows

### Autocomplete modes

Lite-Anvil supports a small set of autocomplete source modes through `config.plugins.autocomplete.mode`.

- `lsp`: Uses LSP completion items. This is the default. Suggestions appear when the language server's trigger characters are typed, such as `.` or `::`, and can also be opened manually.
- `in_document`: Uses symbols collected from the current document only.
- `totally_on`: Uses symbols from all open documents plus built-in syntax symbols.
- `off`: Disables automatic autocomplete suggestions.

### Multi-cursor editing

Lite-Anvil supports "select many, edit once" workflows.

- `Ctrl+D` / `Cmd+D`: add the next occurrence of the current selection
- `Ctrl+Shift+L` / `Cmd+Shift+L`: select all occurrences of the current selection at once
- `Ctrl+Alt+L` / `Cmd+Option+L`: after `Ctrl+F` / `Cmd+F`, turn the current find term into multi-cursors for every match in the file

Typical flow:

1. Select a word or phrase, or run Find with `Ctrl+F` / `Cmd+F`.
2. Use `Ctrl+D` / `Cmd+D` to grow one match at a time, or `Ctrl+Shift+L` / `Cmd+Shift+L` to grab every occurrence of the current selection.
3. If you used Find, press `Ctrl+Alt+L` / `Cmd+Option+L` to convert all matches of the current find term into simultaneous selections.
4. Type once to edit all selected matches together.

### Remote SSH editing

Remote editing is implemented by mounting a remote path locally with `sshfs`, then opening that mount as a normal project.

Requirements:

- `sshfs` must be installed on the machine running Lite-Anvil
- SSH authentication should already work non-interactively, or be handled by your SSH agent

Usage:

1. Open the command palette.
2. Run `Remote Ssh Open Project` to replace the current project with a remote one, or `Remote Ssh Add Project` to add a second remote project.
3. Enter a remote spec in the form `user@host:/absolute/path`.
4. Browse and edit files normally in the tree view and editor.

The mount is cleaned up when the remote project is removed from the session.

Useful shortcuts:

- `Ctrl+P` runs the command palette.
- `Ctrl+Shift+O` opens a file from the current project.
- `Ctrl+Alt+O` opens a project folder.
- `Ctrl+T` shows document symbols through LSP when available.
- `Ctrl+Alt+T` shows workspace symbols through LSP when available.

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

### macOS notes

If macOS reports `Code Signature Invalid` after install, re-sign the app bundle
locally with an ad hoc signature:

```bash
codesign --force --deep --sign - --timestamp=none /Applications/LiteAnvil.app
```

If the app was quarantined by Gatekeeper, remove the quarantine attribute:

```bash
sudo xattr -dr com.apple.quarantine /Applications/LiteAnvil.app
```

### Other Notes

Font: [Lilex](https://github.com/mishamyrt/Lilex)

## License

MIT — see [LICENSE](LICENSE).
