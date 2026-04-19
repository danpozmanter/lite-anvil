---
title: Lite Anvil - Code Editor Built in Rust
description: Fast code editor built in Rust with SDL3. Built-in LSP, embedded terminal, Git integration, and 50+ syntax grammars.
---

<div class="hero" markdown>

# Lite Anvil

<p class="tagline">A fast code editor built in Rust with SDL3.</p>

<div class="button-row">
  <a href="installation/" class="primary">Get Started</a>
  <a href="https://github.com/danpozmanter/lite-anvil" class="secondary">View on GitHub</a>
</div>

<img src="assets/screenshot.png" alt="Lite Anvil screenshot" class="screenshot">

</div>

## Key Features

<div class="feature-grid" markdown>

<div class="feature-card" markdown>

### Built-in LSP

Diagnostics, completion, hover, go-to-definition, references, inlay hints, and more for Rust, Python, TypeScript, Go, C/C++, and other languages.

</div>

<div class="feature-card" markdown>

### Embedded Terminal

Full PTY terminal with ANSI/VT100 colors, scrollback, and multi-terminal support.

</div>

<div class="feature-card" markdown>

### Git Integration

Git status view, gutter markers, per-line blame annotations, file commit log, push, pull, commit, stash from the command palette.

</div>

<div class="feature-card" markdown>

### 50+ Syntax Grammars

Rust, Go, Python, TypeScript, C/C++, Java, Kotlin, Scala, F#, C#, Haskell, Zig, Elixir, Erlang, OCaml, Gleam, and many more.

</div>

<div class="feature-card" markdown>

### Fast

Pure Rust. Sub-second startup. Low memory footprint. Ring-buffer terminal scrollback, glyph cache with ASCII pre-warming, merged undo entries.

</div>

<div class="feature-card" markdown>

### Multi-Cursor Editing

Ctrl+Shift+Up/Down to add cursors. Typing, deletion, and movement apply to all cursors simultaneously.

</div>

<div class="feature-card" markdown>

### Code Folding & Minimap

Fold code blocks with Ctrl+Shift+[. Syntax-colored minimap with click-to-scroll.

</div>

<div class="feature-card" markdown>

### Session Restore

Open files, active tab, font scale, and project root persist across restarts. Recent projects list for quick switching.

</div>

<div class="feature-card" markdown>

### Bookmarks & Find-in-Selection

Ctrl+F4 to bookmark lines, F4/Shift+F4 to navigate. Find scopes to the selection when a multi-line region is active.

</div>

<div class="feature-card" markdown>

### Bracket Pair Colorization

Matching brackets colored by nesting depth (gold, pink, blue) like VS Code.

</div>

<div class="feature-card" markdown>

### Find & Replace in Files

Find and Replace across files in a project folder.

</div>

<div class="feature-card" markdown>

### Nano Anvil

A minimal single-file editor with software rendering. No GPU driver overhead -- ~28MB RAM on Linux. No sidebar, terminal, LSP, git, or tabs.

</div>

## Overview

Lite Anvil is a fork of [Lite XL](https://github.com/lite-xl/lite-xl), rewritten from the ground up in Rust.

Lite Anvil also ships Nano Anvil, a minimal single-file editor that uses software rendering (no GPU drivers). See the [Installation](installation.md) page for details on both binaries.

| | |
|---|---|
| **Languages** | 50+ syntax grammars, built-in LSP configurations |
| **Platform** | Linux, macOS, Windows |
| **License** | MIT |
| **Rust version** | 1.85+ |
