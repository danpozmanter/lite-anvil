---
title: User Guide - Lite Anvil
description: Keyboard shortcuts, LSP language server setup, configuration, and syntax highlighting for Lite Anvil.
---

# User Guide

## Keyboard Shortcuts

### General

| Key | Action |
|-----|--------|
| `Ctrl+P` | Command palette |
| `Ctrl+O` | Open file (supports `file:42` to go to line) |
| `Ctrl+Shift+O` | Open file from project |
| `Ctrl+Shift+R` | Open recent file / project |
| `Ctrl+N` | New file |
| `Ctrl+S` | Save (save-as for unnamed files) |
| `Ctrl+Shift+S` | Save as |
| `Ctrl+W` | Close tab |
| `Ctrl+Q` | Quit |
| `Ctrl+=` / `Ctrl+-` | Increase / decrease font size |
| `Ctrl+0` | Reset font size |
| `Ctrl+B` | Toggle sidebar |
| `Ctrl+M` | Toggle minimap |
| `Alt+Z` | Toggle line wrapping |
| `Ctrl+Shift+H` | Toggle whitespace rendering |
| `Ctrl+Shift+P` | Cycle theme |
| `F11` | Toggle fullscreen |
| `Ctrl+`` ` / `F5` | Toggle terminal |
| `Ctrl+Shift+T` | New terminal |

### Editing

| Key | Action |
|-----|--------|
| `Ctrl+D` | Select word / add next occurrence |
| `Ctrl+L` | Select line |
| `Ctrl+A` | Select all |
| `Ctrl+/` | Toggle line comment (language-aware) |
| `Ctrl+Up` / `Ctrl+Down` | Move line up / down |
| `Ctrl+Shift+D` | Duplicate line |
| `Ctrl+Shift+K` | Delete line |
| `Ctrl+J` | Join lines |
| `Ctrl+Shift+Up/Down` | Add cursor above / below |
| `Ctrl+Shift+[` | Fold code block |
| `Ctrl+Shift+]` | Unfold code block |
| `Ctrl+Shift+\` | Unfold all |

### Find & Replace

| Key | Action |
|-----|--------|
| `Ctrl+F` | Find in file |
| `Alt+F` | Replace in file |
| `F3` / `Enter` | Next match |
| `Shift+F3` / `Shift+Enter` | Previous match |
| `Alt+R` | Toggle regex (inside find bar) |
| `Alt+W` | Toggle whole word (inside find bar) |
| `Alt+I` | Toggle case-insensitive (inside find bar) |
| `Alt+S` | Toggle find-in-selection (inside find bar) |
| `Ctrl+Enter` | Replace current match and find next |
| `Ctrl+Shift+F` | Find in files (project search) |
| `Alt+Shift+F` | Replace in files (project replace) |

### LSP

| Key | Action |
|-----|--------|
| `F12` | Go to definition |
| `Ctrl+F12` | Go to implementation |
| `Shift+F12` | Find references |
| `Ctrl+Shift+F12` | Go to type definition |
| `Ctrl+K` | Hover |

### Bookmarks

| Key | Action |
|-----|--------|
| `Ctrl+F4` | Toggle bookmark on current line |
| `F4` | Jump to next bookmark |
| `Shift+F4` | Jump to previous bookmark |

Bookmarked lines show an accent-colored marker in the gutter. Bookmarks wrap around and are per-document.

### Navigation

| Key | Action |
|-----|--------|
| `Ctrl+G` | Go to line |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+PageUp` / `Ctrl+PageDown` | Move tab left / right |
| `Ctrl+Shift+G` | Git status |

## Command Palette

Press `Ctrl+P` to open the command palette. All commands are searchable. The palette filters out raw key-input commands and only shows meaningful actions. Git commands are prefixed with "Git" (e.g. `Git Pull`, `Git Push`, `Git Commit`, `Git Stash`).

Additional commands available from the palette:

- `Sort Lines`, `Upper Case`, `Lower Case`
- `Open User Settings`
- `Toggle Minimap`, `Toggle Line Wrapping`, `Toggle Whitespace`
- `Toggle Hidden Files` -- show/hide dotfiles in the sidebar
- `Check For Updates` -- query GitHub for a newer release
- `Git Pull`, `Git Push`, `Git Commit`, `Git Stash`, `Git Status`
- `Git Blame` -- toggle per-line blame annotations (author + date)
- `Git Log` -- show the last 50 commits for the active file
- `Close All`, `Close All Others`

## Sidebar

File icons in the sidebar are color-coded by extension (e.g. Rust files in red, Python in blue, Go in cyan). Colors are loaded from `data/assets/file_icons.json`. Hidden files (dotfiles) are hidden by default; toggle visibility with `Toggle Hidden Files` from the command palette.

## Sidebar Context Menu

Right-click a file or folder in the sidebar:

