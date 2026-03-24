# API Reference

This directory holds documentation for the native Rust API exposed to Lua
plugins. These files contain no executable code — only
[EmmyLua annotations](https://emmylua.github.io/annotation.html)
for use with LSP servers like
[lua-language-server](https://github.com/LuaLS/lua-language-server).
Point your LSP at this directory to get autocompletion and type checking
when writing plugins or editing `config.lua`.

## Native Modules

| Module | Description |
|--------|-------------|
| [system](api/system.lua) | File system, clipboard, window management, events |
| [renderer](api/renderer.lua) | Drawing primitives, font loading and measurement |
| [regex](api/regex.lua) | PCRE2 regular expressions |
| [process](api/process.lua) | Child process spawning and stream I/O |
| [renwindow](api/renwindow.lua) | Window creation and persistence |
| [dirmonitor](api/dirmonitor.lua) | File system change monitoring |
| [utf8extra](api/utf8extra.lua) | UTF-8 string utilities |
| [string (u\* extensions)](api/string.lua) | UTF-8 methods injected into the string table |

## Globals

All global variables set by the runtime are documented in
[globals.lua](api/globals.lua).

## Core Modules (Lua)

These modules are implemented in Rust but exposed as standard `require`-able
Lua modules. They are not annotated here — refer to the source or the
[PLUGINS_GUIDE.md](../PLUGINS_GUIDE.md) (when available) for their API:

- `core` — application lifecycle, threads, logging, projects, file dialogs
- `core.command` — command registry (`add`, `perform`, predicates)
- `core.keymap` — keybinding management
- `core.config` — editor settings and plugin configuration
- `core.style` — colors, fonts, theme registration
- `core.common` — utility functions (paths, fuzzy match, serialize, colors)
- `core.doc` — document model (buffer, selections, undo/redo)
- `core.doc.translate` — cursor movement helpers
- `core.doc.search` — text search with regex/plain/wrap support
- `core.object` — OOP base class (extend, new, is, extends)
- `core.view` — base UI view class
- `core.docview` — code editor view
- `core.commandview` — command palette
- `core.contextmenu` — right-click menus
- `core.nagview` — confirmation dialogs
- `core.scrollbar` — scrollbar component
- `core.dirwatch` — directory change watcher
- `core.process` — process streams with coroutine-aware I/O
- `core.project` — project file listing and filtering
- `core.gitignore` — .gitignore pattern matching
- `core.storage` — persistent key-value storage across restarts
- `core.plugin_api` — stable facade for plugin authors
- `core.regex` — regex helpers (find, match, find_offsets)
- `core.ime` — input method editor hooks
