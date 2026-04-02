---
title: API Reference
description: Native Rust API exposed to Lua plugins in Lite-Anvil, with EmmyLua type annotations.
---

# API Reference

Lite-Anvil exposes a native Rust API to Lua plugins. The annotation files in
[`docs/api/`](https://github.com/danpozmanter/lite-anvil/tree/main/docs/api)
contain [EmmyLua annotations](https://emmylua.github.io/annotation.html)
for use with LSP servers like
[lua-language-server](https://github.com/LuaLS/lua-language-server).
Point your LSP at that directory to get autocompletion and type checking
when writing plugins or editing `config.lua`.

## Native Modules

| Module | Description | Source |
|--------|-------------|--------|
| `system` | File system, clipboard, window management, events | [system.lua](https://github.com/danpozmanter/lite-anvil/blob/main/docs/api/system.lua) |
| `renderer` | Drawing primitives, font loading and measurement | [renderer.lua](https://github.com/danpozmanter/lite-anvil/blob/main/docs/api/renderer.lua) |
| `regex` | PCRE2 regular expressions | [regex.lua](https://github.com/danpozmanter/lite-anvil/blob/main/docs/api/regex.lua) |
| `process` | Child process spawning and stream I/O | [process.lua](https://github.com/danpozmanter/lite-anvil/blob/main/docs/api/process.lua) |
| `renwindow` | Window creation and persistence | [renwindow.lua](https://github.com/danpozmanter/lite-anvil/blob/main/docs/api/renwindow.lua) |
| `dirmonitor` | File system change monitoring | [dirmonitor.lua](https://github.com/danpozmanter/lite-anvil/blob/main/docs/api/dirmonitor.lua) |
| `utf8extra` | UTF-8 string utilities | [utf8extra.lua](https://github.com/danpozmanter/lite-anvil/blob/main/docs/api/utf8extra.lua) |
| `string` (u* extensions) | UTF-8 methods injected into the string table | [string.lua](https://github.com/danpozmanter/lite-anvil/blob/main/docs/api/string.lua) |

## Globals

All global variables set by the runtime (`ARGS`, `SCALE`, `PLATFORM`, `USERDIR`, `DATADIR`, `PATHSEP`, `VERSION`, etc.) are documented in [globals.lua](https://github.com/danpozmanter/lite-anvil/blob/main/docs/api/globals.lua).

## Core Modules

These modules are implemented in Rust but exposed as standard `require`-able Lua modules:

### Application

| Module | Description |
|--------|-------------|
| `core` | Application lifecycle, threads, logging, projects, file dialogs |
| `core.command` | Command registry (`add`, `perform`, predicates) |
| `core.keymap` | Keybinding management |
| `core.config` | Editor settings and plugin configuration |
| `core.style` | Colors, fonts, theme registration |
| `core.common` | Utility functions (paths, fuzzy match, serialize, colors) |
| `core.storage` | Persistent key-value storage across restarts |

### Document

| Module | Description |
|--------|-------------|
| `core.doc` | Document model (buffer, selections, undo/redo) |
| `core.doc.translate` | Cursor movement helpers |
| `core.doc.search` | Text search with regex/plain/wrap support |
| `core.syntax` | Syntax grammar registry |

### UI

| Module | Description |
|--------|-------------|
| `core.object` | OOP base class (extend, new, is, extends) |
| `core.view` | Base UI view class |
| `core.docview` | Code editor view |
| `core.commandview` | Command palette |
| `core.contextmenu` | Right-click menus |
| `core.nagview` | Confirmation dialogs |
| `core.scrollbar` | Scrollbar component |
| `core.statusview` | Status bar |
| `core.logview` | Log viewer |

### System

| Module | Description |
|--------|-------------|
| `core.dirwatch` | Directory change watcher |
| `core.process` | Process streams with coroutine-aware I/O |
| `core.project` | Project file listing and filtering |
| `core.gitignore` | .gitignore pattern matching |
| `core.regex` | Regex helpers (find, match, find_offsets) |
| `core.ime` | Input method editor hooks |
| `core.plugin_api` | Stable facade for plugin authors |

For full details, see the [Plugin Guide](https://github.com/danpozmanter/lite-anvil/blob/main/PLUGINS_GUIDE.md).
