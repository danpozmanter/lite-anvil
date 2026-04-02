# Change Log

## [1.5.1] - 2026-04-02 — Session fix.

* Fix closed project reopening on restart: session restore now respects an explicitly closed project instead of falling back to the most recent one.

## [1.5.0] - 2026-04-02 — Bookmarks, indent guides, line sorting, sidebar improvements, and 15 new language servers.

* Add bookmarks plugin: toggle (Ctrl+F2), next (F2), previous (Shift+F2), clear. Accent marker in gutter.
* Add indent guide plugin: vertical lines at each indentation level (off by default). Toggle via `indent-guide:toggle`.
* Add line sorting commands: `lines:sort`, `lines:sort-reverse`, `lines:reverse`, `lines:unique`, `lines:sort-case-insensitive`.
* Add goto-line support in file picker and open file dialog: type `file.rs:42` to open at a specific line.
* Add sidebar context menu: Open, Copy Path, Copy Relative Path, Refresh, Rename, Delete, New File, New Folder.
* Add `treeview:refresh` command for manual sidebar rescan.
* Fix `doc:save-as` defaulting to `/` for unsaved files: now defaults to project root.
* Fix unsaved changes dialog.
* Move unsaved changes confirmation from command view (bottom bar) to NagView (top modal dialog) with Save/Close/Cancel buttons for consistency.
* Sidebar refreshes instantly after save-as (direct `sync_model` call instead of waiting for dirwatch).
* Deleting a file via sidebar now flags the open doc as dirty/unsaved.
* Added builtin LSP specs for Elixir, Erlang, OCaml, Gleam, C/C++, Haskell, Zig, Dart, Scala, Swift, Ruby, Julia, Clojure, Crystal, Lua, and Bash.

## [1.4.1] - 2026-04-02 — Stability fixes and dead code cleanup.

* Fix F# (fsautocomplete) root patterns to include `.fsproj` for standalone F# projects without solution files.
* Fix workspace data loss: storage is now deleted only after restore succeeds, not before.
* Fix `test_class_name` returning `Option<String>` when it can never be `None`.
* Remove unused `clear_failed` LSP API function and dead variables from log removal.

## [1.4.0] - 2026-04-01 — Per-project workspace memory, builtin LSP for 10 languages, log cleanup.

* Enable workspace plugin: per-project open file/tab memory now activates (was registered but never loaded).
* Fix workspace not restoring on project switch: `load_workspace` was only called at startup, not after `set_project`.
* Fix workspace restore opening phantom blank docs for files that no longer exist or have relative paths: resolve filenames against the saved project root, and skip files that are missing from disk.
* Add `workspace:clear-project-memory` palette command to clear all saved workspace state.
* Add `workspace:clear-recents` palette command to clear recent projects and recent files lists.
* Add builtin LSP specs for C# (OmniSharp), F# (fsautocomplete), Java (jdtls), Kotlin (kotlin-language-server), Python (pyright), Go (gopls), JavaScript/TypeScript/TSX (typescript-language-server), and PHP (intelephense).
* Fix LSP client not declaring support for references, type definition, implementation, document symbols, code actions, call/type hierarchy, and signature help.
* Fix fsautocomplete not loading F# projects: add `AutomaticWorkspaceInit: true` to initialization options.
* Add LSP_SUPPORT.md documenting all builtin language servers and custom configuration.
* Fix LSP server spawn spamming: failed servers are remembered and not retried until config reload.
* Suppress LSP semantic token errors during server startup (retried automatically on next tick).
* Suppress raw server error dump on go-to-definition failure; show user-friendly message instead.
* Warn the user when an LSP server exits before initialization completes or crashes unexpectedly.
* Log LSP server lifecycle: "LSP starting X" and "LSP X initialized" entries in the log.
* Log LSP server progress/workspace notifications (e.g. fsautocomplete project loading status).
* Remove noisy per-plugin "Loaded plugin", "Registered lazy", "Replacing existing command", and "Opened doc" log messages; keep only the summary line.
* Remove `at [C]:0` from log entries originating in Rust; Lua-sourced entries still show source location.
* Filter LSP stderr `WARN notify error:` messages (file watcher noise like "Too many open files" from Rust Analyzer).

## [1.3.6] - 2026-04-01 — Test runner fixes, language support, and new syntax highlighting.

* Fix F#/C# projects with a `tests/` directory being misdetected as Python unittest.
* Fix `has_extension` crashing at runtime (`list_dir` returns a flat string table, not a table of tables).
* Fix node file-scoped test always using vitest regardless of detected runner (jest, npm test).
* Add Scala (sbt) test runner support.
* Add PHP (phpunit) test runner support.
* Add `.scala` extension handling in Gradle and Maven file-scoped test commands.
* Remove unused test framework detection code (dotnet, Gradle, Maven framework sniffing).
* Add XML syntax highlighting for .NET project files (`.csproj`, `.fsproj`, `.vbproj`, `.vcxproj`, `.sln`, `.props`, `.targets`, `.nuspec`), `.pom`, `.svg`, `.plist`, and `.xaml`.
* Add Groovy syntax highlighting (`.groovy`, `.gradle`).
* Add Dockerfile syntax highlighting.
* Add builtin LSP specs for C# (OmniSharp), F# (fsautocomplete), Java (jdtls), and Kotlin (kotlin-language-server).

## [1.3.5] - 2026-04-01 — Removing legacy files.

* Removed `scripts/fontello-config.json` - no longer needed (project uses PNG icons instead of font icons).
* Removed `scripts/keymap-generator/` - obsolete SDL-based keymap generator incompatible with winit input system.

## [1.3.4] - 2026-04-01 — Syntax highlighting fix for multi-byte characters, code quality, stability, and style fixes.

