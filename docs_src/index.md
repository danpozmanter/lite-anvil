---
title: Lite-Anvil - Lightweight Code Editor Built in Rust
description: Fast, lightweight code editor built in Rust with SDL3. Built-in LSP, embedded terminal, Git integration, and 50+ syntax grammars.
---

<div class="hero" markdown>

# Lite-Anvil

<p class="tagline">A fast and lightweight code editor built in Rust with SDL3.</p>

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

Diagnostics, completion, hover, go-to-definition, references, inlay hints, and more for Rust, Python, TypeScript, Go, C/C++, and other languages.

</div>

<div class="feature-card" markdown>

### Embedded Terminal

Full PTY terminal with ANSI/VT100 colors, scrollback, and multi-terminal support.

</div>

<div class="feature-card" markdown>

### Git Integration

Git status view, gutter markers for changed lines, push, pull, commit, stash from the command palette.

</div>

<div class="feature-card" markdown>

### 50+ Syntax Grammars

Rust, Go, Python, TypeScript, C/C++, Java, Kotlin, Scala, F#, C#, Haskell, Zig, Elixir, Erlang, OCaml, Gleam, and many more.

</div>

<div class="feature-card" markdown>

### Fast & Lightweight

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

### Bracket Pair Colorization

Matching brackets colored by nesting depth (gold, pink, blue) like VS Code.

</div>

</div>

## Overview

Lite-Anvil is a fork of [Lite XL](https://github.com/lite-xl/lite-xl), rewritten from the ground up in Rust.

| | |
|---|---|
| **Languages** | 50+ syntax grammars, built-in LSP configurations |
| **Platform** | Linux, macOS, Windows |
| **License** | MIT |
| **Rust version** | 1.85+ |
