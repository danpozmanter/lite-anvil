---
title: Lite-Anvil - Lightweight Code Editor Built in Rust
description: A fast, lightweight code editor built in Rust with built-in LSP for 25+ languages, embedded terminal, Git integration, and 50+ syntax grammars.
---

<div class="hero" markdown>

# Lite-Anvil

<p class="tagline">A lightweight, lightning fast, and powerful code editor built in Rust, with Lua for user plugins.</p>

<div class="button-row">
  <a href="installation/" class="primary">Get Started</a>
  <a href="https://github.com/danpozmanter/lite-anvil" class="secondary">View on GitHub</a>
</div>

<img src="assets/screenshot.png" alt="Lite-Anvil screenshot" class="screenshot">

</div>

## Key Features

<div class="feature-grid" markdown>

<div class="feature-card" markdown>

### Built-in LSP

25+ languages with diagnostics, completion, hover, go-to-definition, references, rename, code actions, formatting, inlay hints, semantic highlighting, and call/type hierarchy.

</div>

<div class="feature-card" markdown>

### Embedded Terminal

Full PTY terminal with ANSI colors, scrollback, color schemes. Open bottom, left, right, or as a tab.

</div>

<div class="feature-card" markdown>

### Integrated Test Runner

Auto-detects Cargo, npm/vitest/jest, pytest, Go, dotnet, Gradle, Maven, sbt, PHPUnit, Make. Run all tests or the current file.

</div>

<div class="feature-card" markdown>

### Git Integration

Branch/status in status bar, tree highlighting, diff views, stage/unstage, commit, push, pull, stash.

</div>

<div class="feature-card" markdown>

### 50+ Syntax Grammars

Rust, Go, Python, TypeScript, C/C++, Java, Kotlin, Scala, F#, C#, Haskell, Zig, Elixir, Erlang, OCaml, Gleam, Dart, Swift, Ruby, and many more.

</div>

<div class="feature-card" markdown>

### Fast & Lightweight

Native Rust core. Sub-second startup. Low memory footprint. All core modules, views, commands, and bundled plugins are pure Rust via mlua.

</div>

<div class="feature-card" markdown>

### Multi-Cursor Editing

Ctrl+D to add next occurrence, Ctrl+Shift+L for all occurrences, Ctrl+Alt+L to turn find matches into cursors.

</div>

<div class="feature-card" markdown>

### Project Workspace Memory

Open files, tabs, splits, and scroll positions restore when switching between projects.

</div>

<div class="feature-card" markdown>

### Bookmarks & Indent Guides

Toggle line bookmarks (Ctrl+F2), navigate with F2. Vertical indent guides at each level. Line sorting, unique, reverse.

</div>

</div>

## Overview

Lite-Anvil is a fork of [Lite XL](https://github.com/lite-xl/lite-xl), rewritten from the ground up in Rust. The core, all views, commands, and bundled plugins are native Rust. User plugins and configuration remain Lua for easy extensibility.

| | |
|---|---|
| **Languages** | 50+ syntax grammars, 25+ built-in LSP configurations |
| **Platform** | Linux, macOS, Windows |
| **License** | MIT |
| **Rust version** | 1.85+ |