* Fix syntax highlighting breaking after multi-byte UTF-8 characters (e.g. arrows, emoji).
* Fix `assert!(x == false)` anti-pattern → `assert!(!x)` in project_fs tests.
* Remove unnecessary `mut` declarations on non-reassigned variables.
* Simplify redundant boolean comparison patterns throughout codebase.
* Optimize string formatting in runtime Lua path setup.
* Remove redundant clone operations in editor command handlers.
* Fix redundant iterator `.cloned()` calls on already-owned data.
* Remove spurious `#[allow(dead_code)]` annotations from used functions.
* Standardize error mapping to use `.map_err()` consistently.
* Clean up unused imports in core editor and LSP modules.
* Consolidate redundant `PathBuf` to `String` conversions.

## [1.3.3] - 2026-04-01 — F# syntax fix, CLI file/folder open fix, active file fixes, and BOM support.

* Fix wrong active file after restart: suppress active_file disk writes during session restore and exit so only user tab switches persist the value.
* Remove duplicate active_file disk write that was in `core.set_active_view` (autoreload's patch is the single writer now).
* Fix F# syntax highlighting for type parameters like `'Type` - no longer misinterpreted as character literals.
* When opening lite-anvil with a file/directory from CLI, previously open files are now closed first.
* Add full BOM (Byte Order Mark) support: UTF-8, UTF-16 BE/LE, and UTF-32 BE/LE BOMs are detected on load and preserved on save.

## [1.3.2] - 2026-03-31 — Dead code removal, float comparison fix, and error handling improvements.

* Remove `#[allow(dead_code)]` on tree model watcher (legitimate RAII pattern, explicitly marked).
* Fix float comparison with epsilon in session.lua_to_color.
* Fix potential double-panic in ProcessHandle Drop implementation.
* Replace `unreachable!()` with proper error handling in command core.
* Clean up enumerate pattern in runtime args processing.

## [1.3.1] - 2026-03-31 — Bug fixes and dead code cleanup.

* Fix panic on truncated multi-byte UTF-8 sequences in the renderer text cache.
* Fix byte-level string truncation when copying whole lines without a trailing newline.
* Remove no-op `width -= 0.0` in tab width calculation.
* Remove erroneous `#[allow(dead_code)]` on `LuaEventVal::Bool` (variant is used).
* Clean up fragile unwrap patterns in linewrapping and git command checking.

## [1.3.0] - 2026-03-31 — Multi-tab terminal, breadcrumbs, LSP hierarchy, test runner, and file watching.

* Multi-tab terminal: navigate between terminal tabs with Ctrl+Alt+Left/Right, jump by number with Ctrl+Alt+1-9, and list all terminals with Ctrl+Alt+T.
* Breadcrumbs/scope bar: displays file path segments and current code scope (from LSP document symbols) between the tab bar and editor content.
* LSP call hierarchy
* LSP type hierarchy
* Some LSP navigation items added to the right-click context menu.
* Native file watcher auto-refresh for immediate external-change detection.
* Integrated test runner: auto-detects cargo, pytest/unittest, go, gradle/mvn, dotnet, npm/vitest/jest, and make.
* Dividers for the context menu.
* Fix cursor navigation bug.

## [1.2.1] - 2026-03-30 — Diagnostics improvements.

* Diagnostic tooltip width increased for readability.
* Tooltip text wraps at word boundaries instead of splitting mid-word.
* Single-character diagnostic underlines expand to cover the full word.
* Fix diagnostics on load.

## [1.2.0] - 2026-03-30 — LSP snippets, diagnostics hover-only, dirty state fix.

* LSP snippet support -- completions with placeholders and tabstops now expand correctly.
* Diagnostic text only shown on mouse hover; underlines and gutter markers always visible.
* Fix dirty indicator not showing after restoring a file with unsaved undo history.
* Fix quit dialog appearing multiple times on repeated close attempts.
* Fix `extract_subsyntaxes` crash when toggling block/line comments.
* Dialog bar color changed from red to neutral gray across all themes.

## [1.1.2] - 2026-03-29 — Fix quit confirmation, Windows CI reliability.

* Fix quit confirmation dialog not waiting for user input when there are unsaved changes.
* Add retry logic and binary caching for vcpkg installs to handle transient download failures.

## [1.1.1] - 2026-03-29 — Terminal opens in active file's directory.

* New terminals open in the directory of the currently active tab. Falls back to project root or home for unsaved/untitled files.

## [1.1.0] - 2026-03-29 — LSP inlay hints.

* LSP inlay hints (type annotations, parameter names) rendered inline with text shifting.

## [1.0.4] - 2026-03-27 — Fix command view typeahead completing on multiple suggestions.

* Fix typeahead completing full word on single character when multiple suggestions exist. Typeahead now only fires when exactly one suggestion matches.

## [1.0.3] - 2026-03-27 — Fix persistent undo for saved files, fix active tab persistence.

* Fix persistent undo not working for saved (clean) files. Undo history was only persisted for unsaved documents, clean files lost their undo stacks on restart.
* Fix active tab being lost across sessions. Active file is now saved in session.json during session save.

## [1.0.2] - 2026-03-27 — Persistent undo with 5MB cap.

* Persistent undo history — undo/redo stacks are saved alongside backup files and restored when reopening unsaved/dirty documents.
* 5MB cap per file on persistent undo storage to prevent excessive disk usage.

## [1.0.1] - 2026-03-26 — Cursor fix, codebase reorganization.

* Fix cursor up/down always jumping to line start.
* Codebase reorganization.

## [1.0.0] - 2026-03-26 1.0.0 Release - stability and performance fixes + minimap, find in selection, unsaved file persistence.

* Find in selection — when a multi-line selection is active and Find is opened, search is limited to the selected region. Toggle with Alt+S. Status bar shows [S] when active.
* Tab reordering — drag tabs within the same pane to reorder them (previously only cross-pane moves worked).
* Recent files in Open File — when the Open File command palette is empty, shows recently opened files instead of directory listing.
* Incremental highlighter — undo/redo no longer calls soft_reset() (which wiped ALL cached tokens). Now uses targeted insert_notify/remove_notify to invalidate only affected lines. Typing and undo on large files is significantly faster.
* Event batching — after a redraw, immediately checks for pending events before sleeping. Key repeat & rapid undo no longer wait for the frame timer between events.
* Add PLUGINS_GUIDE.md with API reference, config reference (79+ options), 10 recipes, lifecycle docs, and pitfall warnings.
* 74 tests (up from 69). New tests for undo merge edge cases, atomic save, content signature, selection iterator truthiness.
* SAFETY comments on all 40+ unsafe blocks. All are FFI (SDL3, FreeType, libc).
* `let _ =` silent error swallows upgraded to `log::warn`.
* Optional minimap.
* Unsaved file persistence (Sublime-style) — dirty/unsaved buffers are backed up to `USERDIR/backups/` on session save and restored on next launch. Files that no longer exist on disk remain open instead of being closed.
* LSP typing debounce — rapid keystrokes are coalesced with a 150ms debounce before sending `textDocument/didChange` to the language server, preventing server flooding.
* LSP robustness — malformed JSON from language servers is now logged via `log::warn` instead of silently dropped or panicking. LSP panics are caught gracefully.
* SIGINT/SIGTERM signal handler — graceful shutdown on Unix signals. Session is saved (including unsaved file backups) before exiting.
* Linter cleanup across 75 files: replaced `unwrap_or_else` error swallows with proper propagation, tightened borrow patterns, removed unused variables.

## [0.20.0] - 2026-03-25 — Stability hardening for 1.0.
* Atomic file writes for doc save and session save (write to .tmp, fsync, rename). Prevents data corruption on power loss or crash mid-write.
* Guard undo merge stack pop against empty stack.
* Safe defaults for terminal view property access (nil → 0.0 instead of crash).
* Safe hex color parsing in terminal (malformed escape sequences → 0 instead of panic).
* Bounds-safe line content extraction in doc commands.
* Fix Lua truthiness check in selection sort (`doc_module.rs:665`) — same class of bug as the v0.19.5 `get_selections` fix.
* Log backup file write failures in project replace instead of silently ignoring.

## [0.19.6] - 2026-03-25 — Undo grouping and input latency improvements.
* Consecutive single-character inserts (typing/key repeat) merge into a single undo entry. A new undo group starts on: pause >1s, newline, delete, cursor movement, or batch edit. Holding a key then pressing Ctrl+Z undoes the entire run at once.
* Reduced per-frame overhead: cached `core.try`'s error handler and `xpcall` (previously recreated every event), cached `poll_event`/`renderer` via named registry slots, replaced Lua `math.min/max/ceil` with native Rust in the run loop, cached `fps`/`blink_period`/`wait_event`/`has_focus` outside the loop.
* Fixed 75% rendering regression from integer-to-float window size conversion.

## [0.19.5] - 2026-03-24 — Key repeat, typeahead, and wrap rendering fixes.
* Improved key repeat stutter.
* Fix ghost character at end of wrapped lines.
* Fix aggressive command palette typeahead after typing a path separator (/ or \).
* Undo now handles edits cohesively.

## [0.19.4] - 2026-03-24 — macOS "Open With" fix.
* Fix macOS "Open With" not opening the file. macOS sends files via Apple Events (converted to `SDL_EVENT_DROP_FILE`), not command-line args. The `on_file_dropped` handler required x/y coordinates that the drop event doesn't provide, crashing on nil-to-f64 conversion. Made coordinates optional.

## [0.19.3] - 2026-03-24 — Cross-platform "Open With" file associations.
* Register Lite-Anvil for "Open With" on all platforms for 100+ file extensions matching supported syntax types plus .txt, .log, .conf, .env, .diff, .patch, Dockerfile, and other common text files.
* Linux: updated .desktop file with full MimeType list; included in .tar.gz and .deb archives.
* macOS: new Info.plist with CFBundleDocumentTypes covering all extensions and UTI content types; replaces inline plist in CI.
* Windows: PowerShell scripts (install/uninstall-file-associations.ps1) register per-user OpenWithProgids for all extensions; included in .zip archive.

## [0.19.2] - 2026-03-24 — macOS Intel link fix.
* Fix macOS x86_64 link failure: undefined `HVF_*` symbols from FreeType HEAD (VER-2-14-3, released 2026-03-22). Pin CI FreeType build to VER-2-14-2. Provide C stub fallbacks via `cc` build dep for local builds against newer FreeType.

## [0.19.1] - 2026-03-23 — Fix Mac OS Intel build regression.

## [0.19.0] - 2026-03-23 — All embedded Lua eliminated from Rust source; all plugins native, fixes++.
* Converted all remaining embedded Lua to pure Rust mlua closures. Modules converted: doc, syntax, highlighter, statusview, node, rootview, git_view, treeview, toolbarview, terminal_view, tokenizer_shim.
* Converted all 13 bundled plugin .lua files (3,550 lines) to pure Rust: projectsearch, projectreplace, markdown_preview, remotessh, scale, trimwhitespace, theme_toggle, macro, reflow, tabularize, language_md. Delete data/plugins/ .lua files.
* Converted all 6 color theme .lua files (277 lines) to pure load from json.
* Fixed scale plugin missing storage persistence and session hooks (text size now remembered across restarts).
* Fixed `core` not registered as a strict-mode global, causing "cannot get undefined variable: core" on quit.
* Fixed Lua truthiness bug in `get_selections` iterator — numeric idx_reverse values were treated as falsy, breaking backspace, delete, and left/right arrow keys
* Removed LSP warning at startup.
* Last active file remembered.
* Color themes now loaded from JSON files (`data/assets/themes/*.json`).

## [0.18.2] - 2026-03-23 — Lua iterator and UTF-8 navigation fixes.

* Fix freeze when arrow-key navigating through multi-byte UTF-8 text near document boundaries. `previous_char` and `next_char` in `doc_translate` looped on continuation bytes without checking whether `position_offset` actually advanced; at the start/end of a document the position stays unchanged, producing an infinite loop. Added boundary guards that break when the position stops moving.
* Fix "error converting Lua string to String (incomplete utf-8 byte sequence)" crash when navigating through non-ASCII text. `Doc:get_char` returns a single byte via Lua `string.sub`, which for multi-byte UTF-8 characters yields an incomplete byte sequence that cannot be converted to a Rust `String`. Changed all `get_char` consumers in `doc_translate`, `docview`, `autocomplete`, and `lsp_plugin_preloads` to use `LuaString` (raw bytes) instead of `String`, and updated `is_non_word` to operate on byte slices.
* Fix command palette crash from incorrectly driven Lua iterator protocol. `commands_findreplace` captured only the iterator function from `Doc:get_selections()`, discarding the invariant table and start index required by Lua's generic-for protocol. The iterator was then called with no arguments, making `invariant` nil inside `selection_iterator`. Added `collect_selections` helper that properly unpacks all three return values and drives the iterator correctly.

## [0.18.1] - 2026-03-23 — Dirty-state tracking behavior fix.
* Move content signature (FNV-1a hash) to native Rust in `BufferState`.
* Fix stale signature cache after `load_file_into_state`.
* Fix `Doc:new` not calling `clean()` after non-lazy `load()`, leaving `clean_signature` stuck at the empty-buffer hash.

## [0.18.0] - 2026-03-23 — Core runtime fully native Rust.
* Converted all 38 `data/core/*.lua` files and the `core` orchestrator to pure Rust via mlua.
* Some key fixes including around Lua/C/Rust boundaries, yielding, and closures.

## [0.17.3] - 2026-03-22 — Command palette, open, and script fixes.
* Fix command palette input showing only the very last character of a path (e.g. "e" from "forge-core/"). `docview_get_line_screen_position` used `docview_get_gutter_width` (Rust, line-number-based, ~28 px) to position text on screen, while `scroll_to_make_visible` and the clip rect both used `gutter_width_from_method` (virtual Lua dispatch, returns CommandView's label width, ~98 px). The 70 px discrepancy caused `scroll_to_make_visible` to over-scroll by exactly that amount, leaving only the last character in view. Changed `docview_get_line_screen_position` to use `gutter_width_from_method` so all three subsystems agree on where text starts — for regular DocView the result is identical (both paths call the same Rust function), and for CommandView the label width is used consistently.
* Fix path truncation and invisible-backspace in the Open File (and all other) command palette inputs. `CommandView:scroll_to_make_visible` was a no-op and `get_h_scrollable_size` returned 0, so the view never scrolled horizontally and `View:clamp_scroll_position` immediately zeroed any scroll.x that was set. Now `scroll_to_make_visible` delegates to DocView for horizontal tracking (resetting y=0 to stay single-line), `clamp_scroll_position` is overridden to preserve x while locking y=0, and the `get_h_scrollable_size` override is removed so the inherited `math.huge` allows the scroll position to be maintained.
* Fix `attempt to index a nil value (local 'path_stat')` crash when submitting a filename in the Open File command palette. `system.get_file_info` was returning a single `nil` on error, but the Lua validate callback expects the canonical two-return `nil, error_string` form. Changed the Rust implementation to return `nil, error_message` on failure.
* Fix `bad argument #4: error converting Lua number to i64 (out of range)` crash when opening a file through the command palette. The autocomplete `Doc:remove` wrapper used `i64` for coordinates, which cannot represent `math.huge` (Lua infinity) passed by `commandview.set_text` to clear the input doc. Changed coordinate types to `f64` so infinity passes through to the original sanitizing function.

## [0.17.2] - 2026-03-22 — CI cleanup + release script.

* Fix macOS Intel CI build - attempt 2.
* Add `release.sh` helper script.

## [0.17.1] - 2026-03-22 — macOS Intel build fix.

* Fix macOS Intel CI build: SDL3 cmake issue.

## [0.17.0] - 2026-03-22 — Some plugins into Rust + fixes.

* Translate all embedded Lua plugin bootstraps to pure Rust via mlua APIs. Autorestart, quote, terminal, findfile, lineguide, autoreload, folding, drawwhitespace, toolbarview, git commands and UI, autocomplete, and all three LSP modules (`plugins.lsp`, `plugins.lsp.server-manager`, `plugins.lsp.client`) are now registered as Rust closures; no Lua string is interpreted at runtime for any bundled plugin.
* Fix LSP inline diagnostics (squiggly underlines and end-of-line ghost text) broken by an incorrect `core.add_thread` call pattern introduced during the Lua-to-Rust migration.
* Fix all fully-Rust bundled plugins not loading on startup: populate `package.native_plugins` from Rust, teach `core.load_plugins()` to consume it, and guard `runtime_setup.lua` with `or {}` so it does not overwrite the list Rust built before Lua initialised.
* Fix LSP diagnostic tooltip crash on mouse move: `mgr_wrap_tooltip_lines` called `font:get_width` without passing `font` as `self`, causing "bad argument #1: error converting Lua string to table" in every `on_mouse_moved` event. Also replace twelve other `table.get::<LuaFunction>("method")?.call((table, args))` OOP dispatch anti-patterns with `table.call_method("method", args)` throughout the LSP patches.

## [0.16.0] - 2026-03-20 — More progress on core modules to Rust and moving some plugins into core.

* Move all `core.*` modules (config, style, syntax, tokenizer, highlighter, command, keymap, process, view, scrollbar, contextmenu, nagview, logview, commandview, all commands submodules, doc.search, doc.translate, common, object, strict, regex, storage, utf8string, gitignore, dirwatch, ime, project, plugin_api, modkeys, emptyview, titleview, and more) to Rust-owned `package.preload` entries. Every `require "core.*"` call is now intercepted before any disk lookup.
* Move all bundled plugins (`plugins.autocomplete`, `plugins.autoreload`, `plugins.autorestart`, `plugins.bracketmatch`, `plugins.detectindent`, `plugins.drawwhitespace`, `plugins.findfile`, `plugins.folding`, `plugins.git` and sub-modules, `plugins.language_md`, `plugins.lineguide`, `plugins.linewrapping`, `plugins.macro`, `plugins.markdown_preview` and sub-modules, `plugins.projectreplace`, `plugins.projectsearch`, `plugins.quote`, `plugins.reflow`, `plugins.remotessh`, `plugins.scale`, `plugins.tabularize`, `plugins.theme_toggle`, `plugins.toolbarview`, `plugins.terminal` and sub-modules, `plugins.trimwhitespace`) to Rust-owned preloads. Plugins are discovered from disk metadata but loaded from the binary.
* Embed all six bundled color themes (`colors.default`, `colors.dark_default`, `colors.light_default`, `colors.fall`, `colors.summer`, `colors.textadept`) as Rust-owned preloads.
* Delete orphaned `data/core/start.lua` (superseded by `runtime.rs` startup logic).
* JSON syntax assets are parsed by Rust via `native_tokenizer.load_assets()`; `plugins.lsp.json` dependency removed from syntax initialization.
* Tech debt: Move all Lua embedded in Rust source strings to external `.lua` files loaded via `include_str!`; no functional change.
* Move `plugins.bracketmatch` and `plugins.detectindent` fully to Rust: bracket-pair highlighting computed in `affordance_model`, indent detection in `doc_native`, all commands in `detectindent.rs`; delete `data/plugins/bracketmatch.lua` and `data/plugins/detectindent.lua`.
* Move `plugins.linewrapping` fully to Rust: line-break computation, wrap-state management, and all coordinate/rendering overrides implemented in `linewrap.rs` and `docview.rs`; commands and translate-function patches registered from `linewrapping.rs`; delete `data/plugins/linewrapping.lua`. (57.7% Rust)
* Move `plugins.git` async state management to Rust: background-threaded `git status` refresh with change-detection cache, per-file-entry state map, and async command dispatch via handle polling, all in `git.rs`; `git_plugin.rs` embeds the three Lua modules as inline const strings; delete `data/plugins/git/{status,init,ui}.lua`. (59.4% Rust)
* Move `plugins.lsp.json` and `plugins.lsp.protocol` to native Rust modules (direct delegation to `lsp_protocol`); move `plugins.lsp.client` to an inline Rust const string; delete all three Lua source files. (59.8% Rust)
* Move `plugins.lsp.server-manager` (1,892 lines) to an inline Rust const string in `lsp_plugin_preloads.rs`; delete `forge-core/src/api/lua/plugins_lsp_server_manager.lua`. (63.5% Rust)
* Move `plugins.lsp` init module (507 lines) to an inline Rust const string in `lsp_plugin_preloads.rs`; delete `forge-core/src/api/lua/plugins_lsp_init.lua`. All LSP Lua sources are now embedded in the binary. (64.5% Rust)

## [0.15.1] - 2026-03-19 — New window command and clippy cleanup.

* Add `core:new-window` command (`Ctrl+Shift+N` / `Cmd+Shift+N`) that opens a new editor instance by spawning the current executable.
* Make `system.exec` cross-platform: uses `sh -c` on Unix and `cmd /C` on Windows.
* Fix 23 clippy warnings across `affordance_model`, `commands_doc_native`, `docview_native`, `node_model`, `project_fs`, `project_model`, `terminal_buffer`, `tree_model`, and `workspace_native`.

## [0.15.0] - 2026-03-19 — Rust-owned editor runtime + fixes.

* Move all core module bodies (`core`, `core.statusview`, `core.rootview`, `core.node`, `core.doc`, `core.docview`, `core.commands.doc`, `plugins.treeview`, `plugins.lsp.server-manager`, `plugins.terminal.view`, `plugins.workspace`) to Rust-owned package preloads. Lua is now the extension and customization layer; the runtime is Rust.
* Add native Rust backends for startup globals and path resolution, tab and pane layout math, persistent storage I/O, document load/save/edit/undo/redo, session persistence, project file search and replace, bracket matching, fold calculations, indent detection, trim-whitespace decisions, and autorestart path checks.
* Move status-bar panel layout, drag handling, and hit-testing into Rust. Move treeview init, model sync, selection, hover, and scale-metrics into Rust.
* Add declarative JSON-backed theme and syntax assets. Migrate bundled default themes and Rust, Bash, TOML, env, and ini syntax definitions onto that path. Remove per-language Lua wrapper files for all JSON-covered languages.
* Add `core.plugin_api` as a stable Lua interface for bundled plugins, replacing direct access to internal views, prompts, status items, session hooks, and thread spawning.
* Fix startup and directory-change crash in the bundled Git plugin. Git commands remain available; the branch indicator is no longer shown in the status bar.
* Fix empty treeview on startup when the session has no active project.
* Fix Open File crash when submitting an empty string.
* Fix Esc incorrectly triggering focus-mode exit when focus mode was not active.

## [0.14.7] - 2026-03-18 — RAM reductions and cache bounds.

* Reduce terminal scrollback memory and allocator churn.
* Terminal now swaps ownership instead of cloning.
* Share the native project file list between `project_model` and `project_manifest` where possible.
* Optmize tree path lookup.
* Explicit symbol index cache cleanup.
* Git status cache cap.
* Shrink undo/redo history after resets/loads/clears.
* Caps to command pallete suggestions, gitignore rule caching, and font glypgh caching.
* Reduce treeview memory per node by removing the redundant `project_root` string from every `TreeNode`;
* Replace path-string keys in the treeview `visible_index` with node-id keys, avoiding a second string allocation per visible node.
* Add size-based eviction to the treeview label and text-width caches so very large project trees do not accumulate unbounded UI entries.
* Fix Windows treeview project lookup: `sync_model` now stores the forward-slash form of each project path.
* Close all open files belonging to a project when that project directory is removed, prompting to save any unsaved changes.
* Remove the `alt+1` keybinding (`root:switch-to-tab-1`).
* Fix empty treeview on startup when the session has `active_project=false`.
* Fix crash in Open File validate when submitting an empty string (`bad argument #1: error converting Lua nil to String`).
* Fix Esc not exiting commands like Open File: `root:exit-focus-mode` now only fires when focus mode is actually active.

## [0.14.6] - 2026-03-17 — macOS release fix and lower-memory restore.

* Fix macOS release builds by replacing the non-portable BSD-`sed` version lookup in the release workflow with a portable parser.
* Persist the “no active project open” state so closing a project folder stays closed across restart.
* Release native project file-list and manifest caches immediately when a project closes instead of retaining them until restart.
* Restore session and workspace documents lazily so inactive tabs do not eagerly load every file into memory on startup.

## [0.14.5] - 2026-03-17 — Versioning and packaging consistency.

* Centralize the app version in the workspace Cargo manifest so Cargo, `about:version`, installers, and release packaging all report the same version.
* Include the app version in generated release archive names such as `lite-anvil-0.14.5-macos-aarch64.zip`.

## [0.14.4] - 2026-03-17 — Find/replace and command palette polish.

* Fix a command palette suggestion-index crash that could trigger while opening search prompts.
* Make in-file find/replace easier to reach with `Ctrl+F` / `Ctrl+H`.
* Point the toolbar search button at in-file find and label search commands in the palette as `Find`, `Replace`, or `Swap`.

## [0.14.3] - 2026-03-17 — Inline diagnostics.

* Render LSP diagnostics inline at the error line in addition to the existing hover popup.

## [0.14.2] - 2026-03-17 — About:version and highlight-open fix.

* Preload the matching language plugin before opening a file so syntax-highlighted files render once, immediately, without a plain-text flash.
* Stop requesting LSP semantic token overlays on initial open, avoiding a second recolor pass right after the file appears.
* Make app and installer build-version metadata derive from the package version instead of duplicated literals.
* Add an `about:version` command that shows the current Lite-Anvil version inside the app.

## [0.14.1] - 2026-03-17 — macOS bundle/signing fixes.

* Fix macOS app metadata to use the current release version and a valid bundle identifier.
* Improve macOS dependency bundling so `@rpath` libraries and framework-style dependencies are copied into the app bundle correctly.
* Sign and verify the assembled `.app` bundle during install to catch broken local builds before launch.

## [0.14.0] - 2026-03-16 — Focus mode, LSP navigation, unsaved files ergonomics, terminal improvements.

* Add closing the current project folder so Lite-Anvil can stay open with no folder attached and just unsaved files, plus a reversible focus mode for the active file.
* Harden LSP-driven navigation and diagnostics with jump-back, inline error surfacing, hover popups for diagnostic messages, and LSP quick fixes.
* Improve terminal/TUI support by handling alternate-screen, charset, cursor, and scroll-region escape sequences natively, draining PTY output more aggressively, and adding first-letter nag dialog shortcuts like Terminate/Cancel.
* Terminal UI bugfixes.

## [0.13.9] - 2026-03-16 — UI polish.

* Fix cross-platform tree spacing and truncation so icons, chevrons, labels, tooltips, and resize behavior stay clean on macOS and Linux.
* Polish tabs, titlebar, command palette, Git status, and panel separators while keeping the recent large-workspace Git/search/replace work off the main thread.

## [0.13.8] - 2026-03-16 — UI optimizations.

* Move sidebar/tree hot paths fully native, fetch only visible row windows, and cut repeated tab, toolbar, titlebar, tooltip, statusbar, and context-menu layout work on the UI thread.
* Remove remaining main-thread blockers from workspace-scale features by dropping synchronous native Git refreshes from the refresh loop, moving native replace work off-thread, and preferring the async project file cache for search/replace file collection.

## [0.13.7] - 2026-03-16 — Treeview lazy loading.

* Make the native tree model lazy so treeview no longer walks whole projects before showing results.
* Fix blank or slow treeview loads on very large projects, including macOS/APFS.

## [0.13.6] - 2026-03-16 — Native treeview hot-path port.

* Move treeview traversal, flattening, expand state, ignore filtering, and filesystem watching into a native async Rust tree model.
* Remove overlapping recursive treeview watchers that could stall even small projects when expanding folders.

## [0.13.5] - 2026-03-16 — Tree highlight stability and tests.

* Sidebar tree's blue focused-row highlight flicker fix.
* Fix Git branch parsing for statuses that report both ahead and behind counts.
* Improve TreeView folder-creation error reporting and add regression tests for Git status parsing and native project file walking.

## [0.13.4] - 2026-03-16 — Large-project responsiveness: deeper dive.

* Move project file-tree walks and filesystem-watcher setup entirely off the Lua main thread into background threads; callers always get the current (possibly stale) list immediately and the UI wakes when fresh results arrive.
* Defer inotify/FSEvents recursive watcher registration until after the file walk completes, eliminating the multi-second main-thread stall on trees with tens of thousands of directories.
* Apply a 500 ms debounce to dirty-flag rebuilds so that build-system churn (thousands of rapid file events) triggers at most one rebuild per burst.
* Eliminate one `stat(2)` syscall per file in `project:files()` and `core:find-file` by substituting a synthetic `{type="file", size=0}` info table; the native model already enforced the size cap, so no filtering is lost.

## [0.13.3] - 2026-03-16 — Large-project responsiveness.

* Stop recursive native directory watches from expanding across entire subtrees.
* Cap native project file collection and tree directory listing to keep huge workspaces responsive.
* Avoid full-project native scans when project search or replace targets a single file.
* Coalesce filesystem change bursts so large directory churn does not flood rebuild work.

## [0.13.2] - 2026-03-16 — Additional syntax highlighting.

* Add syntax highlighting for env, ini, and zsh files.

## [0.13.1] - 2026-03-15 — Autocomplete mode cleanup and LSP-first completions.

* Replace autocomplete source toggles with explicit modes: off, in-document, via LSP, and totally on.
* Default autocomplete to via-LSP when the LSP provider is available; otherwise default it to off.
* Wire LSP completions into the autocomplete popup so typing can use server results instead of only manual `ctrl+space`.

## [0.13.0] - 2026-03-15 — Stability fixes, tab menu, recent items, folding, editor polish.

* Stability fixes - segfault fixes.
* Add a tab right-click menu with Close, Close Right, Close Others, Close Saved, and Close All.
* Add recent file and recent folder pickers.
* Add visible sticky find toggles for case, regex, and whole-word search.
* Add selection match highlighting and dirty-tab markers.
* Add LSP format-on-save, enabled by default and configurable.
* Add gutter and status quick-fix surfacing for diagnostics.
* Add indentation-based code folding with gutter UI and persisted fold state.
* Improve save and rename path previews while editing names.

## [0.12.0] - 2026-03-15 — Large-file mode, project cleanup, terminal reuse, more.

* Close all open docs when changing the active project folder.
* Add large-file mode with plain-text, read-only fallback and reduced LSP/autocomplete work.
* Make document open/save I/O failures fail with editor errors instead of hard asserts.
* Expand the default light and dark editor themes with richer token colors.
* Add terminal placement commands and configurable terminal reuse modes.
* Remove deprecated status bar item merging and deprecated command view entry shims.
* Make dirwatch and LSP JSON/config parsing fail more safely.
* Removed unused manifest.json file.

## [0.11.2] - 2026-03-15 — New default themes, light and dark mode toggle.

* Adjusted default dark theme, and added light theme.
* Added a bottom bar toggle for switching light and dark mode.

## [0.11.1] - 2026-03-15 — Windows build fix.

* Fixed Windows MSVC SDL3 linking by wiring vcpkg library discovery into the build.

## [0.11.0] - 2026-03-15 — Terminal placement, native hot-path work.

* New terminals now open in a bottom pane by default, with configurable tab/left/right/top/bottom placement.
* Moved more hot-path layout and cache work into Rust for LSP overlays, monospaced doc hit-testing, status bar fitting, tab metrics, and Git status caching.

## [0.10.3] - 2026-03-15 — Terminal color output fix, removing more unsafe code.

* Fixed terminal color output by ensuring terminal sessions get TERM and COLORTERM defaults when the app is launched without them.
* Stability fix + removing more unsafe code with Lua VM.

## [0.10.2] - 2026-03-15 — Terminal ANSI color fix & further fixes.

* Fixed terminal ANSI color parsing by switching the native terminal parser back to byte-oriented processing.
* Further fixes in terminal.

## [0.10.1] - 2026-03-14 — macOS terminal build fix & unsafe reduction.

* Fixed the native terminal PTY build on macOS.
* Removed unnecessary unsafe Send/Sync impls from native terminal, picker, regex, and process wrappers.

## [0.10.0] - 2026-03-14 — Native LSP, project, command palette picker, terminal emulation.

* Moved LSP config/spec resolution, diagnostics state, and semantic refresh scheduling into Rust.
* Added a native project model for cached project file lists and path normalization helpers.
* Added a native picker backend for command, file, branch, and status item ranking.
* Moved terminal emulation and scrollback buffering into the Rust core.

## [0.9.0] - 2026-03-14 — Native text buffer core.

* Moved the document text buffer core into Rust.
* Moved document load/save, edit apply, and undo/redo into the native buffer path.
* Kept the Lua `Doc` API as a thin wrapper over the native core for compatibility.

## [0.8.0] - 2026-03-14 — Native edit, autocomplete, and watch paths.

* Moved document edits and packed undo records into the Rust core.
* Added a native autocomplete symbol index and project manifest cache.
* Added native Git status/branch plumbing and native LSP transport/framing/JSON.
* Switched dirwatch polling to the native watcher backend.

## [0.7.0] - 2026-03-14 — Native search, tree, and document/buffer functionality.

* Moved tree directory listing into the Rust core.
* Added Rust file & project search, replace, and offset helper.
* Moved some document/buffer functions to Rust.

## [0.6.0] - 2026-03-14 — Native tokenizer, swap.

* Moved tokenization from Lua to Rust.
* Project swap operation with per-side regex and case-sensitivity options, using an isolated placeholder pass.

## [0.5.0] - 2026-03-14 — LSP, terminal, Git, and project workflow upgrades, fixes.

* Added built-in LSP support with startup enabled by default, project lsp.json config, completion, hover, definition/type-definition/implementation, references, rename, symbols, code actions, formatting, signature help, diagnostics, and restart/refresh commands.
* Added semantic token overlays on top of the core tokenizer/highlighter instead of replacing syntax highlighting.
* Switched the default editor theme.
* Improved Rust syntax highlighting so attribute arguments like `#[arg(help = \"...\")]` keep string coloring.
* Added a PTY-backed embedded terminal with shell tabs, ANSI color handling, scrollback, resize support, color schemes, rename support, and terminal open/close/clear actions.
* Added .gitignore awareness for project scanning so file tree, open-file, project search, and project replace respect repository ignore rules, with optional extra ignore patterns in config.
* Improved project search with hierarchical file-grouped results and optional path glob filters.
* Improved project-wide replace with optional path glob filters and configurable .bak backup creation before writes.
* Added Git integration with cached repo status, branch display in the status bar, treeview change highlighting, a Git status panel, diff views, and basic commit/pull/push/checkout/branch/stash/stage/unstage commands.
* Added configuration toggles for LSP startup, semantic highlighting, inline diagnostics, terminal behavior, Git status refresh, tree highlighting, and replace backups.
* Added persistent treeview sidebar width, so manual sidebar resizing is restored on restart.
* Reload file on regaining focus, check if state is dirty.
* Added syntax highlighting for PowerShell, CSV, D, Haskell, Zig, TSX, Vue, Svelte, Julia, Lisp, Makefile, Meson, Crystal, fstab, Gleam, PostgreSQL, and OCaml.
* Fixed segfault when restarting with a terminal open in a split panel: the renderer command cache is now cleared between restarts to release all font references cleanly.
* Fixed terminal nil crash when an ANSI color palette index falls outside the configured scheme range.
* Fixed Git status view hanging indefinitely on EOF.
* Removed the bouncing-icon easter egg from the status bar.
* Terminal tabs now close automatically when the shell exits.
* Added `root:reset-layout` command to collapse all split panels back to a single panel while keeping all open files.
* Open files and terminal windows are now saved and restored across restarts.

## [0.4.0] - 2026-03-12 — Startup loading optimizations, SDL tuning, and fixes.

* Load language_*.lua syntax plugins lazily on first matching file/header instead of at editor startup.
* Lazy-load selected command-driven plugins on first use instead of at startup, including Markdown preview, project search, project replace, and remote SSH.
* Delay loading of large display-only fonts until they are first used by the welcome screen or toolbar.
* Lazy-initialize native regex and markdown modules, and defer plugin metadata regex compilation until plugin scanning actually needs it.
* Reduce the default startup window/backbuffer footprint by using usable display bounds and clamping oversized initial HiDPI backbuffers.
* Remove internal uses of deprecated project-path helper functions to avoid deprecation warnings in normal editor workflows.
* Fix Rust lifetime highlighting so &'static str is no longer tokenized as a quoted string.

## [0.3.1] - 2026-03-11 — Release binary size optimization.

## [0.3.0] - 2026-03-11 — Config + editing upgrades and language support.

* Moved editor fonts, theme colors, syntax colors, and UI style tuning into config.lua
* Added long-line indicator
* Added log font controls
* Multi-selection editing commands, including find-to-multi-cursor selection of all matches at once.
* Remote SSH project mounting via sshfs.
* Added syntax highlighting for F#, SQL, PHP, Assembly, Ruby, Dart, Swift, R, Elixir, Clojure, and Scala.

## [0.2.6] - 2026-03-11 — Markdown + fonts.

* Switching to [Lilex Font](https://github.com/mishamyrt/Lilex)
* Further markdown rendering fix.

## [0.2.5] - 2026-03-10 — Fixes and polish.

* Fixing markdown rendering bugs.
* Adding "show shortcuts" to the load screen prompt.
* Delete key fix.

## [0.2.4] - 2026-03-10 — Mac + minor fixes.

## [0.2.3] - 2026-03-10 — Lua fixes.

## [0.2.2] - 2026-03-10 — Fixes / polishing:

* Ctrl + +/- changes font for sidebar as well as main window.
* Command instead of control on Mac OS
* Ctrl + Shift + ? for Help dialog.
* Window resize on folder open bugfix.
* Mac OS display bug fix.

## [0.2.1] - 2026-03-10 — Fixing Mac OS bundling bug.

## [0.2.0] - 2026-03-10 — Adding markdown preview.

### Added

* Markdown preview pane (`Ctrl+Shift+M` on any `.md` / `.markdown` file) rendered
  natively using pulldown-cmark: headings, paragraphs, bold/italic (colour
  differentiated), inline and fenced code blocks, blockquotes, ordered and
  unordered lists, horizontal rules, and tables with column alignment.
* Clickable links — left-click any link or image reference to open it with
  `xdg-open` (Linux) or `open` (macOS).
* Table rendering with equal-width columns, header highlighting, and border lines
  derived from the active theme.
* Preview stays in sync with the editor in real time; layout reflows when the
  pane is resized.

## [0.1.1] - 2026-03-08 — Clipboard fix, initial render fix, dialog render fix.

## [0.1.0] - 2026-03-06 — Additional languages, bracket matching, replace in project, font size.

### Added

* Syntax highlighting for 9 new languages:
  * **Rust** (`.rs`) — lifetimes, macros (`name!`), attributes (`#[...]`), raw strings
  * **Kotlin** (`.kt`, `.kts`) — annotations, triple-quoted strings, coroutine keywords
  * **Go** (`.go`) — backtick raw strings, built-in functions (`make`, `append`, `len`, …)
  * **Bash** (`.sh`, `.bash`, `.zsh`, `.fish`) — shebang detection, `$VAR`/`${VAR}` variables, heredocs
  * **Java** (`.java`) — annotations, text blocks, modern keywords (`record`, `sealed`, `permits`, `yield`)
  * **C#** (`.cs`) — verbatim strings, attributes, modern keywords (`record`, `init`, `required`, `file`)
  * **TOML** (`.toml`) — `[[array]]`/`[table]` headers, bare key highlighting, ISO 8601 dates
  * **YAML** (`.yaml`, `.yml`) — anchors (`&`), aliases (`*`), tags (`!!str`), key detection
  * **TypeScript** (`.ts`, `.tsx`, `.d.ts`) — decorators, template literals, utility types, TS-specific keywords

* Bracket pair highlighting — when the cursor is adjacent to `(`, `)`, `[`, `]`, `{`, or `}`, both brackets are underlined using the theme accent color. Nesting is tracked correctly across lines.

* Persistent font size — `Ctrl+-` decreases and `Ctrl++`/`Ctrl+=` increases the code font size. The chosen size is saved to disk and restored on next launch. `Ctrl+0` resets to the default.

* **Project-wide replace** (`Ctrl+Shift+H`) — two-step command palette prompt (search term, then replacement). Scans the project and lists all matches identically to project-find. Press `F5` in the results view to apply all replacements atomically (files are written only when the match count is non-zero). A regex variant (`project-search:replace-regex`) is also available from the command palette.

## [0.0.0] - 2026-03-06 — First Rust release

Initial port. Complete replacement of the C backend with Rust. The Lua editor layer is
unchanged; all Lite XL plugins targeting mod-version 4 should (could?) continue to work.

(From version ## [2.1.7] - 2024-12-05)
