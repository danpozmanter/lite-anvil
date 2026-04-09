---
title: User Guide - Lite-Anvil
description: Keyboard shortcuts, LSP language server setup, configuration, and syntax highlighting for Lite-Anvil.
---

# User Guide

## Keyboard Shortcuts

### General

| Key | Action |
|-----|--------|
| `Ctrl+P` | Command palette |
| `Ctrl+O` | Open file (supports `file:42` to go to line) |
| `Ctrl+Shift+O` | Open file from project |
| `Ctrl+N` | New file |
| `Ctrl+S` | Save (save-as for unnamed files) |
| `Ctrl+W` | Close tab |
| `Ctrl+Q` | Quit |
| `Ctrl+=` / `Ctrl+-` | Font zoom in / out |
| `F10` | Toggle line wrapping |

### Editing

| Key | Action |
|-----|--------|
| `Ctrl+D` | Add next occurrence to selection |
| `Ctrl+Shift+L` | Select all occurrences |
| `Ctrl+Alt+L` | Turn find matches into multi-cursors |
| `Ctrl+/` | Toggle line comment |
| `Ctrl+Shift+Up/Down` | Move line up/down |
| `Ctrl+Shift+D` | Duplicate line |
| `Alt+Shift+F` | Format document (LSP) |

### Find & Replace

| Key | Action |
|-----|--------|
| `Ctrl+F` | Find in file |
| `Ctrl+H` | Replace in file |
| `Alt+S` | Toggle find-in-selection |
| `F3` / `Shift+F3` | Next / previous match |

### Bookmarks

| Key | Action |
|-----|--------|
| `Ctrl+F2` | Toggle bookmark |
| `F2` | Next bookmark |
| `Shift+F2` | Previous bookmark |

### LSP

| Key | Action |
|-----|--------|
| `F12` | Go to definition |
| `Ctrl+F12` | Go to type definition |
| `Shift+F12` | Find references |
| `F2` | Rename symbol |
| `Ctrl+Space` | Trigger completion |
| `Ctrl+Shift+Space` | Signature help |
| `Ctrl+K` | Hover |
| `Ctrl+T` | Document symbols |
| `Ctrl+Alt+T` | Workspace symbols |
| `Ctrl+Shift+A` | Code action |
| `Alt+Return` | Quick fix |
| `F8` / `Shift+F8` | Next / previous diagnostic |
| `Alt+F12` | Incoming calls |
| `Ctrl+Shift+F12` | Outgoing calls |
| `Alt+F11` | Supertypes |
| `Ctrl+Shift+F11` | Subtypes |

### Test Runner

| Key | Action |
|-----|--------|
| `Ctrl+Shift+R` | Run all tests |

Also available: `test:run-file` from the command palette.

## Command Palette

Press `Ctrl+P` to open the command palette. All commands are searchable. Key commands:

- `lines:sort`, `lines:reverse`, `lines:unique`, `lines:sort-case-insensitive`
- `bookmarks:toggle`, `bookmarks:next`, `bookmarks:previous`, `bookmarks:clear`
- `indent-guide:toggle`
- `minimap:toggle`
- `treeview:refresh`
- `workspace:clear-project-memory`, `workspace:clear-recents`

## Sidebar Context Menu

Right-click a file or folder in the sidebar:

- **Open** -- open the file
- **Copy Path** -- copy absolute path to clipboard
- **Copy Relative Path** -- copy project-relative path
- **Refresh** -- rescan the project tree
- **Rename** / **Delete** -- file operations
- **New File** / **New Folder** -- create in the selected directory

## LSP Support

Lite-Anvil includes built-in configurations for the following language servers. Install the binary and it works automatically -- no configuration needed.

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

Lite-Anvil is configured via a TOML file. Open it from the sidebar settings icon or via the command palette: **Open User Settings**.

### Config location

| Platform | Path |
|----------|------|
| Linux | `~/.config/lite-anvil/config.toml` |
| macOS | `~/Library/Application Support/lite-anvil/config.toml` |
| Windows | `%APPDATA%\lite-anvil\config.toml` |

### Command line options

```
lite-anvil [file...]          Open files
lite-anvil -v                 Verbose mode (log LSP errors to stderr)
lite-anvil --verbose          Same as -v
```

### Themes

Cycle themes with `Ctrl+Shift+P` or the command palette. JSON theme files are in `data/assets/themes/`.
