# LSP Support

Lite Anvil includes builtin LSP (Language Server Protocol) configurations for
common languages. When you open a file, the editor matches its syntax to a
server spec and launches the language server automatically.

## Requirements

The language server binary must normally be installed and available on your
`PATH`. Lite Anvil does not install language servers for you. Lite Anvil can
also use servers bundled with installed VS Code extensions for Rust and C#.

## Builtin Language Servers

| Language | Server | Binary | Root markers |
|---|---|---|---|
| Rust | rust-analyzer | auto-detected | `Cargo.toml`, `rust-project.json` |
| C# | Roslyn, csharp-ls, or OmniSharp | auto-detected | `.sln`, `.csproj` |
| F# | fsautocomplete | `fsautocomplete` | `.fsproj`, `.sln` |
| Java | Eclipse JDT.LS | `jdtls` | `pom.xml`, `build.gradle[.kts]` |
| Kotlin | kotlin-language-server | `kotlin-language-server` | `build.gradle[.kts]`, `pom.xml` |
| Python | Pyright | `pyright-langserver` | `pyproject.toml`, `setup.py`, `pyrightconfig.json` |
| Go | gopls | `gopls` | `go.mod`, `go.work` |
| JavaScript | typescript-language-server | `typescript-language-server` | `tsconfig.json`, `jsconfig.json`, `package.json` |
| TypeScript | typescript-language-server | `typescript-language-server` | `tsconfig.json`, `jsconfig.json`, `package.json` |
| TSX | typescript-language-server | `typescript-language-server` | `tsconfig.json`, `jsconfig.json`, `package.json` |
| PHP | Intelephense | `intelephense` | `composer.json` |
| Elixir | elixir-ls | `elixir-ls` | `mix.exs` |
| OCaml | ocamllsp | `ocamllsp` | `.ocamlformat`, `dune-project`, `*.opam` |
| Gleam | gleam lsp | `gleam lsp` | `gleam.toml` |
| Erlang | erlang_ls | `erlang_ls` | `rebar.config`, `erlang.mk` |
| C/C++ | clangd | `clangd` | `.clangd`, `compile_commands.json` |
| Haskell | haskell-language-server | `haskell-language-server --lsp` | `hie.yaml`, `*.cabal`, `stack.yaml` |
| Zig | zls | `zls` | `build.zig` |
| Dart | Dart SDK | `dart language-server` | `pubspec.yaml` |
| Scala | Metals | `metals` | `build.sbt` |
| Swift | SourceKit-LSP | `sourcekit-lsp` | `Package.swift` |
| Ruby | ruby-lsp | `ruby-lsp` | `Gemfile` |
| Julia | LanguageServer.jl | `julia -e 'using LanguageServer; runserver()'` | `Project.toml` |
| Clojure | clojure-lsp | `clojure-lsp` | `deps.edn`, `project.clj` |
| Crystal | Crystalline | `crystalline` | `shard.yml` |
| Lua | lua-language-server | `lua-language-server` | `.luarc.json` |
| Bash | bash-language-server | `bash-language-server start` | `.git` |
| Gossamer | gossamer-lsp (via `gos`) | `gos lsp` | `project.toml` |

All builtin specs fall back to `.git` as a final root marker.

The Rust builtin tries the configured `rust-analyzer` command first, then
installed VS Code Rust Analyzer extension servers. A command that starts but
exits before initialization is rejected so the editor can advance to the next
candidate. This fallback lifecycle is shared by every LSP configuration.

The C# builtin tries the configured OmniSharp command first, then Roslyn on
`PATH`, the Roslyn server bundled with installed VS Code C# extensions,
`csharp-ls`, and lowercase `omnisharp`. Roslyn pull diagnostics and standard
LSP push diagnostics are both supported.
Roslyn session and design-time build logs are stored under the editor's
`logs/roslyn` directory instead of being written into the project.

## Problems and Quick Fixes

Hover an underlined diagnostic to open its problem popup. **Quick Fix** appears
only when the language server returns one or more actions for that exact
diagnostic. Select it, choose an action with the arrow keys, and press Enter to
apply it. `Ctrl+Shift+A` opens code actions for the current cursor or selection
without using the popup.

This behavior is shared by every builtin and custom LSP server. The client
supports diagnostic push and pull, lazy code-action resolution, command-based
actions, and both `changes` and `documentChanges` workspace edits.

## Custom Configuration

Create an `lsp.json` file to add servers or override builtins:

- **User-wide:** `~/.config/lite-anvil/lsp.json`
- **Project-specific:** `<project-root>/lsp.json`

Project settings merge on top of user settings, which merge on top of builtins.

### Format

```json
{
  "server_name": {
    "command": ["binary", "--arg"],
    "filetypes": ["language"],
    "rootPatterns": ["marker_file"],
    "initializationOptions": {},
    "settings": {},
    "env": {},
    "autostart": true
  }
}
```

**Fields:**

- `command` (required) -- string or array of strings
- `filetypes` (required) -- array of lowercase language names matching syntax
  file names (e.g. `"rust"`, `"c#"`, `"f#"`, `"javascript"`)
- `rootPatterns` -- files/directories that identify the project root
- `initializationOptions` -- passed to the server on initialize
- `settings` -- server-specific configuration
- `env` -- environment variables for the server process
- `autostart` -- set to `false` to disable a builtin spec

### Examples

Replace pyright with pylsp:

```json
{
  "pyright": { "command": ["echo"], "filetypes": ["_"], "autostart": false },
  "pylsp": {
    "command": ["pylsp"],
    "filetypes": ["python"],
    "rootPatterns": ["pyproject.toml", "setup.py"]
  }
}
```

Add Scala Metals:

```json
{
  "metals": {
    "command": ["metals"],
    "filetypes": ["scala"],
    "rootPatterns": ["build.sbt", "build.sc"]
  }
}
```