- **Open** -- open the file
- **Copy Path** -- copy absolute path to clipboard
- **Copy Relative Path** -- copy project-relative path
- **Refresh** -- rescan the project tree
- **Rename** / **Delete** -- file operations
- **New File** / **New Folder** -- create in the selected directory

## LSP Support

Lite Anvil includes built-in configurations for the following language servers. Install the binary and it works automatically -- no configuration needed.

### Recommended Language Servers

| Language | Server | Install |
|----------|--------|---------|
| Rust | `rust-analyzer` | `rustup component add rust-analyzer` |
| Python | `pyright-langserver` | `pip install pyright` or `npm install -g pyright` |
| Go | `gopls` | `go install golang.org/x/tools/gopls@latest` |
| JavaScript / TypeScript / TSX | `typescript-language-server` | `npm install -g typescript-language-server typescript` |
| C / C++ | `clangd` | Package manager (e.g. `apt install clangd`) |
| Java | `jdtls` | [Eclipse JDT.LS](https://github.com/eclipse-jdtls/eclipse.jdt.ls) |
| Kotlin | `kotlin-language-server` | [GitHub releases](https://github.com/fwcd/kotlin-language-server) |
| C# | `OmniSharp` | `dotnet tool install -g OmniSharp` |
| F# | `fsautocomplete` | `dotnet tool install -g fsautocomplete` |
| Scala | `metals` | [Metals](https://scalameta.org/metals/docs/editors/new-editor) |
| PHP | `intelephense` | `npm install -g intelephense` |
| Ruby | `ruby-lsp` | `gem install ruby-lsp` |
| Lua | `lua-language-server` | [GitHub releases](https://github.com/LuaLS/lua-language-server) |
| Bash | `bash-language-server` | `npm install -g bash-language-server` |
| Zig | `zls` | [ZLS](https://github.com/zigtools/zls) |
| Haskell | `haskell-language-server` | `ghcup install hls` |
| Elixir | `elixir-ls` | [ElixirLS](https://github.com/elixir-lsp/elixir-ls) |
| Erlang | `erlang_ls` | [erlang_ls](https://github.com/erlang-ls/erlang_ls) |
| OCaml | `ocamllsp` | `opam install ocaml-lsp-server` |
| Gleam | `gleam lsp` | Bundled with `gleam` CLI |
| Dart | `dart language-server` | Bundled with Dart SDK |
| Swift | `sourcekit-lsp` | Bundled with Swift toolchain |
| Julia | `LanguageServer.jl` | `julia -e 'using Pkg; Pkg.add("LanguageServer")'` |
| Clojure | `clojure-lsp` | [GitHub releases](https://github.com/clojure-lsp/clojure-lsp) |
| Crystal | `crystalline` | [Crystalline](https://github.com/elbywan/crystalline) |

### Custom LSP Configuration

Create `lsp.json` in your user config directory or project root to add servers or override builtins:

```json
{
  "my_server": {
    "command": ["my-lsp", "--stdio"],
    "filetypes": ["mylang"],
    "rootPatterns": ["myproject.toml", ".git"]
  }
}
```

Set `"autostart": false` to disable a builtin server.

## Syntax Highlighting

51 built-in syntax grammars:

| Category | Languages |
|----------|-----------|
| **Systems** | Assembly, C, C++, D, Rust, Zig |
| **JVM** | Java, Kotlin, Scala, Groovy, Clojure |
| **Web** | JavaScript, TypeScript, TSX, HTML, CSS, Vue, Svelte, PHP |
| **.NET** | C#, F# |
| **Scripting** | Python, Ruby, Lua, Bash, PowerShell, R, Lisp |
| **Functional** | Haskell, OCaml, Elixir, Erlang, Gleam, Julia, Crystal |
| **Other** | Go, Dart, Swift |
| **Data/Config** | JSON (via JS), TOML, YAML, INI, XML, CSV, Env, Fstab, SQL, PostgreSQL, Meson |
| **Markup** | Markdown, Dockerfile |

XML highlighting also covers `.csproj`, `.fsproj`, `.vbproj`, `.vcxproj`, `.sln`, `.props`, `.targets`, `.nuspec`, `.pom`, `.svg`, `.plist`, `.xaml`.

Groovy highlighting covers `.gradle` files. Kotlin highlighting covers `.gradle.kts` files (via `.kts` extension).

Toggle line comments (`Ctrl+/`) automatically picks the correct marker for the active language -- `//` for Rust/C/JS, `#` for Python/Bash/TOML, `--` for Lua/SQL/Haskell, `;` for Assembly/Lisp/INI, `%` for Erlang, and block-comment wrapping (`<!-- -->` for HTML/Markdown/XML/Vue, `/* */` for CSS, `(* *)` for OCaml) for languages without a line-comment form.

## Test Runner

The test runner auto-detects your project's framework and runs tests in a terminal pane.

| Language | Detection | Tool | Run All | Run File |
|----------|-----------|------|---------|----------|
| Rust | `Cargo.toml` | cargo | `cargo test` | `cargo test <module>` |
| JavaScript / TypeScript | `package.json` | vitest, jest, or npm | `npx vitest run` / `npx jest` / `npm test` | `<runner> <file>` |
| Python | `pytest.ini`, `conftest.py`, `pyproject.toml` | pytest or unittest | `python -m pytest -v` | `pytest <file>` |
| Go | `go.mod` | go test | `go test ./...` | `go test -v ./<dir>` |
| C# / F# | `.sln`, `.csproj`, `.fsproj` | dotnet test | `dotnet test` | `dotnet test --filter <class>` |
| Java / Kotlin / Scala | `build.gradle[.kts]` | Gradle | `./gradlew test` | `./gradlew test --tests "*Class*"` |
| Java / Kotlin / Scala | `pom.xml` | Maven | `mvn test` | `mvn test -Dtest="Class"` |
| Scala | `build.sbt` | sbt | `sbt test` | `sbt "testOnly *Class*"` |
| PHP | `phpunit.xml[.dist]` | PHPUnit | `./vendor/bin/phpunit` | `phpunit <file>` |
| Any | `Makefile` | make | `make test` | -- |

## Configuration

Lite Anvil is configured via a TOML file. Open it from the sidebar settings icon or via the command palette: **Open User Settings**.

### Config location

| Platform | Path |
|----------|------|
| Linux | `~/.config/lite-anvil/config.toml` |
| macOS | `~/Library/Application Support/lite-anvil/config.toml` |
| Windows | `%APPDATA%\lite-anvil\config.toml` |

### Key options

```toml
theme = "dark_default"         # dark_default, light_default, fall, summer, textadept
indent_size = 2
tab_type = "soft"              # "soft" (spaces) or "hard" (tabs)
line_endings = "lf"            # "lf" or "crlf"
line_limit = 80
line_height = 1.2
highlight_current_line = true
draw_whitespace = false
borderless = false
max_tabs = 8
blink_period = 0.8
disable_blink = false
mac_command_as_ctrl = true     # macOS: fold Cmd into Ctrl (on by default on Mac)
format_on_paste = true         # convert pasted indent to match document style

[lsp]
load_on_startup = true
semantic_highlighting = true
inline_diagnostics = true
format_on_save = true

[terminal]
placement = "bottom"
reuse_mode = "pane"

[ui]
padding_x = 14
padding_y = 7
caret_width = 2
scrollbar_size = 4
tab_width = 170

[fonts.ui]
path = "/path/to/your/font.ttf"
size = 15

[fonts.code]
path = "/path/to/your/mono.ttf"
size = 15
```

Custom keybindings can be added under `[keybindings]`:

```toml
[keybindings]
"ctrl+shift+l" = "doc:select-lines"
"alt+shift+f" = "lsp:format-document"
```

### Command line options

```
lite-anvil [file...]          Open files
lite-anvil file.rs:42         Open file at line 42
lite-anvil -v                 Verbose mode (log LSP errors to stderr)
lite-anvil --verbose          Same as -v
```

The `:N` line-number suffix also works in the file picker (`Ctrl+O`).

### Themes

Cycle themes with `Ctrl+Shift+P` or the command palette. JSON theme files are in `data/assets/themes/`.

## Nano Anvil

Nano Anvil is a minimal single-file editor included alongside Lite Anvil.

### Running Nano Anvil

```bash
# From the release archive
./nano-anvil

# Open a specific file
./nano-anvil myfile.rs
```

On Linux, Nano Anvil is also available as a desktop application (Nano Anvil in your application menu).

### Differences from Lite Anvil

Nano Anvil is designed for lightweight, single-file editing:

- Single file at a time (no tabs, no project)
- No sidebar, terminal, LSP, git integration, bookmarks, or code folding
- Software rendering (no GPU driver loaded)
- ~28MB RAM on Linux vs ~100MB for Lite Anvil (on NVIDIA/X11)
- Syntax highlighting for 50+ languages (shared with Lite Anvil)
- Find and replace within the current file
- Full undo/redo (capped at 100 entries)
- 2 built-in themes (dark + light)

### Keyboard Shortcuts (Nano Anvil)

| Key | Action |
|-----|--------|
| `Ctrl+O` | Open file |
| `Ctrl+S` | Save (prompts for path if untitled) |
| `Ctrl+Shift+S` | Save As |
| `Ctrl+Q` | Quit |
| `Ctrl+F` | Find |
| `Alt+F` | Find and Replace |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` | Redo |
| `Ctrl+A` | Select All |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copy / Cut / Paste |
| `Ctrl+P` | Command Palette |
| `Ctrl+=` / `Ctrl+-` | Zoom In / Out |
