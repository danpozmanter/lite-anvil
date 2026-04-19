# Change Log

## [2.9.9] - 2026-04-19 -- Nano-Anvil Open Recent; command palette "Open File" ranking.

* Nano-Anvil: `core:open-recent` is now available (ctrl+shift+r / command palette), listing only recent **files**. The project-folder list is skipped in single-file mode since Nano-Anvil has no concept of a project folder.
* Command palette: typing "open" now surfaces **Open File** before **Open User Settings**. `fuzzy_match` includes a `-length` term in its score; ranking on the full display string (`"Open File  (ctrl+o)"`, 19 chars) pushed it below the shorter suffix-free `"Open User Settings"` (18 chars). Palette ranking now strips the `  (ctrl+...)` keybinding tail before scoring, so "Open File" wins on the query "open" as users expect.

## [2.9.8] - 2026-04-18 -- Linux: SDL hints now respect env-var overrides (Wayland-only fix).

* Linux: `window::init` defaults `SDL_VIDEO_DRIVER=x11,wayland`, `SDL_FRAMEBUFFER_ACCELERATION=0`, and `SDL_RENDER_DRIVER=software`, but now applies each hint *only* when the matching SDL environment variable is unset. Wayland-only hosts whose SDL3 was built without the X11 backend (previously stuck on `SDL3 init failed: x11 not available`) can override with `SDL_VIDEO_DRIVER=wayland`; users who prefer the OpenGL-backed renderer can set `SDL_FRAMEBUFFER_ACCELERATION=1`. CI-built binaries (SDL3 compiled with both backends) are unchanged. Documented the escape hatches in `BUILDING.md`.

## [2.9.7] - 2026-04-18 -- Nano-Anvil font-loading fix; sidebar rescan on save-as.

* Fixed `FT_New_Face failed (data/fonts/Lilex-Regular.ttf): error 1` on nano-anvil.
  * Added `data-nano/fonts/` default code fornt `Lilex-Medium.ttf`.
  * Fallback path in `main_loop::run` for fonts fix.
* Added `Lilex-Medium.ttf` to `data-nano/fonts/`, and changed `FontsConfig::default()` to leave paths as `None` so the datadir-aware resolver always produces absolute `/usr/share/<app>/data/fonts/...` paths.
* Sidebar: Save As now rescans the project tree after a successful write.

## [2.9.6] - 2026-04-18 -- Windows installer Start Menu fix; single combined Linux .deb / .rpm; Mac reverted to zip + install.sh.

* Windows installer: Start Menu group is now "Lite-Anvil" instead of "Lite-Anvil contributors".
* Linux: release now publishes a single `lite-anvil_*.deb` and single `lite-anvil-*.rpm` containing both editors (binaries + data under `/usr/share/{lite,nano}-anvil/data/` + both `.desktop` entries + icons) instead of two packages per format.
* macOS: reverted the 2.9.4 switch to `.dmg` — macOS Sequoia's Gatekeeper no longer honors the right-click → *Open* bypass for ad-hoc signed apps, so drag-to-Applications hits a hard "malware" block. Release ships `lite-anvil-*-macos-{x86_64,aarch64}.zip` with `install-mac.sh` again: the script invoked via `bash` clears quarantine + codesigns without Gatekeeper interference (quarantine is enforced on app launch, not on interpreted scripts).

## [2.9.5] - 2026-04-17 -- CI fix: install cargo-deb / cargo-generate-rpm from source.

* Release CI: prebuilt `cargo-deb` Switched the install step to `cargo install --locked cargo-deb cargo-generate-rpm` to target the right glibc.

## [2.9.4] - 2026-04-17 -- Packaging overhaul: Add AppImage + .deb + .rpm on Linux, switch to .dmg on macOS and Inno Setup installer on Windows.

* Linux: release now publishes `.AppImage`, `.deb`, and `.rpm` alongside the existing `.tar.gz`. `.deb` / `.rpm` drop into `/usr/bin` + `/usr/share/{lite,nano}-anvil/data/` and register a `.desktop` entry + 256×256 hicolor icon. AppImage is self-contained and runs on any distro (glibc 2.35+). Built via `cargo-deb`, `cargo-generate-rpm`, and `appimagetool` respectively.
* macOS: shipped artifact is now a drag-to-install `.dmg` (LiteAnvil.app + NanoAnvil.app + Applications symlink) instead of a `.zip` + `install-mac.sh`. Drops the need for the install script entirely. `scripts/build-local-mac.sh` produces a `.dmg` too.
* Windows: shipped artifact is now `LiteAnvil-X.Y.Z-x86_64-setup.exe` built with Inno Setup 6 instead of the raw `.zip`. Creates Start Menu shortcuts for both editors, registers the "Open with Lite-Anvil" context menu, and offers optional *Add to PATH* / desktop icon tasks. VC runtime still bundled as a safety net. Icon is auto-generated from the PNG in CI so no `.ico` asset lives in the tree.
* Runtime: `find_data_dir` now picks `/usr/share/<binary-name>` based on the binary's basename, so `nano-anvil` installed via its own `.deb` / `.rpm` finds `/usr/share/nano-anvil/data/` instead of stepping on `lite-anvil`'s data.

## [2.9.3] - 2026-04-17 -- macOS blank-window fix; Windows VCRUNTIME140.dll fix; macOS CLI symlink, Find fix.

* Fixed blank window / nothing renders on macOS (Intel + ARM).  (Regression fix).
* Fixed `VCRUNTIME140.dll was not found` on clean Windows installs. (Regression fix).
* macOS installer (`install-mac.sh` shipped in the release zip) now creates `lite-anvil` / `nano-anvil` CLI symlinks in `/usr/local/bin` and `/opt/homebrew/bin` so `lite-anvil` resolves from zsh under both Intel and Apple Silicon Homebrew layouts.
* Find: F3 / Enter now reliably centers on off-screen matches — previous scroll target used a hardcoded line height and ignored the find-bar overlay.

## [2.9.2] - 2026-04-17 -- Fix macOS / Windows launch; context-menu clipboard fix; install.sh fix. Find/cursor fixes.

* Fixed `Fatal SDL3 init failed, x11,wayland not available` on launch for macOS (Intel + ARM) and Windows.
* Context-menu Cut / Copy / Paste fix.
* Find: match selection no longer hidden behind the find bar — doc view's rect now excludes the bar's height while active.
* Find: navigating to a match now always vertically centers the view on it.
* Find bar: Left/Right, Ctrl+Left/Right, Home/End, and Delete now work inside the find and replace inputs.
* Find bar: click-to-focus.
  * PCRE2 JIT is enabled (`jit_if_available(true)`), speeds up match-heavy queries.
  * The compiled regex is cached across keystrokes when the query / flags haven't changed.
  * A single `CaptureLocations` is reused across every line instead of being allocated per line.
  * ASCII-only lines skip char-index walking entirely (byte offset == column). Non-ASCII lines walk `char_indices` once per line.
* PgUp / PgDn now scroll the view by a full page alongside the cursor, so the cursor keeps the same relative row position on screen.
* `install.sh` on macOS now prints the `export PATH=...` line and matching shell rc (`.zshrc` / `.bash_profile` / fish config) when `/usr/local/bin` isn't in the user's PATH — common on Apple Silicon setups that prefer `/opt/homebrew/bin`.

## [2.9.1] - 2026-04-17 -- Fix macOS ARM release build.

* Fixed `Undefined symbols for architecture arm64: "___isPlatformVersionAtLeast"` in the Mac ARM release build. (2.9.0 issue).

## [2.9.0] - 2026-04-17 -- Statically-linked C deps, no GPU drivers loaded, big code cleanup.

* Static C deps. SDL3, FreeType, and PCRE2 are now compiled from vendored source and statically linked into `lite-anvil` / `nano-anvil`. No `libSDL3.*` / `libfreetype.*` / `libpcre2-*.*` / `libpng*` / `libbrotli*` / `libbz2.*` / `libz.*` ships with the editor; Linux `ldd` output shrinks from ten libraries to just the libc family. Distro drift (Nix's GPU-enabled SDL3, Homebrew's freetype version, Debian's pcre2-dev) no longer changes the binary's behavior — source builders on NixOS, Arch, Debian, Fedora, macOS, and Windows all get the same editor.
  * `sdl3-sys` uses `build-from-source-static` with `no-sdl-gpu` / `no-sdl-camera` / `no-sdl-joystick` / `no-sdl-haptic` / `no-sdl-hidapi` / `no-sdl-sensor` / `no-sdl-dialog` / `no-sdl-tray` / `no-sdl-power` + `sdl-lean-and-mean`.
  * `pcre2-sys` is forced static via `PCRE2_SYS_STATIC=1` in `.cargo/config.toml`.
  * `freetype-sys` uses its compile-from-source fallback via `FREETYPE2_NO_PKG_CONFIG=1`. FreeType's bundled build leaves PNG / BZIP2 / Brotli / HarfBuzz off by default — the only capability lost is color-emoji PNG font tables and WOFF2, neither of which we render.
  * Source-build prerequisites: cmake + a C compiler. No `libfreetype*-dev` / `libpcre2-dev` / `libsdl3-dev` / vcpkg packages needed on any platform. See `BUILDING.md`.
* No GPU driver load at runtime. `window::init` sets `SDL_FRAMEBUFFER_ACCELERATION=0`, `SDL_VIDEO_DRIVER=x11,wayland`, and `SDL_RENDER_DRIVER=software` before `SDL_Init`. The framebuffer hint is load-bearing: without it, `SDL_GetWindowSurface` silently creates an OpenGL SDL_Renderer for "accelerated" presentation on Linux, which dlopens `libGL` / `libGLX_nvidia` / `libnvidia-glcore` and balloons RSS from ~18 MB to ~70 MB. With the hint, RSS at startup is ~32 MB and no GPU libraries are mapped.
* Packaging cleanup. `release.yml`, `install.sh`, and the local build scripts drop every manual C-lib build step (git-clone SDL + cmake on all platforms, manual freetype / pcre2 on Mac Intel, vcpkg installs on Windows, `brew install freetype pcre2` on Mac ARM, `libfreetype6-dev libpcre2-dev` apt packages on Linux). Windows no longer bundles the VC runtime DLLs either — `+crt-static` plus the static C deps leave the archive as just two exes + `data/` + `data-nano/`. `lite-anvil/build.rs` and `nano-anvil/build.rs` shrink to empty stubs.
* Code cleanup. Removed 12 dead modules (`view_bridge`, `view_types`, `scheduler`, `symbol_index`, `highlighter`, `doc_layout`, `doc_ops`, `event_loop`, `command_view`, `plugins`, `commands`, `views`) — ~2,800 lines of Lua-era residue. Dropped the blanket `#[allow(dead_code)]` attributes in `editor/mod.rs`. Stale Lua-era doc comments rewritten throughout.
  * Extracted three new modules from `main_loop`: `open_doc` (per-tab state + session I/O, ~400 lines), `cmdview` (file-picker helpers, ~290 lines), and `commands_dispatch` (the unified command-dispatch match body, ~840 lines pulled in via `include!()`).
  * Moved `build_render_lines`, `click_to_doc_pos`, `simple_tokenize`, `syntax_color`, `draw_breadcrumb`, and `format_window_title` out of `main_loop` into `doc_view` where they belong.
  * Collapsed three independent nag-bar state trackers (unsaved-changes, reload-from-disk, create-directory) into a single `Nag` enum with four variants — only one prompt can be active at a time and the draw/key paths match on it once.
  * Consolidated 14 scattered `#[cfg(feature = "sdl")]` gates under a new `sdl_only!` macro that bulk-gates a contiguous item block.
  
## [2.8.5] - 2026-04-17 -- Long-path ergonomics in the file picker, breadcrumb, and status bar.

* The Open / Save / Open Recent picker now handles long paths gracefully. The input area horizontally auto-scrolls so the caret is always visible and clips its text to the box (previously long paths bled past the border); `<` / `>` hints appear at the edges when content is scrolled off.
* Suggestions with paths wider than the picker are ellipsis-truncated on the LEFT (`…Downloads\projects\deep\nest\file.rs`), keeping the filename — the part users are actually trying to pick — on screen.
* Widened the picker box from 50% to 70% of the window so most common paths fit without scrolling.
* Breadcrumb above the editor area now drops leading path segments one at a time (prefixed with `… > `) until the tail fits, instead of letting long paths run off the right edge. The filename (final segment) is always visible; when the filename alone is wider than the bar, it's ellipsis-truncated on the LEFT so the extension stays in view.
* Status-bar filename is capped at one-third of the window width, again with left ellipsis, so a runaway filename can't collide with the cursor-position segment or the Lang / EOL / INS group on the right.

## [2.8.4] - 2026-04-17 -- Windows path & console cleanup, Unicode tokenization.

* Merged PR from cp89cyber: Lua `%a` / `%w` / `%l` / `%u` classes now expand to PCRE2 Unicode property classes (`\p{L}`, `[\w\p{M}]`, `\p{Ll}`, `\p{Lu}`), so syntax tokenizers match accented / combining-mark text (Hungarian, Cyrillic, etc.) instead of stopping at the first non-ASCII char.
* No more black console window behind the editor on Windows: release builds now use `windows_subsystem = "windows"` for both `lite-anvil.exe` and `nano-anvil.exe`. Debug builds still get a console so `eprintln!` stays visible when developing.
* Fixed `Theme not found: \\?\C:\Users\...\data/assets/themes/dark_default.json` and the same class of failure for syntax / icon / config / log paths. Two root causes: (1) `std::fs::canonicalize` on Windows returns paths in the `\\?\` extended-length form which rejects forward-slash separators — we now strip that prefix in `runtime.rs`; (2) several hot paths (theme loader, syntax loader, file icons, log, settings, Open File / Save picker) concatenated with `/` via `format!`, mixing separators even after the prefix fix. Those now use `PathBuf::join` so Windows gets `\` throughout and Unix stays unchanged.
* The Open / Save picker accepts and displays the platform's native separator. `~` now expands via `USERPROFILE` on Windows, `Path::is_absolute` replaces the `starts_with('/')` check, and `Ctrl+Left/Right` walk over both `/` and `\` segments.
* Windows user-data now lives under `%APPDATA%\lite-anvil` (Microsoft's recommended location) instead of dropping a folder directly at `%USERPROFILE%\lite-anvil`. `LITE_USERDIR` and `XDG_CONFIG_HOME` continue to override.

## [2.8.3] - 2026-04-17 -- Windows CRT bundling.

* Fixed "Code execution cannot proceed because VCRUNTIME140.dll was not found" on clean Windows installs.

## [2.8.2] - 2026-04-17 -- Line wrap default & fixes, in-place save, Open Recent fix, Nano icon, Windows packaging.

* Line wrapping is now on by default and the preference is persisted across sessions (`Alt+Z` toggles).
* Wrap-aware cursor and mouse: pressing `End` on a wrapped line puts the caret on the last visual row, and clicking past the visual end of a wrapped segment maps to the correct logical column.
* Wrap-aware rendering: line numbers, fold, bookmark, and git-gutter markers only appear on the first visual row of each logical line; indent guides and the trailing `\n` whitespace marker likewise stop repeating on continuation rows. Wrapped rows keep their syntax colors instead of collapsing to default text.
* Saves are now in-place by default, so the executable bit (and other file metadata) survives every save. Set `files.atomic_save = true` in `config.toml` to opt into the old crash-safe write-tmp-then-rename path; the atomic path now explicitly copies permissions, ownership, and xattrs (which covers POSIX ACLs and SELinux labels on Linux).
* Saving into a directory that does not exist prompts `Create it and save? [Y]es [N]o` instead of silently skipping or surprising you; No leaves the file unwritten.
* Nano-Anvil defaults Save / Save As to the user's home directory (`$HOME` / `%USERPROFILE%`) when no project folder is open, instead of the current working directory.
* Nano-Anvil now registers as `nano-anvil` (via `SDL_SetAppMetadata`) and ships `nano-anvil.png` + the updated `.desktop` file, so the Linux taskbar/dock picks the Nano-Anvil icon instead of falling through to Lite-Anvil.
* Open Recent now filters the combined recent-files/recent-projects list as you type and persists every entry immediately (previously the file was only written on clean exit). Files opened from the sidebar, project search, LSP jumps, git-status view, and the command palette are all tracked.
* Removed `core:open-file-dialog`, `core:open-folder-dialog`, and `core:save-as-dialog` commands together with their keybindings. `Ctrl+O`, `Ctrl+Shift+O`, `doc:save`, and `doc:save-as` use the in-app picker uniformly.
* Windows packaging now bundles `SDL3.dll` and the vcpkg runtime DLLs (freetype, pcre2-8, zlib, ...) alongside the exe. First launch no longer fails with a missing-DLL dialog or `0xc000007b`.
* Default font paths are built with `PathBuf::join` so Windows uses `\` throughout instead of mixing `\` and `/`, fixing the occasional `Lilex-Regular.ttf` load failure reported on some Windows installs.

## [2.8.1] - 2026-04-13 -- Markdown preview restored.

* Markdown preview pane. Toggle with `Ctrl+Shift+M` (or `Toggle Markdown Preview` in the command palette) on any `.md` / `.markdown` file. Opens a live 50/50 split alongside the editor; re-parses and re-lays-out as you type.
* Renders headings, paragraphs, blockquotes, fenced code blocks, horizontal rules, ordered and unordered lists, GFM tables with header highlighting, inline code chips, bold/italic/strikethrough, and links.
* Click a link: opens the URL in the OS default browser (`open` on macOS, `start` on Windows, `xdg-open` on Linux). Cursor changes to a hand while hovering a link.
* Click a task-list checkbox: toggles the `[ ]` / `[x]` marker in the source buffer (undoable) and the filled state updates in the preview instantly.
* Mouse wheel is routed to whichever pane the cursor is over; clicks outside the editor's pane don't reposition the source cursor.

## [2.8.0] - 2026-04-12 -- Multi-gigabyte files - performance fixes.

* O(1) dirty check. Replaced per-frame full-buffer hashing with change_id check + per-frame memoization.
* Working signature cache. Hash now keyed by change_id; at most once per edit (small files only).
* Huge-file mode (>50MB). Disables undo snapshots (no-op undo/redo); avoids multi-GB allocations.
* Undo memory cap. Undo+redo limited to 64MB; oldest entries evicted first.
* Efficient size tracking. total_bytes computed once; drives huge-file detection.
* No large-file rescans. Signature fallback only <8MB; larger files skip byte-identical undo detection.

## [2.7.7] - 2026-04-12 -- Simpler/consistent unsaved-changes prompts.

* Quit confirmation is now "X has unsaved changes, quit anyway?" with Yes/No. Y or Enter confirms, N or Esc cancels. When more than one file is modified, the subject becomes "Multiple files".
* Close-tab confirmation is now "X has unsaved changes, close anyway?" with the same Yes/No semantics. The filename is always shown.
* Removed the three-button Save / Don't Save / Cancel nag in favor of a straight two-button Yes/No dialog.

## [2.7.6] - 2026-04-12 -- Large file loading.

* Streaming file loader: reads files in 1MB chunks with a single-pass parse. Eliminates the 3x double-copy (`fs::read` -> `from_utf8_lossy` -> `to_string`) that the old loader used. Peak memory during load dropped from ~4x the file size to ~2.5x.
* Background loading: files larger than 25 MB load on a worker thread so the UI stays responsive.
* Loading overlay: animated progress bar showing MB read / total and percentage while a background load is in progress.
* `large_file.hard_limit_mb` raised from 128 MB to 4 GB. Files exceeding this are refused with an error message.

## [2.7.5] - 2026-04-11 -- Close Project Folder, CLI args, no-project state.

* Fixed: "Close Project Folder" (Ctrl+Alt+W) now works. Saves session and expanded folders, closes all tabs, clears sidebar, resets to empty state.
* CLI: passing a folder opens it as the project folder. Passing a file opens it without a project folder. Restores 1.5.5 behavior.
* No-project state: when no project folder is open, the editor starts without a sidebar. Open File/Folder dialogs default to the home directory.

## [2.7.4] - 2026-04-11 -- macOS & windows rendering fix.

* Fixed blank window on macOS: `SDL_RENDER` is now kept enabled on macOS and Windows (required for Metal/GDI surface presentation).

## [2.7.3] - 2026-04-11 -- macOS SDL3 bundling fix.

* Fixed `@rpath/libSDL3.0.dylib` not being copied to `Contents/Frameworks/`.
* macOS `install-mac.sh` uses `xattr -dr com.apple.quarantine`.

## [2.7.2] - 2026-04-11 -- macOS install script, RPATH fix.

* macOS release zips now include `install-mac.sh`.
* Added `@executable_path/../Frameworks` RPATH via `install_name_tool` in the build scripts.

## [2.7.1] - 2026-04-11 -- macOS launch fix.

* Fixed macOS launch crash ("no LC_RPATH's found").

## [2.7.0] - 2026-04-11 -- Software rendering for all platforms, Nano-Anvil Open file.

* Both Lite-Anvil and Nano-Anvil now use software-only SDL3 (no OpenGL, Vulkan, or GPU drivers loaded) on all platforms. Eliminates ~70MB of GPU driver overhead on Linux/NVIDIA, and reduces memory on macOS and Windows.
* Nano-Anvil: Ctrl+O open file restored.
* CI builds SDL3 from source with `-DSDL_OPENGL=OFF -DSDL_VULKAN=OFF -DSDL_GPU=OFF -DSDL_RENDER=OFF` on Linux, macOS, and Windows.

## [2.6.4] - 2026-04-11 -- Fix render loop memory leak.

* Fixed infinite render loop that boosted memory usage and CPU.

## [2.6.3] - 2026-04-11 -- macOS NanoAnvil.app, file cursor fix.

* macOS: Nano-Anvil now ships as a separate NanoAnvil.app bundle with its own bundled dylibs. Fixes "libfreetype not found" when launching nano-anvil directly.
* macOS: install.sh installs both LiteAnvil.app and NanoAnvil.app to /Applications with CLI symlinks.
* File picker and Save As cursor changed from underscore to vertical bar caret, matching the editor cursor style.

## [2.6.2] - 2026-04-11 -- Session restore for unsaved files.

* Lite-Anvil: unsaved files (including untitled buffers) are now restored on session resume.
* Closing a single tab or switching projects still prompts to save unsaved changes.
* Nano-Anvil: closing with unsaved changes still prompts (no session restore).
* Unsaved files are now consistently named "untitled" everywhere.
* Added Nano-Anvil section to user guide and site.

## [2.6.1] - 2026-04-11 — LSP Inlay hints fix.

* Fixed: LSP inlay hints didn't display on initial editor load without scrolling.

## [2.6.0] - 2026-04-11 — Nano-Anvil, workspace refactor, lazy syntax loading, fixes.

* **Nano-Anvil**: minimal single-file editor. Software-rendered (no GPU drivers), ~28MB RAM. No sidebar, terminal, LSP, git, find-in-files, bookmarks, code folding, or toolbar.
* Cargo workspace: `anvil-core` (shared library), `lite-anvil` (full editor), `nano-anvil` (minimal editor).
* Trait-based subsystem architecture for optional features.
* Renamed `native_loop` to `main_loop`.
* Lazy syntax loading: metadata-only index at startup, full parse on first use.
* Sidebar folder expansion state persisted per project.
* Save As uses built-in text input (Ctrl+Shift+S); native dialog available via palette.
* Nano-Anvil: 2 fonts, halved glyph cache, no glyph prewarm, undo capped at 100, no-GL SDL3.
* Fixed: save on untitled files opens Save As prompt.
* Fixed: nag bar dismisses all overlays and appears immediately.
* Fixed: autocomplete popup hidden during Save As input.
* Desktop files for both Lite-Anvil and Nano-Anvil with full mime type coverage.

## [2.5.0] - 2026-04-10 — Replace in files, find/replace shortcut overhaul.

* Replace in files (`Alt+Shift+F`): project-wide find-and-replace with live preview. Search input, replace input, Tab to switch fields, Enter to preview matches, Ctrl+Enter to execute replace across all matching files. Open buffers are reloaded after replacement.
* Replace in file shortcut changed from `Ctrl+H` to `Alt+F` (avoids Cmd+H conflict on macOS).
* Removed duplicate `Ctrl+R` binding for replace.
* Find in files (`Ctrl+Shift+F`) no longer conflicts with focus mode (focus mode moved to palette only).
* Both Find in Files and Replace in Files now have clear "FIND IN FILES" / "REPLACE IN FILES" title bars, match counts, and the same toggle options as single-file find: `Alt+R` regex, `Alt+W` whole word, `Alt+I` case-insensitive. Grep flags (`-F`, `-w`, `-i`) update live as toggles change.

## [2.4.3] - 2026-04-10 — Scroll discipline.

* Scrolling now only occurs from: mouse wheel, scrollbar, keyboard navigation (arrows, Home/End, PageUp/Down, Ctrl+arrows).

## [2.4.2] - 2026-04-10 — Minor render bug fix.

* Tab and sidebar gap fix.

## [2.4.1] - 2026-04-10 — Scroll and rendering fixes.

* Mouse wheel over sidebar scrolls sidebar only; over editor scrolls editor only.
* Sidebar width limit raised to 90% of window (was hardcoded 600px).
* Sidebar header (folder name) stays pinned when scrolling the file tree.
* Fixed project-switch render cache bug: switching projects no longer shows stale content from the previous project's active file.
* Render line caching: `build_render_lines` (syntax tokenization) is skipped on cursor-only redraws when the buffer hasn't changed, reducing per-frame work.

## [2.4.0] - 2026-04-10 — File type icons, hidden file toggle, check for updates, other ergonomic improvements.

* Sidebar file icons now use the Seti icon font (MIT, from VS Code's Seti theme) for recognizable per-language glyphs -- Rust gear, Python snake, Go gopher, JS/TS badges, HTML brackets, Docker whale, and 90+ more. Each icon renders in the language's signature color. The Seti font (`data/fonts/seti.ttf`) scales perfectly at any DPI. Icon mappings (extension to codepoint + color) are in `data/assets/file_icons.json`. Directories keep the existing folder icon.
* Added RGBA image blitting to the render cache (`DrawImageCmd`) for future use.
* Toggle Hidden Files: new command `Toggle Hidden Files` in the command palette shows/hides dotfiles in the sidebar. Displays an info banner confirming the current state.
* Check for Updates: new command `Check For Updates` in the command palette. Queries the GitHub releases API via curl and shows a banner with the result ("Up to date" or "New version available: vX.Y.Z").
* Open file at line: `lite-anvil src/main.rs:42` (CLI) and the file picker (`Ctrl+O`) both support `:N` suffix to open a file scrolled to a specific line.
* Format on paste: pasted text has its leading whitespace automatically converted to match the document's indent style (tabs to spaces or vice versa, preserving relative depth). Enabled by default; set `format_on_paste = false` in config.toml to disable.
* Git Blame: `Git Blame` in the command palette toggles per-line blame annotations (author + date) shown at the right edge of the editor. Re-runs `git blame --porcelain` for the active file on toggle.
* Git Log: `Git Log` in the command palette opens a scrollable overlay showing the last 50 commits for the active file (hash, date, message). Navigate with Up/Down, dismiss with Esc.
* Per-project session memory: switching projects saves the current open files and active tab, and restores them when you switch back. Stored per-project in the user config directory.
* Default scrollbar width doubled from 4px to 8px.
* Smooth scrolling: all auto-scroll (cursor off-screen, find, bookmarks) now animates via the lerp instead of snapping.
* Clicking in the document no longer triggers auto-scroll.

## [2.3.0] - 2026-04-10 — Bookmarks, find-in-selection, graceful font fallback.

* Bookmarks: `Ctrl+F4` toggles a bookmark on the current line, `F4` jumps to the next bookmark, `Shift+F4` to the previous. Bookmarked lines show an accent-colored marker in the gutter. Bookmarks are per-document and wrap around.
* Find in selection: when a multi-line selection is active and `Ctrl+F` is pressed, search automatically scopes to the selected region. Toggle with `Alt+S` inside the find bar. The hint row shows `[x]` next to `Sel` when active.
* Graceful font fallback: if a configured font path fails to load (e.g. missing file, bad path), the editor now resets to the built-in default fonts and shows a warning banner instead of crashing. If even the defaults fail, a clear error message is printed to stderr before exiting.
* SDL Window title fix - consistent across platforms.

## [2.2.2] - 2026-04-10 — Mac font fix, Cmd-as-Ctrl on by default, docs overhaul.

* Fixed `FT_New_Face failed (data/fonts/Lilex-Regular.ttf)` on macOS: `current_exe()` can return a relative path on macOS; we now canonicalize it before deriving the data directory. Also added a `Contents/Resources/data/` lookup for standard app-bundle layouts.
* `mac_command_as_ctrl` now defaults to `true` on macOS (matching native Mac conventions: Cmd+S saves). Set to `false` in config.toml to use Ctrl-only behavior.
* Documentation overhaul across the GitHub Pages site:
    - **User Guide**: removed 15+ phantom shortcuts that were documented but never bound (bookmarks, most LSP actions, Alt+S find-in-selection, Ctrl+Shift+L select-all, Alt+Shift+F format, F10 line-wrap). Fixed move-line shortcut from `Ctrl+Shift+Up/Down` to `Ctrl+Up/Down`. Fixed line-wrapping shortcut from `F10` to `Alt+Z`. Fixed swapped Ctrl+F12 / Ctrl+Shift+F12 descriptions. Added find-bar toggles (Alt+R/W/I), project search, code folding, terminal, navigation, and Git status shortcuts. Documented `mac_command_as_ctrl` config option and keybindings section. Added comment-toggle language-awareness note.
    - **Installation**: fixed Debian package command from `cargo deb -p forge-core` to `cargo deb`.
    - **Command Palette**: updated to reflect the filtered palette (Git-prefixed commands, no raw key-input entries).
* Rebuilt `docs/` from `docs_src/` via mkdocs.

## [2.2.1] - 2026-04-09 — Mac Ctrl/Cmd behavior reverted, opt-in via config.

* Reverted the macOS Ctrl→Cmd alias that shipped in 2.2.0. On Mac, the default shortcuts are now the same as Linux/Windows — press `Ctrl+S` to save, not `Cmd+S`.
* New config option `mac_command_as_ctrl` (default `false`, top-level in `config.toml`). When enabled on macOS, the Command key folds into Control so `Cmd+S` acts like `Ctrl+S`, matching standard Mac conventions. No-op on other platforms.
* Removed the now-dead `ctrl+X → cmd+X` binding duplication from `NativeKeymap::with_defaults` and the two `macos_alias_*` tests that exercised it.

## [2.2.0] - 2026-04-09 — Enhanced Find/Replace, Command Palette fixes, comment fixes.

* Find/Replace bar now appears at the top of the editor (just below the tabs and breadcrumb), matching the file picker, command palette, and project search. Previously it sat in the lower-left, splitting the user's attention between two corners of the window.
* Find bar now spans only the active editor's column instead of the full window, so it visually belongs to the document being searched.
* Live search: typing in the Find input immediately jumps to the next match from where the cursor was when Find opened.
* Match counter (e.g. `3/12`) shown on the right of the Find row; renders in the error color when the query has no matches.
* New toggles with hint row beneath the inputs: `Alt+R` regex, `Alt+W` whole word, `Alt+I` case-insensitive. Each shows a `[x]` / `[ ]` indicator.
* `F3` jumps to the next match, `Shift+F3` to the previous, both with wraparound. Works whether the Find bar is open or closed (as long as a query exists).
* `Enter` / `Shift+Enter` inside the Find input also navigate next / previous.
* Command Palette Cleanup: filtered out raw key-input commands (Backspace, Return, Tab, cursor movement, selection extension, multi-cursor creation, and the `command:` / `context-menu:` / `dialog:` namespaces) so the palette only lists meaningful actions.
* Git commands in the palette are now prefixed with "Git" — `Git Pull`, `Git Push`, `Git Commit`, `Git Stash` — instead of bare verbs.
* `Ctrl+/` toggle line comments now picks the right marker for the active language by reading the `comment` field from the per-language syntax JSON.
* Updated tagline / description across `README.md`, `Cargo.toml`, `mkdocs.yml`, and the GitHub Pages site to drop "lightweight" — now consistently "code editor".

## [2.1.3] - 2026-04-09 — File picker editing, sidebar overflow, docs rebuild.

* Fixed file/folder open picker text editing: arrow keys, Home, End, Delete, and Ctrl+Left / Ctrl+Right now work.
* Fixed file/folder open picker root path: now opens with the absolute project directory instead of `./`.
* Fixed sidebar entry overflow: long folder/file names.
* Refactored sidebar toolbar click handler to delegate to the unified `dispatch_command!` macro.
* New `scripts/build-docs.sh`: rebuilds the static documentation site (`docs/`) from `docs_src/` via `mkdocs build`.
* Scroll weirdness fixes.
* Fixed horizontal scroll: long lines no longer slide under the line-number gutter;

## [2.1.2] - 2026-04-09 — Dispatch refactor + recent folder restore + version + keybinding fixes.

* Refactored command dispatch into a single `dispatch_command!` macro. Both the keyboard binding path and the command palette now share one match block instead of duplicating logic in two places. Adding a new command is now a one-place edit.
* Fixed Ctrl+Shift+R: actually opens the Open Recent picker (was a victim of the duplicate-dispatch bug; the refactor above prevents this class of bug).
* Fixed recent folder not loading on restart: session save now persists `project_root` even when no files are open. Previously the session was cleared in that case, so the project folder was lost across restarts.
* Fixed About: now reports the actual package version instead of a hardcoded `v2.0.0`.

## [2.1.1] - 2026-04-09 — Release pipeline + test workflow fixes.

* Fixed release workflow regression: pinned `softprops/action-gh-release` to `@v2` (`@v3` does not exist).
* Fixed Windows test job: vcpkg DLLs now on PATH so the test binary loads at runtime.
* Fixed macOS keymap alias: `cmd` added to modifier ordering so Cmd+ aliases normalize correctly.

## [2.1.0] - 2026-04-09 — Windows terminal, Open Recent, macOS keys, many fixes.

* Windows terminal support via piped stdin/stdout with reader thread (cmd.exe /Q).
* Open Recent (Ctrl+Shift+R, auto-aliased to Cmd+Shift+R on macOS : combined recent files and folders list. Files open directly, folders switch project. Listed in the splash screen and README.
* Recent files and folders persisted to session storage (max 100 files, 20 projects).
* macOS: all Ctrl+ keybindings automatically aliased to Cmd+ (matching 1.5.5 behavior).
* Fixed mouse selection and typing into selected text bugs.
* Fixed selection and end key.
* Fixed horizontal scrolling.
* Fixed GitHub Actions: upgrade download-artifact to v6, gh-release to v3 (Node.js 24).
* Fixed CI and release.sh version parsing for single-package Cargo.toml layout.
* Local build scripts for linux, mac, and windows. install.sh uses the correct one.
* Increased text coverage.

## [2.0.2] - 2026-04-09 — Windows build fix, caret/selection fixes.

* Full dummy stubs for TerminalInstance fields (inner, tbuf) on non-Unix platforms.
* Terminal emulator unavailable on Windows (conpty planned).
* Fix: delete/backspace with active selection now deletes the selection instead of a single character.
* Fix: selection highlight no longer extends one character past the cursor position.

## [2.0.1] - 2026-04-09 — Windows build fix.

* Fix Windows compilation: gate Unix-only PTY/process code behind `#[cfg(unix)]`.
* Provide dummy terminal panel on non-Unix platforms.

## [2.0.0] - 2026-04-08 — Entirely Rust - Lua Removed + UI/Ux refinement.

* 100% Rust.
* mlua (and Lua support, plugins) removed in favor of a fully native editor.
* LSP startup consistency.
* Minor memory optimizations.
* Optional file dialog.
* Code reorganization.
* Smoother more correct syntax highlighting.
* UI/Ux improvements including consistency.
* Command palette command-naming simplification.

## [1.5.5] - 2026-04-06 — Save crash and project folder memory fixes.

* Fix crash when saving a file with no folder open.
* Fix project folder being forgotten on restart.

## [1.5.4] - 2026-04-03 — Folder open post startup fix.

* Fix crash on folder open after restart: `_goto_positions` table was nil when the arg-parsing block didn't run (e.g. on `core:restart`).

## [1.5.3] - 2026-04-03 — Command line arguments.

* Add `-n` / `--new-window`: launch with no project and a blank file, skipping session/workspace/backup restore.
* Add `-g` / `--goto <file:line[:col]>`: open a file at a specific line and optional column.
* Add `-h` / `--help`: print usage and exit.
* Support `file:line[:col]` syntax in bare path arguments (e.g. `lite-anvil src/main.rs:42:10`).
* Invalid file paths and unknown flags are logged and skipped.

## [1.5.2] - 2026-04-02 — Project session fixes.

* Fix restart after closing a project opening `/` as a project: now starts with no project and a blank sidebar.
* Fix `core.exit` being defined after plugin load, causing the workspace plugin's exit wrapper to be overwritten. Workspace state (open files/tabs) now saves correctly on quit.

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
