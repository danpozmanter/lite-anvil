//! Editor main loop.
//!
//! The module is split into two visually-distinct sections: `pub fn run`
//! and the big `#[cfg(feature = "sdl")] fn run` it delegates to live
//! near the top; the bottom 1.4k lines are supporting helpers most of
//! which only make sense when the `sdl` feature is on. Those helpers
//! are bulk-gated via the `sdl_only!` macro below so each one doesn't
//! need its own `#[cfg(feature = "sdl")]` attribute.

/// Wrap a block of items with `#[cfg(feature = "sdl")]`. Lets a long
/// run of SDL-only helpers share a single gate declaration at the top
/// instead of attributing every individual fn.
macro_rules! sdl_only {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "sdl")]
            $item
        )*
    };
}

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
#[cfg(feature = "sdl")]
use std::time::Instant;

use crossbeam_channel::Receiver;
use notify::{Event, RecursiveMode, Watcher};

// Module-level editor mode. Set once at startup, read by helper functions.
thread_local! {
    static SINGLE_FILE_MODE: Cell<bool> = const { Cell::new(false) };
    static MAX_OPEN_TABS: Cell<usize> = const { Cell::new(8) };
}

/// Whether the editor is running in single-file mode (Nano-Anvil).
fn is_single_file() -> bool {
    SINGLE_FILE_MODE.with(|c| c.get())
}

/// Whether git integration is active (inverse of single-file mode).
fn use_git() -> bool {
    !is_single_file()
}

pub(crate) fn can_open_another_tab(_current: usize) -> bool {
    // The open tab limit has been removed; any number of tabs may be open.
    true
}

/// Commands which can change the active document's content or edit metadata.
/// Used to enforce configured read-only behavior for large files at every
/// command-dispatch entry point.
fn is_document_mutation_command(command: &str) -> bool {
    matches!(
        command,
        "doc:convert-indentation"
            | "doc:toggle-line-endings"
            | "doc:undo"
            | "doc:redo"
            | "doc:cut"
            | "doc:paste"
            | "doc:insert-list-item"
            | "doc:insert-checkbox-item"
            | "doc:format"
            | "doc:newline"
            | "doc:newline-below"
            | "doc:newline-above"
            | "doc:backspace"
            | "doc:delete"
            | "doc:delete-to-previous-word-start"
            | "doc:delete-to-next-word-end"
            | "doc:indent"
            | "doc:unindent"
            | "doc:toggle-line-comments"
            | "doc:move-lines-up"
            | "doc:move-lines-down"
            | "doc:duplicate-lines"
            | "doc:delete-lines"
            | "doc:join-lines"
            | "core:sort-lines"
    )
}

use crate::editor::buffer;
use crate::editor::config::NativeConfig;
#[cfg(feature = "sdl")]
use crate::editor::context_menu::ContextMenu;
use crate::editor::context_menu::MenuItem;
#[cfg(feature = "sdl")]
use crate::editor::doc_view::{
    DocView, RenderLine, SYNTAX_COLORS, build_render_lines, click_to_doc_pos, syntax_color,
};
#[cfg(feature = "sdl")]
use crate::editor::empty_view::EmptyView;
#[cfg(feature = "sdl")]
use crate::editor::event::{EditorEvent, MouseButton};
#[cfg(feature = "sdl")]
use crate::editor::keymap::NativeKeymap;
#[cfg(feature = "sdl")]
use crate::editor::lsp;
#[cfg(feature = "sdl")]
use crate::editor::lsp_client::*;
#[cfg(feature = "sdl")]
use crate::editor::picker;
#[cfg(feature = "sdl")]
use crate::editor::status_view::{StatusItem, StatusView};
use crate::editor::storage;
#[cfg(feature = "sdl")]
use crate::editor::style_ctx::StyleContext;
#[cfg(feature = "sdl")]
use crate::editor::terminal_panel::*;
#[cfg(feature = "sdl")]
use crate::editor::tokenizer::{self, CompiledSyntax};
#[cfg(feature = "sdl")]
use crate::editor::view::{UpdateContext, View};

/// Append a timestamped message to the log file in the user directory.
#[cfg(feature = "sdl")]
fn log_to_file(userdir: &str, msg: &str) {
    let path = Path::new(userdir).join("lite-anvil.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

/// A single entry in the file tree sidebar.
struct SidebarEntry {
    name: String,
    path: String,
    is_dir: bool,
    depth: usize,
    expanded: bool,
}

/// Width of the sidebar in logical pixels.
const DEFAULT_SIDEBAR_W: f64 = 200.0;
const MIN_SIDEBAR_W: f64 = 100.0;
/// Editor's share of the editor|markdown-preview split, as a fraction of the
/// shared content area. Stored as a fraction (not pixels) so the split tracks
/// the window width across resizes; persisted per app via the `session` store.
const DEFAULT_PREVIEW_SPLIT: f64 = 0.5;
const MIN_PREVIEW_SPLIT: f64 = 0.2;
const MAX_PREVIEW_SPLIT: f64 = 0.8;
/// Collapse redundant `.` segments in a path string. Preserves a single
/// leading `./` for relative paths and leaves absolute paths intact.
/// Does not touch `..` segments (we don't want to silently traverse symlinks).
pub(crate) fn normalize_path(p: &str) -> String {
    use std::path::Component;
    let path = Path::new(p);
    let mut out = PathBuf::new();
    let mut started_with_curdir = false;
    let mut has_anchor = false;
    for comp in path.components() {
        match comp {
            Component::CurDir => {
                if !has_anchor && !started_with_curdir {
                    out.push(".");
                    started_with_curdir = true;
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                out.push(comp.as_os_str());
                has_anchor = true;
            }
            _ => {
                out.push(comp.as_os_str());
                has_anchor = true;
            }
        }
    }
    if out.as_os_str().is_empty() {
        ".".to_string()
    } else {
        out.to_string_lossy().to_string()
    }
}

/// Filter + sort `sidebar_entries` for notes-mode display.
/// Returns indices into `sidebar_entries` in the order they should be
/// rendered. `sort_mode`: 0 = A-Z asc, 1 = A-Z desc, 2 = recent-first,
/// 3 = oldest-first. Filter is a case-insensitive substring match on
/// the entry name (empty = no filter).
fn compute_notes_display_order(
    entries: &[SidebarEntry],
    search: &str,
    sort_mode: u8,
) -> Vec<usize> {
    let needle = search.to_lowercase();
    let mut indices: Vec<usize> = (0..entries.len())
        .filter(|&i| {
            if needle.is_empty() {
                true
            } else {
                entries[i].name.to_lowercase().contains(&needle)
            }
        })
        .collect();
    match sort_mode {
        0 => indices.sort_by(|&a, &b| {
            entries[a]
                .name
                .to_lowercase()
                .cmp(&entries[b].name.to_lowercase())
        }),
        1 => indices.sort_by(|&a, &b| {
            entries[b]
                .name
                .to_lowercase()
                .cmp(&entries[a].name.to_lowercase())
        }),
        2 | 3 => {
            let mtime = |path: &str| -> std::time::SystemTime {
                std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            };
            indices.sort_by(|&a, &b| {
                let ta = mtime(&entries[a].path);
                let tb = mtime(&entries[b].path);
                if sort_mode == 2 {
                    tb.cmp(&ta)
                } else {
                    ta.cmp(&tb)
                }
            });
        }
        _ => {}
    }
    indices
}

/// Wrapper around `scan_directory` that, in notes-mode, flattens to a
/// `*.md`-only top-level list (no folders, no recursion).
fn scan_for_sidebar(notes_mode: bool, dir: &str, show_hidden: bool) -> Vec<SidebarEntry> {
    let entries = scan_directory(dir, 0, show_hidden);
    if notes_mode {
        entries
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_lowercase().ends_with(".md"))
            .collect()
    } else {
        entries
    }
}

/// Scan a directory non-recursively and return sorted sidebar entries at the given depth.
fn scan_directory(dir: &str, depth: usize, show_hidden: bool) -> Vec<SidebarEntry> {
    let mut entries = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        return entries;
    };
    for entry in read.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        entries.push(SidebarEntry {
            name,
            path: entry.path().to_string_lossy().to_string(),
            is_dir: meta.is_dir(),
            depth,
            expanded: false,
        });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    entries
}

/// Expand previously-saved sidebar folders, inserting children as needed.
fn restore_expanded_folders(
    sidebar_entries: &mut Vec<SidebarEntry>,
    userdir: &std::path::Path,
    show_hidden: bool,
    session_key: &str,
) {
    let key = format!("{session_key}_expanded");
    let Ok(Some(data)) = storage::load_text(userdir, "project_session", &key) else {
        return;
    };
    let Ok(expanded) = serde_json::from_str::<Vec<String>>(&data) else {
        return;
    };
    let expanded_set: HashSet<&str> = expanded.iter().map(|s| s.as_str()).collect();
    // Iterate by index because expanding inserts children, shifting subsequent indices.
    let mut i = 0;
    while i < sidebar_entries.len() {
        if sidebar_entries[i].is_dir
            && !sidebar_entries[i].expanded
            && expanded_set.contains(sidebar_entries[i].path.as_str())
        {
            sidebar_entries[i].expanded = true;
            let children = scan_directory(
                &sidebar_entries[i].path,
                sidebar_entries[i].depth + 1,
                show_hidden,
            );
            let insert_at = i + 1;
            for (j, child) in children.into_iter().enumerate() {
                sidebar_entries.insert(insert_at + j, child);
            }
        }
        i += 1;
    }
}

/// Save the set of expanded sidebar folder paths for a project.
fn save_expanded_folders(
    sidebar_entries: &[SidebarEntry],
    userdir: &std::path::Path,
    session_key: &str,
) {
    let expanded: Vec<&str> = sidebar_entries
        .iter()
        .filter(|e| e.is_dir && e.expanded)
        .map(|e| e.path.as_str())
        .collect();
    if expanded.is_empty() {
        let _ = storage::clear(
            userdir,
            "project_session",
            Some(&format!("{session_key}_expanded")),
        );
    } else {
        let _ = storage::save_text(
            userdir,
            "project_session",
            &format!("{session_key}_expanded"),
            &serde_json::to_string(&expanded).unwrap_or_default(),
        );
    }
}

/// Re-expand sidebar directories from an in-memory set of previously expanded paths.
fn expand_sidebar_from_set(
    sidebar_entries: &mut Vec<SidebarEntry>,
    expanded: &HashSet<String>,
    show_hidden: bool,
) {
    let mut i = 0;
    while i < sidebar_entries.len() {
        if sidebar_entries[i].is_dir
            && !sidebar_entries[i].expanded
            && expanded.contains(&sidebar_entries[i].path)
        {
            sidebar_entries[i].expanded = true;
            let children = scan_directory(
                &sidebar_entries[i].path,
                sidebar_entries[i].depth + 1,
                show_hidden,
            );
            let insert_at = i + 1;
            for (j, child) in children.into_iter().enumerate() {
                sidebar_entries.insert(insert_at + j, child);
            }
        }
        i += 1;
    }
}

/// A file-type icon: Seti font codepoint + color.
struct FileIcon {
    /// Unicode codepoint in the Seti icon font.
    codepoint: u32,
    color: [u8; 4],
}

/// Load file-extension to icon mapping from the JSON config.
fn load_file_icons(datadir: &str) -> std::collections::HashMap<String, FileIcon> {
    let path = Path::new(datadir).join("assets").join("file_icons.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return std::collections::HashMap::new();
    };
    let Ok(map) =
        serde_json::from_str::<std::collections::HashMap<String, serde_json::Value>>(&text)
    else {
        return std::collections::HashMap::new();
    };
    map.into_iter()
        .filter_map(|(ext, val)| {
            let obj = val.as_object()?;
            let codepoint = obj.get("codepoint")?.as_u64()? as u32;
            let color = obj.get("color")?.as_str().and_then(parse_hex_color)?;
            Some((ext, FileIcon { codepoint, color }))
        })
        .collect()
}

/// Parse "#rrggbb" into [r, g, b, 255].
fn parse_hex_color(s: &str) -> Option<[u8; 4]> {
    let hex = s.strip_prefix('#')?;
    if hex.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b, 255])
}

/// File watcher state for autoreload on external changes.
///
/// We watch each file's *parent directory* (not the file inode) and
/// filter events by filename. This is the standard robust pattern for
/// inotify-backed watchers: an external editor saving via write-to-temp
/// and atomic rename replaces the file's inode, which silently breaks a
/// file-inode watch (only the first save fires and all subsequent ones
/// miss). Watching the directory sidesteps that entirely.
pub(crate) struct AutoreloadState {
    watcher: Option<notify::RecommendedWatcher>,
    rx: Option<Receiver<notify::Result<Event>>>,
    /// Watched file paths mapped to the directory registered with
    /// notify. Used to filter events and to look up which dir to
    /// decrement in `unwatch`.
    watched_files: HashMap<String, PathBuf>,
    /// Reference count per watched directory so the last file in a
    /// directory unwatches it, but shared dirs stay watched while any
    /// of their files are open.
    watched_dirs: HashMap<PathBuf, usize>,
}

impl AutoreloadState {
    fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .ok();
        Self {
            watcher,
            rx: Some(rx),
            watched_files: HashMap::new(),
            watched_dirs: HashMap::new(),
        }
    }

    /// Start watching a file path for external changes.
    pub(crate) fn watch(&mut self, path: &str) {
        if self.watched_files.contains_key(path) {
            return;
        }
        let file_path = PathBuf::from(path);
        let dir = match file_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => return,
        };
        let count = self.watched_dirs.entry(dir.clone()).or_insert(0);
        if *count == 0 {
            if let Some(ref mut w) = self.watcher {
                if w.watch(&dir, RecursiveMode::NonRecursive).is_err() {
                    self.watched_dirs.remove(&dir);
                    return;
                }
            }
        }
        *self.watched_dirs.get_mut(&dir).expect("just inserted") += 1;
        self.watched_files.insert(path.to_string(), dir);
    }

    /// Stop watching a file path.
    pub(crate) fn unwatch(&mut self, path: &str) {
        let Some(dir) = self.watched_files.remove(path) else {
            return;
        };
        if let Some(count) = self.watched_dirs.get_mut(&dir) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.watched_dirs.remove(&dir);
                if let Some(ref mut w) = self.watcher {
                    let _ = w.unwatch(&dir);
                }
            }
        }
    }

    /// Drain pending events and return paths of modified files.
    fn poll_changed(&self) -> Vec<String> {
        let mut changed = HashSet::new();
        if let Some(ref rx) = self.rx {
            while let Ok(event) = rx.try_recv() {
                if let Ok(ev) = event {
                    use notify::EventKind;
                    // Creates count too: an atomic save rename replaces
                    // the target with a fresh inode, which arrives as a
                    // Create event on the dir watcher.
                    let is_interesting =
                        matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_));
                    if !is_interesting {
                        continue;
                    }
                    for p in &ev.paths {
                        if let Some(s) = p.to_str() {
                            if self.watched_files.contains_key(s) {
                                changed.insert(s.to_string());
                            }
                        }
                    }
                }
            }
        }
        // One buffered write may still produce several backend events. Do the
        // metadata comparison only after paths have been deduplicated, so a
        // save can never turn an event burst into a UI-thread syscall loop.
        changed.retain(|path| !buffer::is_current_saved_file(path));
        changed.into_iter().collect()
    }
}

/// Watches project directories so the sidebar refreshes when the filesystem changes.
struct SidebarWatcher {
    watcher: Option<notify::RecommendedWatcher>,
    rx: Option<Receiver<notify::Result<Event>>>,
    watched_dirs: HashSet<PathBuf>,
}

impl SidebarWatcher {
    fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .ok();
        Self {
            watcher,
            rx: Some(rx),
            watched_dirs: HashSet::new(),
        }
    }

    fn watch_dir(&mut self, dir: &str) {
        let path = PathBuf::from(dir);
        if self.watched_dirs.contains(&path) {
            return;
        }
        if let Some(ref mut w) = self.watcher {
            if w.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                self.watched_dirs.insert(path);
            }
        }
    }

    fn unwatch_dir(&mut self, dir: &str) {
        let path = PathBuf::from(dir);
        if self.watched_dirs.remove(&path) {
            if let Some(ref mut w) = self.watcher {
                let _ = w.unwatch(&path);
            }
        }
    }

    fn unwatch_all(&mut self) {
        let dirs: Vec<PathBuf> = self.watched_dirs.drain().collect();
        if let Some(ref mut w) = self.watcher {
            for dir in &dirs {
                let _ = w.unwatch(dir);
            }
        }
    }

    /// Returns true if any directory-listing change (create/remove/rename) was detected.
    fn poll_changed(&self) -> bool {
        let Some(ref rx) = self.rx else {
            return false;
        };
        let mut changed = false;
        while let Ok(event) = rx.try_recv() {
            if let Ok(ev) = event {
                use notify::EventKind;
                if matches!(
                    ev.kind,
                    EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(notify::event::ModifyKind::Name(_))
                ) {
                    changed = true;
                }
            }
        }
        changed
    }
}

/// Comment style chosen for the toggle-line-comments command.
#[derive(Debug, Clone)]
enum CommentMarker {
    /// `prefix` is prepended after the indent (e.g. `//` for Rust, `#` for Python).
    Line(String),
    /// `(open, close)` wraps each line individually (e.g. `<!-- ... -->` for HTML).
    /// Used for languages that have no line-comment form.
    Block(String, String),
}

/// Resolve the comment marker for a document based on its filename's matched
/// syntax. Returns `None` when no syntax matches or when the language has
/// neither a line- nor a block-comment form — callers must treat that as
/// "no-op" rather than substituting a default.
fn comment_marker_for_path(
    path: &str,
    index: &[crate::editor::syntax::SyntaxEntry],
) -> Option<CommentMarker> {
    if path.is_empty() {
        return None;
    }
    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    let entry = crate::editor::syntax::match_syntax_entry(filename, index)?;
    if let Some(line) = &entry.comment {
        return Some(CommentMarker::Line(line.clone()));
    }
    entry
        .block_comment
        .as_ref()
        .map(|(o, c)| CommentMarker::Block(o.clone(), c.clone()))
}

/// Whether the active filename matches a configured language syntax. Files
/// without a match are plain text and should not receive language editing
/// behavior such as automatic indentation.
fn has_syntax_for_path(path: &str, index: &[crate::editor::syntax::SyntaxEntry]) -> bool {
    if path.is_empty() {
        return false;
    }
    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    crate::editor::syntax::match_syntax_entry(filename, index).is_some()
}

/// Truncate a tab name to `max_chars` characters, appending an ellipsis when
/// the original is longer. Operates on Unicode scalar values so multi-byte
/// filenames don't get cut mid-codepoint.
fn truncate_tab_name(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        return name.to_string();
    }
    let prefix: String = name.chars().take(max_chars).collect();
    format!("{prefix}...")
}

fn push_path_copy_items(items: &mut Vec<MenuItem>, copy_command: &str, relative_command: &str) {
    items.push(MenuItem {
        text: String::new(),
        info: None,
        command: None,
        separator: true,
    });
    items.push(MenuItem {
        text: "Copy Path".into(),
        info: None,
        command: Some(copy_command.into()),
        separator: false,
    });
    items.push(MenuItem {
        text: "Copy Relative Path".into(),
        info: None,
        command: Some(relative_command.into()),
        separator: false,
    });
}

/// Map a Markdown fenced-code `lang` tag (e.g. from ```` ```python ````) to
/// the file extension our bundled syntax index keys on. Unknown or empty
/// tags fall back to the tag itself so anything the index already matches
/// directly (like `sh`, `rs`, `go`) still works without a special case.
fn markdown_lang_to_ext(lang: &str) -> &str {
    match lang.to_ascii_lowercase().as_str() {
        "rust" => "rs",
        "gossamer" => "gos",
        "python" | "python3" => "py",
        "javascript" | "node" => "js",
        "typescript" => "ts",
        "shell" | "bash" | "zsh" => "sh",
        "c++" | "cplusplus" => "cpp",
        "c#" | "csharp" => "cs",
        "golang" => "go",
        "yaml" => "yml",
        "markdown" => "md",
        "ruby" => "rb",
        "kotlin" => "kt",
        "ocaml" => "ml",
        "perl" => "pl",
        "elixir" => "ex",
        _ => lang,
    }
}

/// Run the editor main loop. Returns true if restart requested.
#[cfg(feature = "sdl")]
pub fn run(
    mut config: NativeConfig,
    _args: &[String],
    datadir: &str,
    userdir: &str,
    subsystems: crate::editor::subsystems::EditorSubsystems,
) -> bool {
    let single_file_mode = !subsystems.has_sidebar();
    SINGLE_FILE_MODE.with(|c| c.set(single_file_mode));
    MAX_OPEN_TABS.with(|c| c.set(config.max_tabs.max(1) as usize));
    crate::editor::open_doc::set_large_file_config(&config.large_file);
    if single_file_mode {
        crate::renderer::font::set_glyph_cache_limit(1024);
        crate::renderer::font::set_skip_prewarm(true);
        config.max_undos = 100;
    }
    // macOS: aggressively lower the glyph-cache ceiling + skip ASCII
    // prewarm on every auxiliary font (h1/h2/h3/big/icon_big/seti).
    // Only `ui` and `code` see sustained glyph traffic; the rest barely
    // touch their cache, so warming 95 ASCII glyphs per face wastes ~2-3 MB
    // upfront. macOS pays the highest price for this since Metal keeps
    // each glyph's backing bitmap resident in the GPU's shared memory.
    #[cfg(target_os = "macos")]
    {
        crate::renderer::font::set_glyph_cache_limit(512);
        crate::renderer::font::set_skip_prewarm(true);
    }

    // Create window.
    if !crate::window::restore_window() {
        let window_title = if subsystems.has_notes_mode() {
            "Note Anvil"
        } else if subsystems.has_sidebar() {
            "Lite Anvil"
        } else {
            "Nano Anvil"
        };
        if let Err(e) = crate::window::create_window(window_title) {
            log::error!("Window creation failed: {e}");
            return false;
        }
    }

    // Restore saved window size/position.
    let userdir_path = Path::new(userdir);
    if let Ok(Some(win_json)) = storage::load_text(userdir_path, "session", "window") {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&win_json) {
            if let (Some(w), Some(h), Some(x), Some(y)) = (
                val["w"].as_i64(),
                val["h"].as_i64(),
                val["x"].as_i64(),
                val["y"].as_i64(),
            ) {
                crate::window::set_window_size(w as i32, h as i32, x as i32, y as i32);
            }
        }
    }

    // Enable text input events from SDL.
    crate::window::start_text_input();

    // Load fonts and build style from config.
    // Restore saved font size if available.
    let mut config = config;
    let userdir_path = std::path::Path::new(userdir);
    if let Ok(Some(size_str)) =
        crate::editor::storage::load_text(userdir_path, "session", "font_size")
    {
        if let Ok(size) = size_str.trim().parse::<f32>() {
            let base_size = (size / crate::window::get_display_scale() as f32) as u32;
            config.fonts.ui.size = base_size;
            config.fonts.code.size = base_size;
        }
    }

    let mut font_warning: Option<String> = None;
    let mut draw_ctx = match load_fonts(&config) {
        Ok(ctx) => ctx,
        Err(e) => {
            // Font loading failed (custom path or missing data dir). Try
            // resetting to the built-in defaults before giving up entirely.
            let msg = format!("Font loading failed: {e} -- falling back to defaults");
            log::warn!("{msg}");
            font_warning = Some(msg);
            config.fonts = crate::editor::config::FontsConfig::default();
            config.resolve_font_paths(datadir);
            match load_fonts(&config) {
                Ok(ctx) => ctx,
                Err(e2) => {
                    log::error!("Default font loading also failed: {e2}");
                    eprintln!("Error: could not load any fonts. {e2}");
                    return false;
                }
            }
        }
    };
    let display_scale = crate::window::get_display_scale();
    let mut style = build_style(&config, &draw_ctx);

    // Load theme colors from JSON.
    let theme_name = &config.theme;
    let theme_path = Path::new(datadir)
        .join("assets")
        .join("themes")
        .join(format!("{theme_name}.json"))
        .to_string_lossy()
        .into_owned();
    if let Ok(palette) = crate::editor::style::load_theme_palette(&theme_path) {
        apply_theme_to_style(&mut style, &palette);
    } else {
        eprintln!("Theme not found: {theme_path}, using defaults");
    }
    // Build list of available themes.
    let available_themes: Vec<String> = {
        let themes_dir = Path::new(datadir)
            .join("assets")
            .join("themes")
            .to_string_lossy()
            .into_owned();
        let mut themes = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&themes_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if let Some(stem) = name.strip_suffix(".json") {
                        themes.push(stem.to_string());
                    }
                }
            }
        }
        themes.sort();
        themes
    };
    let mut current_theme_idx = available_themes
        .iter()
        .position(|t| t == theme_name)
        .unwrap_or(0);
    style.scale = display_scale;
    style.padding_x *= display_scale;
    style.padding_y *= display_scale;
    style.divider_size = (style.divider_size * display_scale).ceil();
    style.scrollbar_size *= display_scale;
    style.caret_width = (style.caret_width * display_scale).ceil();
    style.tab_width *= display_scale;
    crate::editor::style_ctx::set_current_style(style.clone());

    // Build native keymap.
    let mut keymap = NativeKeymap::with_defaults();
    keymap.add_from_config(&config.keybindings);

    // Create the view tree: EmptyView (center) + StatusView (bottom).
    // No TitleView -- the OS window title bar is sufficient.

    let mut empty_view = EmptyView::new();
    empty_view.version = format!(
        "{} v{}",
        if subsystems.has_notes_mode() {
            "Note Anvil"
        } else if subsystems.has_sidebar() {
            "Lite Anvil"
        } else {
            "Nano Anvil"
        },
        env!("CARGO_PKG_VERSION"),
    );
    for (fmt, cmd) in EmptyView::commands() {
        if let Some(binding) = keymap.get_binding_display(cmd) {
            empty_view
                .display_commands
                .push(fmt.replace("%s", &binding));
        }
    }

    let mut status_view = StatusView::new();
    status_view.left_items.push(StatusItem {
        text: if subsystems.has_notes_mode() {
            "Note Anvil"
        } else if subsystems.has_sidebar() {
            "Lite Anvil"
        } else {
            "Nano Anvil"
        }
        .to_string(),
        color: None,
        command: None,
    });
    status_view.right_items.push(StatusItem {
        text: format!("v{}", env!("CARGO_PKG_VERSION")),
        color: None,
        command: None,
    });

    // Open files from CLI args. Per-tab state and session/file I/O live
    // in `crate::editor::open_doc`.
    use crate::editor::open_doc::{
        OpenDoc, check_file_size_limit, doc_is_modified, nag_msg_close, nag_msg_quit,
        open_file_into, project_session_key, restore_project_session, save_project_session,
        scroll_new_doc_to_line, split_path_line,
    };

    let mut docs: Vec<OpenDoc> = Vec::new();
    let mut active_tab: usize = 0;

    let line_h_for_scroll = style.code_font_height * 1.2;
    let mut has_cli_files = false;
    let mut cli_project_root: Option<String> = None;
    for arg in _args.iter().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        // If the argument is a directory, open it as the project folder.
        let p = std::path::Path::new(arg);
        if p.is_dir() {
            has_cli_files = true;
            let abs = std::path::absolute(p)
                .map(|a| a.to_string_lossy().to_string())
                .unwrap_or_else(|_| arg.to_string());
            cli_project_root = Some(abs);
            continue;
        }
        // Nano-Anvil: single file only -- skip additional args.
        if single_file_mode && has_cli_files {
            break;
        }
        has_cli_files = true;
        let (path, goto_line) = split_path_line(arg);
        // If path:N doesn't exist as-is but path does, use the split version.
        let (actual_path, line) = if goto_line.is_some()
            && !std::path::Path::new(arg).is_file()
            && std::path::Path::new(path).is_file()
        {
            (path, goto_line)
        } else {
            (arg.as_str(), None)
        };
        if open_file_into(actual_path, &mut docs, use_git()) {
            if let Some(ln) = line {
                scroll_new_doc_to_line(&mut docs, ln, line_h_for_scroll);
            }
        }
    }

    // Session restore: Lite-Anvil restores previous session; Nano-Anvil
    // always starts with a blank document.
    let mut restored_project = String::new();
    if !single_file_mode && !has_cli_files {
        if let Ok(Some(data)) = storage::load_text(userdir_path, "session", "files") {
            if let Ok(session) = serde_json::from_str::<crate::editor::open_doc::SessionData>(&data)
            {
                for (i, file) in session.files.iter().enumerate() {
                    if file == "__untitled__" {
                        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
                        if let Some(content) = session.unsaved_content.get(i) {
                            if !content.is_empty() {
                                let _ = buffer::with_buffer_mut(buf_id, |b| {
                                    b.lines = content.lines().map(|l| format!("{l}\n")).collect();
                                    if b.lines.is_empty() {
                                        b.lines.push("\n".to_string());
                                    }
                                    b.change_id += 1;
                                    Ok(())
                                });
                            }
                        }
                        let mut dv = DocView::new();
                        dv.buffer_id = Some(buf_id);
                        docs.push(OpenDoc {
                            view: dv,
                            path: String::new(),
                            name: "untitled".to_string(),
                            saved_change_id: 1,
                            saved_signature: buffer::content_signature(&["\n".to_string()]),
                            indent_type: "soft".to_string(),
                            indent_size: 2,
                            git_changes: std::collections::HashMap::new(),
                            cached_render: std::sync::Arc::new(Vec::new()),
                            cached_change_id: -1,
                            cached_scroll_y: -1.0,
                            cached_hint_count: 0,
                            cached_rect_w: -1.0,
                            cached_rect_h: -1.0,
                            dirty_cache: std::cell::Cell::new(None),
                            canonical_cache: Default::default(),
                            token_cache: std::cell::RefCell::new(
                                crate::editor::open_doc::TokenCache::default(),
                            ),
                            preview: crate::editor::markdown_preview::MarkdownPreviewState::default(
                            ),
                        });
                    } else {
                        open_file_into(file, &mut docs, use_git());
                    }
                }
                if session.active < docs.len() {
                    active_tab = session.active;
                }
                restored_project = session.active_project;
            }
        }
    }

    // Nano-Anvil: always ensure exactly one document (blank if no CLI file).
    if single_file_mode && docs.is_empty() {
        let buf_state = buffer::default_buffer_state();
        let initial_change_id = buf_state.change_id;
        let buf_id = buffer::insert_buffer(buf_state);
        let mut dv = DocView::new();
        dv.buffer_id = Some(buf_id);
        docs.push(OpenDoc {
            view: dv,
            path: String::new(),
            name: "untitled".to_string(),
            saved_change_id: initial_change_id,
            saved_signature: 0,
            indent_type: "soft".to_string(),
            indent_size: 4,
            git_changes: std::collections::HashMap::new(),
            cached_render: std::sync::Arc::new(Vec::new()),
            cached_change_id: -1,
            cached_scroll_y: -1.0,
            cached_hint_count: 0,
            cached_rect_w: -1.0,
            cached_rect_h: -1.0,
            dirty_cache: std::cell::Cell::new(None),
            token_cache: std::cell::RefCell::new(crate::editor::open_doc::TokenCache::default()),
            preview: crate::editor::markdown_preview::MarkdownPreviewState::default(),
            canonical_cache: Default::default(),
        });
    }

    // Sidebar state.
    // Load saved sidebar width.
    let mut sidebar_width: f64 =
        crate::editor::storage::load_text(userdir_path, "session", "sidebar_width")
            .ok()
            .flatten()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(DEFAULT_SIDEBAR_W);
    let mut sidebar_dragging = false;
    // Editor|markdown-preview split fraction, persisted per app. Loaded the
    // same way the sidebar width is, then clamped so neither pane collapses.
    let mut preview_split: f64 =
        crate::editor::storage::load_text(userdir_path, "session", "preview_split")
            .ok()
            .flatten()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(DEFAULT_PREVIEW_SPLIT)
            .clamp(MIN_PREVIEW_SPLIT, MAX_PREVIEW_SPLIT);
    let mut preview_dragging = false;
    // Held while the mouse is pressed on the editor's vertical
    // scrollbar; lets drag-scrolling track the cursor until release.
    // The drag offset is the pixel gap between the top of the thumb and
    // the click y at the moment the press began, so the thumb stays
    // anchored to the grip point as the mouse moves.
    let mut editor_sb_dragging = false;
    let mut editor_sb_drag_offset: f64 = 0.0;
    let mut terminal_sb_dragging = false;
    let mut terminal_sb_drag_offset: f64 = 0.0;
    let mut sidebar_sb_dragging = false;
    let mut sidebar_sb_drag_offset: f64 = 0.0;
    // Markdown preview scrollbar drags, one flag per axis, with the same
    // grip-anchored offset the editor scrollbar uses.
    let mut preview_sb_v_dragging = false;
    let mut preview_sb_v_drag_offset: f64 = 0.0;
    let mut preview_sb_h_dragging = false;
    let mut preview_sb_h_drag_offset: f64 = 0.0;
    let mut editor_mouse_down = false;
    // Last (buffer, selection range) mirrored into the X11 PRIMARY selection.
    // Keyed so a non-empty selection is pushed once per change rather than
    // every frame, and a re-selection after a foreign app grabbed PRIMARY
    // re-asserts ownership (the intervening caret changes the key).
    let mut last_primary_key: (u64, Vec<usize>) = (0, Vec::new());
    // Local shift-key tracker. SDL's mouse events don't carry modifier state,
    // so tracking it from keyboard events directly by key name makes shift+click
    // robust against any SDL_GetModState quirks on different platforms/WMs.
    let mut shift_held = false;
    let mut tab_dragging: Option<usize> = None;
    // Dropdown menu shown when the tab bar overflows — lists every open tab
    // so they stay reachable even when their labels don't fit on screen.
    let mut tab_dropdown_open: bool = false;
    let mut tab_layout = TabLayout::default();
    /// How often the opt-in memory report is emitted.
    const MEMORY_REPORT_INTERVAL_SECS: u64 = 10;
    let mut last_memory_report = Instant::now();
    // Suppresses the hover tooltip after a tab click until the mouse leaves
    // the tab bar; prevents the tooltip from lingering on the selected tab.
    let mut tab_tooltip_suppressed: bool = false;
    // Tab targeted by the most recent right-click; consumed by the
    // tab:close / close-left / close-right / close-all dispatch in the
    // context-menu click handler.
    let mut tab_menu_target: Option<usize> = None;
    let mut mouse_x: f64 = 0.0;
    let mut mouse_y: f64 = 0.0;
    let mut sidebar_entries: Vec<SidebarEntry>;
    let mut sidebar_watcher = SidebarWatcher::new();
    let mut sidebar_scroll: f64 = 0.0;
    // Content height + scrollbar track geometry captured during the sidebar
    // draw so the click/drag paths can reuse the same numbers instead of
    // recomputing the filtered notes-mode entry list.
    let mut sidebar_content_h: f64 = 0.0;
    let mut sidebar_sb_top: f64 = 0.0;
    let mut sidebar_sb_h: f64 = 0.0;

    // Determine project root for sidebar.
    // Notes-mode forces the configured notes folder so the sidebar
    // always reflects the user's notes dir even after the user changes
    // NOTE_ANVIL_DIR. Otherwise CLI folder overrides everything, then
    // restored project, then nothing. If a file was passed via CLI (no
    // folder), don't open a project.
    let mut project_root: String = if let Some(folder) = subsystems.notes_folder() {
        folder.to_string()
    } else if let Some(root) = cli_project_root {
        root
    } else if has_cli_files {
        // Files passed on CLI -- no project folder.
        String::new()
    } else if !restored_project.is_empty() && Path::new(&restored_project).is_dir() {
        restored_project
    } else {
        String::new()
    };

    let mut sidebar_show_hidden = false;
    // Set while the window is occluded or hidden. We skip the render
    // pass entirely while true so Metal/IOSurface/RenCache buffers
    // don't get touched for frames nobody will see. Reset on Exposed
    // / Shown / FocusGained.
    let mut window_hidden: bool = false;
    // Idle-drop: if the user hasn't interacted for a while, release the
    // glyph / render caches. They'll be rebuilt on the next draw.
    let mut last_activity: Instant = Instant::now();
    let mut dropped_caches_for_idle: bool = false;
    const IDLE_DROP_SECS: u64 = 60;
    // macOS memory-pressure poll: check every N seconds. If the kernel
    // reports WARN or CRITICAL, drop caches immediately.
    let mut last_mem_pressure_check: Instant = Instant::now();
    const MEM_PRESSURE_CHECK_SECS: u64 = 5;
    // Notes-mode sidebar: search filter + sort. NoteSquirrel parity.
    // sort_mode: 0 = A-Z asc, 1 = A-Z desc, 2 = Recent (newest first),
    //            3 = Recent (oldest first).
    let mut notes_search: String = String::new();
    let mut notes_search_focused: bool = false;
    let mut notes_sort_mode: u8 =
        crate::editor::storage::load_text(userdir_path, "session", "notes_sort_mode")
            .ok()
            .flatten()
            .and_then(|s| s.trim().parse::<u8>().ok())
            .filter(|v| *v <= 3)
            .unwrap_or(0);
    let file_icons = load_file_icons(datadir);
    sidebar_entries = if subsystems.has_sidebar() && !project_root.is_empty() {
        scan_for_sidebar(
            subsystems.has_notes_mode(),
            &project_root,
            sidebar_show_hidden,
        )
    } else {
        Vec::new()
    };
    let mut sidebar_visible = subsystems.has_sidebar() && !project_root.is_empty();
    if subsystems.has_sidebar() {
        restore_expanded_folders(
            &mut sidebar_entries,
            userdir_path,
            sidebar_show_hidden,
            &project_session_key(&project_root),
        );
        if !project_root.is_empty() {
            sidebar_watcher.watch_dir(&project_root);
            for entry in &sidebar_entries {
                if entry.is_dir && entry.expanded {
                    sidebar_watcher.watch_dir(&entry.path);
                }
            }
        }
    }

    // Recent projects list (persisted).
    let mut recent_projects: Vec<String> =
        crate::editor::storage::load_text(userdir_path, "session", "recent_projects")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
    // Add current project to recents.
    {
        let abs = std::fs::canonicalize(&project_root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| project_root.clone());
        if !abs.is_empty() && !recent_projects.contains(&abs) {
            recent_projects.insert(0, abs);
            if recent_projects.len() > 20 {
                recent_projects.truncate(20);
            }
        }
    }

    // Recent files list (persisted, max 100).
    let mut recent_files: Vec<String> =
        crate::editor::storage::load_text(userdir_path, "session", "recent_files")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

    let fps = config.fps as f64;
    let mut redraw = true;
    let mut quit = false;
    let mut window_title = String::new();
    // Cached (path, language-name) for the status bar so the syntax-entry glob
    // scan is not repeated on every redraw while the same file stays active.
    let mut status_lang_cache: (String, String) = (String::new(), String::new());
    let frame_interval = 1.0 / fps;
    // When the UI is idle - nothing drawn recently, no terminal panel open, no
    // background job running - the loop blocks this long between timer-driven
    // wakeups instead of spinning at the frame interval. Input and pushed
    // wake-up events still return from the blocked wait immediately, so this
    // lowers idle CPU/battery without affecting responsiveness.
    let idle_wait = 0.5_f64;
    let mut last_draw = Instant::now();
    // Deferred render-line cache: written at the top of the next frame to
    // avoid borrow-checker conflicts with the immutable doc borrow during
    // rendering. Includes the tab index so we write to the correct doc even
    // if the user switched tabs between frames.
    // Tuple layout: (tab_idx, buffer_id, lines, change_id, scroll_y). The
    // `buffer_id` is the only stable identity for the doc being rendered —
    // `tab_idx` aliases once the docs list is swapped (e.g. Open Recent
    // replaces the project), so the consumer uses `buffer_id` to skip
    // applying a render captured against a now-defunct doc.
    // The trailing `usize` is the number of LSP inlay hints actually folded
    // into the render. Recording the count used (after URI filtering) rather
    // than the global `lsp_state.inlay_hints.len()` keeps the cache key honest
    // when the active doc's URI doesn't match the held hints.
    // The last two `f64`s are the view width and height so the cache is
    // invalidated when the window is resized.
    type PendingRenderCache = Option<(
        usize,
        u64,
        std::sync::Arc<Vec<RenderLine>>,
        i64,
        f64,
        usize,
        f64,
        f64,
    )>;
    let mut pending_render_cache: PendingRenderCache = None;

    // Background file load job. When a large file is being loaded on a
    // background thread, this holds the progress atomics and the join handle.
    struct LoadJob {
        path: String,
        name: String,
        bytes_read: std::sync::Arc<std::sync::atomic::AtomicU64>,
        total_bytes: u64,
        handle: Option<std::thread::JoinHandle<Result<buffer::BufferState, String>>>,
    }
    let mut load_job: Option<LoadJob> = None;

    /// Spawn a background file load. Returns a LoadJob to poll each frame.
    fn spawn_load(path: &str, total: u64) -> LoadJob {
        use std::sync::atomic::{AtomicU64, Ordering};
        let bytes_read = std::sync::Arc::new(AtomicU64::new(0));
        let bytes_read_clone = bytes_read.clone();
        let path_owned = path.to_string();
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
        let handle = std::thread::spawn(move || {
            let mut state = buffer::default_buffer_state();
            buffer::load_file_with_progress(&mut state, &path_owned, |bytes, _total| {
                bytes_read_clone.store(bytes, Ordering::Relaxed);
            })
            .map_err(|e| e.to_string())?;
            Ok(state)
        });
        LoadJob {
            path: path.to_string(),
            name,
            bytes_read,
            total_bytes: total,
            handle: Some(handle),
        }
    }

    // Find bar state.
    let mut find_active = false;
    let mut find_query = String::new();
    let mut replace_active = false;
    let mut replace_query = String::new();
    let mut find_focus_on_replace = false;
    let mut find_use_regex = false;
    let mut find_whole_word = false;
    let mut find_case_insensitive = false;
    // All current matches as (line, col, end_col) with 1-based columns.
    let mut find_matches: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut find_current: Option<usize> = None;
    // Anchor (line, col) captured when find is opened — live-search re-centers here
    // so typing a longer query doesn't skip past matches the user hasn't seen yet.
    let mut find_anchor: (usize, usize) = (1, 1);
    // Find-in-selection: when true, matches are limited to the captured range.
    let mut find_in_selection = false;
    // The selection range captured when find-in-selection was activated:
    // (start_line, start_col, end_line, end_col), all 1-based.
    let mut find_selection_range: Option<(usize, usize, usize, usize)> = None;

    // Nag bar state. The three prompts the editor can raise —
    // "unsaved changes on close/quit?", "file changed on disk, reload?",
    // and "parent directory missing, create and save?" — are modelled as
    // a single enum instead of three independent `bool + data` sets so
    // only one nag can be active at a time and the draw code can match
    // on it once.
    #[derive(Default)]
    enum Nag {
        #[default]
        None,
        /// "FOO has unsaved changes, close/quit anyway?" — `tab_to_close`
        /// is `Some(i)` to close that tab on Yes, `None` to quit.
        UnsavedChanges {
            message: String,
            tab_to_close: Option<usize>,
        },
        /// "File changed on disk, reload?" — applies to the doc at `path`.
        ReloadFromDisk { path: String },
        /// "Directory does not exist: PARENT. Create it and save?" — on
        /// Yes, mkdir -p the parent and save to `save_path`; other fields
        /// are needed to complete the interrupted Save / Save As.
        CreateDir {
            parent: String,
            save_path: String,
            doc_tab: usize,
            from_save_as: bool,
        },
        /// "FILE already exists, overwrite?" — Save As targeted an existing
        /// file that isn't the current doc's own path. Yes performs the
        /// save; No returns to the Save As picker so the user can pick a
        /// different name. Guards against autocomplete races where a
        /// late-arriving suggestion silently retargets Enter.
        OverwriteFile { save_path: String, doc_tab: usize },
        /// "No extension detected, save anyway?" — Save As target has no
        /// trailing `.ext`. Yes proceeds (and still checks for overwrite
        /// next); No returns to the picker so the user can add one.
        NoExtension { save_path: String, doc_tab: usize },
        /// "Delete FILE?" — sidebar Delete confirmation. Yes removes the
        /// file from disk and closes any open tab pointing to it; No
        /// dismisses without touching anything.
        DeleteFile { path: String },
    }
    impl Nag {
        fn is_unsaved(&self) -> bool {
            matches!(self, Nag::UnsavedChanges { .. })
        }
    }
    let mut nag = Nag::None;
    // Set by the KeyDown-side nag handlers so the immediately-following
    // SDL_TEXTINPUT event (which fires on every printable keystroke,
    // including Y / N) doesn't leak into the active document.
    let mut eat_next_text_input: bool = false;
    let mut info_message: Option<(String, Instant)> = font_warning.map(|msg| (msg, Instant::now()));

    // Command palette state.
    let mut palette_active = false;
    let mut palette_query = String::new();
    let mut palette_results: Vec<(String, String)> = Vec::new(); // (cmd_name, display_name)
    let mut palette_selected: usize = 0;

    // Build command list for palette from keymap.
    let all_commands: Vec<(String, String)> = {
        let mut cmds = Vec::new();
        // Extract unique command names from keymap bindings, skipping the
        // raw key-input commands that aren't meaningful in the palette.
        let mut seen = std::collections::HashSet::new();
        for (stroke, cmd_names) in keymap.iter_bindings() {
            for cmd in cmd_names {
                if !crate::editor::keymap::is_palette_command(cmd) {
                    continue;
                }
                if seen.insert(cmd.clone()) {
                    let display = crate::editor::keymap::prettify_name(cmd);
                    cmds.push((cmd.clone(), format!("{display}  ({stroke})")));
                }
            }
        }
        // Commands available in the palette without a keybinding.
        let palette_extras: &[&str] = &[
            "core:sort-lines",
            "doc:reopen-closed-tab",
            "doc:convert-indentation",
            "doc:toggle-line-endings",
            "core:open-user-settings",
            "about:version",
            "core:force-quit",
            "core:toggle-hidden-files",
            "core:check-for-updates",
            "doc:upper-case",
            "doc:lower-case",
            "doc:reload",
            "git:pull",
            "git:push",
            "git:commit",
            "git:stash",
            "git:blame",
            "git:log",
            "root:close-all",
            "root:close-all-others",
            "root:close-or-quit",
            "doc:save-as",
            "core:toggle-markdown-preview",
            "notes:delete-current",
            "test:run-all",
            "test:run-in-current-file",
        ];
        for cmd in palette_extras {
            if seen.insert((*cmd).to_string()) {
                let display = crate::editor::keymap::prettify_name(cmd);
                cmds.push(((*cmd).to_string(), display));
            }
        }
        cmds.sort_by(|a, b| a.1.cmp(&b.1));
        // Filter commands for disabled subsystems.
        if !subsystems.has_git() {
            cmds.retain(|c| !c.0.starts_with("git:") && c.0 != "core:git-status");
        }
        if !subsystems.has_lsp() {
            cmds.retain(|c| !c.0.starts_with("lsp:"));
        }
        if !subsystems.has_terminal() {
            cmds.retain(|c| !c.0.contains("terminal"));
        }
        if !subsystems.has_sidebar() {
            cmds.retain(|c| !c.0.contains("sidebar") && c.0 != "core:toggle-hidden-files");
        }
        if !subsystems.has_find_in_files() {
            cmds.retain(|c| c.0 != "core:project-search" && c.0 != "core:project-replace");
        }
        if !subsystems.has_update_check() {
            cmds.retain(|c| c.0 != "core:check-for-updates");
        }
        if !subsystems.has_picker() {
            // Nano-Anvil (single_file_mode) still supports core:open-recent
            // as a files-only list, so only strip open-project-folder.
            let keep_recent = single_file_mode;
            cmds.retain(|c| {
                c.0 != "core:open-project-folder" && (keep_recent || c.0 != "core:open-recent")
            });
        }
        if !subsystems.has_bookmarks() {
            cmds.retain(|c| !c.0.contains("bookmark"));
        }
        if !subsystems.has_folding() {
            cmds.retain(|c| c.0 != "doc:fold" && c.0 != "doc:unfold" && c.0 != "doc:unfold-all");
        }
        if single_file_mode {
            cmds.retain(|c| {
                !c.0.contains("tab") && c.0 != "root:close-all" && c.0 != "root:close-all-others"
            });
        }
        // Notes-mode: drop project / folder / multi-tab / preview-toggle
        // commands. Keep only what NoteSquirrel users would expect.
        if subsystems.has_notes_mode() {
            cmds.retain(|c| {
                let n = c.0.as_str();
                !n.contains("tab")
                    && !n.contains("project")
                    && !n.contains("folder")
                    && n != "core:toggle-markdown-preview"
                    && n != "core:toggle-hidden-files"
                    && n != "doc:save"
                    && n != "doc:save-as"
                    && n != "doc:reload"
                    && n != "core:open-file"
                    && n != "core:find-file"
                    && n != "root:close-all"
                    && n != "root:close-all-others"
            });
        } else {
            // Outside notes-mode the delete-current command is a no-op
            // and would only confuse the palette.
            cmds.retain(|c| c.0 != "notes:delete-current");
        }
        cmds
    };

    // Command view state. Helpers and the `CmdViewMode` enum live in
    // `crate::editor::cmdview`.
    #[cfg(feature = "sdl")]
    use crate::editor::cmdview::truncate_left_to_width;
    use crate::editor::cmdview::{
        CmdViewMode, dir_with_trailing_sep, effective_root, path_suggest,
        refresh_cmdview_suggestions, remember_recent_file, update_recent,
    };
    let mut cmdview_active = false;
    let mut cmdview_mode = CmdViewMode::OpenFile;
    let mut cmdview_text = String::new();
    // Byte position of the input caret within cmdview_text. Always lands on a UTF-8 boundary.
    let mut cmdview_cursor: usize = 0;
    let mut cmdview_suggestions: Vec<String> = Vec::new();
    let mut cmdview_selected: usize = 0;
    let mut cmdview_label = String::new();
    // Pending LSP rename target: (uri, 0-based line, 0-based character). Set
    // when the rename input opens; consumed when the new name is submitted.
    let mut lsp_rename_pos: Option<(String, usize, usize)> = None;
    // Code-action picker state: overlay of (title, action-json) awaiting choice.
    let mut code_action_active = false;
    let mut code_actions: Vec<(String, serde_json::Value)> = Vec::new();
    let mut code_action_selected: usize = 0;

    // Per-field undo/redo history for the dialog text inputs. Each input keeps
    // its own stack so the undo/redo shortcuts edit the focused field rather
    // than the document buffer (VS Code input-box behaviour).
    use crate::editor::field_history::{FieldEdit, FieldHistory};
    let mut find_history = FieldHistory::default();
    let mut replace_history = FieldHistory::default();
    let mut cmdview_history = FieldHistory::default();
    let mut project_search_history = FieldHistory::default();
    let mut project_replace_search_history = FieldHistory::default();
    let mut project_replace_with_history = FieldHistory::default();
    let mut palette_history = FieldHistory::default();
    let mut notes_search_history = FieldHistory::default();
    let mut sidebar_new_file_history = FieldHistory::default();

    // Project-wide search state.
    // Git status view.
    let mut git_status_active = false;
    let mut git_status_entries: Vec<(String, String, String)> = Vec::new();
    let mut git_status_selected: usize = 0;
    // Background git-status refresh job, polled each frame.
    let mut git_status_job: Option<std::thread::JoinHandle<Vec<(String, String, String)>>> = None;
    let mut git_blame_job: Option<std::thread::JoinHandle<Vec<String>>> = None;
    let mut git_log_job: Option<std::thread::JoinHandle<Vec<(String, String, String)>>> = None;
    let mut update_check_job: Option<std::thread::JoinHandle<String>> = None;
    // Paths of recently closed tabs (most-recent last) for reopen-closed-tab.
    let mut closed_tabs: Vec<String> = Vec::new();

    // Git blame: per-line annotations shown inline at the right edge.
    let mut git_blame_active = false;
    let mut git_blame_lines: Vec<String> = Vec::new();

    // Git history (log) for the current file.
    let mut git_log_active = false;
    let mut git_log_entries: Vec<(String, String, String)> = Vec::new(); // (hash, date, message)
    let mut git_log_selected: usize = 0;

    fn run_git_status(root: &str) -> Vec<(String, String, String)> {
        let Ok(output) = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["status", "--porcelain=v1"])
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                if line.len() < 4 {
                    return None;
                }
                let code = line[..2].trim().to_string();
                let path = line[3..].trim().to_string();
                let display = format!("[{code}] {path}");
                Some((code, path, display))
            })
            .collect()
    }

    /// Run `git blame --porcelain` and return one summary string per line.
    fn run_git_blame(file_path: &str) -> Vec<String> {
        let Ok(output) = std::process::Command::new("git")
            .args(["blame", "--porcelain", "--", file_path])
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        // Porcelain format: blocks of header lines followed by a tab-prefixed
        // source line. Each block starts with a 40-char hash. We collect
        // author + author-time for each block, then build a compact summary.
        let text = String::from_utf8_lossy(&output.stdout);
        let mut result: Vec<String> = Vec::new();
        let mut hash = String::new();
        let mut author = String::new();
        let mut date = String::new();
        for line in text.lines() {
            // Block header: 40-char hash followed by line numbers.
            if line.len() >= 40 && line.chars().take(40).all(|c| c.is_ascii_hexdigit()) {
                hash = line[..8].to_string();
            } else if let Some(a) = line.strip_prefix("author ") {
                author = a.to_string();
            } else if let Some(ts) = line.strip_prefix("author-time ") {
                if let Ok(epoch) = ts.parse::<i64>() {
                    let days = epoch / 86400;
                    let (y, m, d) = epoch_to_ymd(days);
                    date = format!("{y:04}-{m:02}-{d:02}");
                }
            } else if line.starts_with('\t') {
                // End of block — emit the summary for this source line.
                let short_author: String = author.chars().take(20).collect();
                result.push(format!("{hash}  {short_author:<20}  {date}"));
                author.clear();
                date.clear();
                hash.clear();
            }
        }
        result
    }

    /// Trivial days-since-epoch to (year, month, day) for blame dates.
    fn epoch_to_ymd(days_since_epoch: i64) -> (i64, i64, i64) {
        // Algorithm from Howard Hinnant's civil_from_days (public domain).
        let z = days_since_epoch + 719468;
        let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        (y, m, d)
    }

    /// Run `git log --oneline` for a file and return (hash, date, message).
    fn run_git_log(file_path: &str) -> Vec<(String, String, String)> {
        let Ok(output) = std::process::Command::new("git")
            .args(["log", "--format=%h|%as|%s", "-50", "--", file_path])
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(3, '|');
                let hash = parts.next()?.to_string();
                let date = parts.next()?.to_string();
                let msg = parts.next().unwrap_or("").to_string();
                Some((hash, date, msg))
            })
            .collect()
    }

    let mut project_search_active = false;
    let mut project_search_query = String::new();
    let mut project_search_results: Vec<(String, usize, String)> = Vec::new();
    let mut project_search_selected: usize = 0;
    // Shared toggles for both project search and project replace.
    let mut project_use_regex = false;
    let mut project_whole_word = false;
    let mut project_case_insensitive = true;

    // Project-wide replace state.
    let mut project_replace_active = false;
    let mut project_replace_search = String::new();
    let mut project_replace_with = String::new();
    let mut project_replace_focus_on_replace = false;
    let mut project_replace_results: Vec<(String, usize, String)> = Vec::new();
    // Background project-wide replace-all (sed) job, polled each frame.
    let mut replace_job: Option<std::thread::JoinHandle<usize>> = None;
    let mut project_replace_selected: usize = 0;

    /// Run grep across the project, returning (path, line_number, line_text) tuples.
    /// Blocking project-wide grep. Runs on a worker thread spawned by
    /// `run_project_search`; do not call directly from the render loop.
    fn project_search_blocking(
        query: &str,
        root: &str,
        use_regex: bool,
        whole_word: bool,
        case_insensitive: bool,
    ) -> Vec<(String, usize, String)> {
        let mut args = vec!["-rn".to_string()];
        if case_insensitive {
            args.push("-i".to_string());
        }
        if !use_regex {
            args.push("-F".to_string()); // fixed string (literal)
        }
        if whole_word {
            args.push("-w".to_string());
        }
        for pat in &[
            "--include=*.rs",
            "--include=*.toml",
            "--include=*.json",
            "--include=*.md",
            "--include=*.txt",
            "--include=*.js",
            "--include=*.ts",
            "--include=*.py",
            "--include=*.go",
            "--include=*.c",
            "--include=*.h",
            "--include=*.cpp",
            "--include=*.java",
        ] {
            args.push(pat.to_string());
        }
        args.push(query.to_string());
        args.push(root.to_string());
        let output = std::process::Command::new("grep").args(&args).output();
        let Ok(out) = output else {
            return Vec::new();
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut results = Vec::new();
        for line in stdout.lines().take(100) {
            // Format: path:line_num:text
            let mut parts = line.splitn(3, ':');
            let Some(path) = parts.next() else { continue };
            let Some(num_str) = parts.next() else {
                continue;
            };
            let Some(text) = parts.next() else { continue };
            let Ok(line_num) = num_str.parse::<usize>() else {
                continue;
            };
            results.push((path.to_string(), line_num, text.trim().to_string()));
        }
        results
    }

    /// Non-blocking project-wide search. Returns the most recently completed
    /// results immediately and runs grep on a worker thread, so typing in the
    /// find box never blocks the render loop; the per-frame pump applies fresh
    /// results for the active panel when they land.
    fn run_project_search(
        query: &str,
        root: &str,
        use_regex: bool,
        whole_word: bool,
        case_insensitive: bool,
    ) -> Vec<(String, usize, String)> {
        type Hit = (String, usize, String);
        type Key = (String, String, bool, bool, bool);
        struct SearchCache {
            ready: std::collections::HashMap<Key, Vec<Hit>>,
            order: std::collections::VecDeque<Key>,
            inflight: std::collections::HashSet<Key>,
            most_recent: Vec<Hit>,
        }
        if query.len() < 2 {
            return Vec::new();
        }
        static CACHE: std::sync::LazyLock<parking_lot::Mutex<SearchCache>> =
            std::sync::LazyLock::new(|| {
                parking_lot::Mutex::new(SearchCache {
                    ready: std::collections::HashMap::new(),
                    order: std::collections::VecDeque::new(),
                    inflight: std::collections::HashSet::new(),
                    most_recent: Vec::new(),
                })
            });
        let key: Key = (
            query.to_string(),
            root.to_string(),
            use_regex,
            whole_word,
            case_insensitive,
        );
        let mut cache = CACHE.lock();
        if let Some(v) = cache.ready.get(&key) {
            return v.clone();
        }
        // First request for this query: kick a background grep. The closure
        // publishes its result only if the query is still wanted, and the
        // cache is bounded so long sessions of typing cannot grow it.
        if cache.inflight.insert(key.clone()) {
            let job_key = key.clone();
            std::thread::spawn(move || {
                let val = project_search_blocking(
                    &job_key.0, &job_key.1, job_key.2, job_key.3, job_key.4,
                );
                let mut cache = CACHE.lock();
                cache.inflight.remove(&job_key);
                cache.most_recent = val.clone();
                cache.ready.insert(job_key.clone(), val);
                cache.order.push_back(job_key);
                while cache.order.len() > 64 {
                    if let Some(old) = cache.order.pop_front() {
                        cache.ready.remove(&old);
                    }
                }
            });
        }
        cache.most_recent.clone()
    }

    /// Execute project-wide find-and-replace using sed. Returns the number of
    /// files modified. In literal mode the search is escaped so every character
    /// matches exactly, and case is folded only when `case_insensitive` is set,
    /// so a replace matches exactly the text the user typed in the case they
    /// typed it.
    fn execute_project_replace(
        root: &str,
        search: &str,
        replace: &str,
        use_regex: bool,
        case_insensitive: bool,
    ) -> usize {
        if search.is_empty() {
            return 0;
        }
        // Find matching files first. `-F` keeps grep's selection literal in
        // non-regex mode so it agrees with the literal sed expression below.
        let mut grep_args: Vec<&str> = vec!["-rl"];
        if case_insensitive {
            grep_args.push("-i");
        }
        if !use_regex {
            grep_args.push("-F");
        }
        for inc in [
            "--include=*.rs",
            "--include=*.toml",
            "--include=*.json",
            "--include=*.md",
            "--include=*.txt",
            "--include=*.js",
            "--include=*.ts",
            "--include=*.py",
            "--include=*.go",
            "--include=*.c",
            "--include=*.h",
            "--include=*.cpp",
            "--include=*.java",
        ] {
            grep_args.push(inc);
        }
        grep_args.push(search);
        grep_args.push(root);
        let grep_out = std::process::Command::new("grep").args(&grep_args).output();
        let Ok(out) = grep_out else { return 0 };
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let files: Vec<&str> = stdout.lines().collect();
        if files.is_empty() {
            return 0;
        }
        // sed matches with BRE. In literal mode escape the BRE metacharacters
        // and the `/` delimiter so the search text is matched verbatim; in regex
        // mode only the delimiter is escaped so the user's pattern is preserved.
        let sed_search = if use_regex {
            search.replace('/', "\\/")
        } else {
            let mut escaped = String::with_capacity(search.len() + 8);
            for c in search.chars() {
                if matches!(c, '\\' | '.' | '*' | '[' | ']' | '^' | '$' | '/') {
                    escaped.push('\\');
                }
                escaped.push(c);
            }
            escaped
        };
        // In the replacement text only `\`, `&`, and the delimiter are special.
        let sed_replace = replace
            .replace('\\', "\\\\")
            .replace('/', "\\/")
            .replace('&', "\\&")
            .replace('\n', "\\n");
        let flags = if case_insensitive { "gi" } else { "g" };
        let sed_expr = format!("s/{sed_search}/{sed_replace}/{flags}");
        let mut count = 0usize;
        for file in &files {
            let file = file.trim();
            if file.is_empty() {
                continue;
            }
            let ok = if cfg!(target_os = "macos") {
                std::process::Command::new("sed")
                    .args(["-i", "", "-e", &sed_expr, file])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            } else {
                std::process::Command::new("sed")
                    .args(["-i", "-e", &sed_expr, file])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            };
            if ok {
                count += 1;
            }
        }
        count
    }

    // Context menu state.
    let mut context_menu = ContextMenu::new();
    // (doc_path, test_name) to run; set by the badge-click hit-test and
    // consumed by the `test:run-single` command dispatch.
    let mut pending_single_test: Option<(String, String)> = None;
    // Per-frame discovered tests for the active doc; rebuilt each frame.
    let mut active_tests: Vec<crate::editor::test_runner::DiscoveredTest> = Vec::new();
    // Per-frame badge rects for the active doc.
    let mut test_badges: Vec<crate::editor::test_runner::TestBadgeRegion> = Vec::new();
    // (path, change_id) the badges were last scanned for, so the discovery scan
    // (which clones the document and probes the filesystem) runs only when the
    // file or its content changes, not on every redraw.
    let mut test_scan_cache: (String, i64) = (String::new(), -1);
    // Sidebar entry targeted by the current context menu (path, is_dir).
    // Set when right-clicking a sidebar row; consumed by the rename flow.
    let mut sidebar_menu_target: Option<(String, bool)> = None;
    // Path being renamed (source). Read by the CmdViewMode::Rename
    // confirm handler to `fs::rename` the file.
    let mut rename_source: String = String::new();
    // Folder path for the in-progress inline new-file creation (`None` = inactive).
    let mut sidebar_new_file_dir: Option<String> = None;
    // Filename currently being typed into the inline new-file input.
    let mut sidebar_new_file_name: String = String::new();
    // Byte-offset cursor position within `sidebar_new_file_name`.
    let mut sidebar_new_file_cursor: usize = 0;

    // LSP completion, hover, and go-to-definition state.
    let mut completion = CompletionState::new();
    let mut hover = HoverState::new();
    // Diagnostic hover identity and hit regions for its action row.
    let mut hovered_diagnostic: Option<(String, usize, usize, usize, usize)> = None;
    let mut diagnostic_hover_region_rect: Option<crate::editor::types::Rect> = None;
    let mut diagnostic_hover_leave_at: Option<Instant> = None;
    let mut diagnostic_quick_fix_rect: Option<crate::editor::types::Rect> = None;
    let mut diagnostic_hover_action_request: Option<i64> = None;
    let mut diagnostic_hover_actions: Vec<(String, serde_json::Value)> = Vec::new();
    // Signature-help popup reuses the hover popup shape (text + visibility).
    let mut signature_help = HoverState::new();
    // Mouse-tracked hover state: `mouse_doc_pos` is the (1-based line,
    // 1-based col) under the cursor when over the active doc, or None
    // otherwise. `mouse_idle_since` records when the cursor settled at
    // that position so we can debounce the `textDocument/hover` LSP
    // request — diagnostic tooltips fire immediately, type-info tooltips
    // wait for the cursor to stop moving for ~600ms. `last_lsp_hover_pos`
    // dedupes repeat requests for the same position.
    let mut mouse_doc_pos: Option<(usize, usize)> = None;
    let mut mouse_idle_since: Option<Instant> = None;
    let mut last_lsp_hover_pos: Option<(usize, usize)> = None;
    let mut pending_lsp_hover_request: Option<i64> = None;

    // Terminal emulator panel (multi-terminal).
    let mut terminal = TerminalPanel::new();

    // Minimap state.
    let mut minimap_visible = false;
    // Line wrap is on by default, and the preference is persisted across
    // sessions so a user who explicitly disables it still sees no wrap the
    // next time they launch.
    let mut line_wrapping =
        match crate::editor::storage::load_text(userdir_path, "session", "line_wrapping") {
            Ok(Some(v)) => v.trim() != "false",
            _ => true,
        };
    let mut overwrite_mode = false;
    let mut cursor_blink_reset = Instant::now();
    let blink_period = 0.5;

    // Autoreload: watch open files for external changes.
    let mut autoreload = AutoreloadState::new();

    // Notes-mode: restore the per-notes-folder session (the previously
    // open note) when no doc was opened from the CLI. Mirrors NoteSquirrel's
    // "remember last open note" behavior so launching note-anvil drops
    // the user back into whatever they were editing last.
    if subsystems.has_notes_mode() && docs.is_empty() && !project_root.is_empty() {
        if let Some(tab) = crate::editor::open_doc::restore_project_session(
            userdir_path,
            &project_root,
            &mut docs,
            &mut autoreload,
            use_git(),
        ) {
            active_tab = tab;
        }
    }

    for doc in &docs {
        autoreload.watch(&doc.path);
    }

    // Syntax highlighting: load lightweight index for file matching, defer
    // full definition parsing to first use per extension.
    let syntax_index = crate::editor::syntax::load_syntax_index(datadir);
    let mut compiled_syntax_cache: HashMap<String, Option<CompiledSyntax>> = HashMap::new();
    // MRU ordering for `compiled_syntax_cache`: `compiled_syntax_mru[0]`
    // is the most recently used extension. Lets us cap the cache at
    // `SYNTAX_CACHE_CAP` entries and evict the oldest instead of
    // growing unbounded on sessions that touch many file types.
    let mut compiled_syntax_mru: Vec<String> = Vec::new();
    const SYNTAX_CACHE_CAP: usize = 8;

    // LSP state.
    let mut lsp_state = LspState::new();
    let mut inactive_lsp_states: HashMap<(String, String), LspState> = HashMap::new();
    let lsp_specs = if subsystems.has_lsp() {
        lsp::load_specs(userdir_path, &project_root)
    } else {
        Vec::new()
    };

    /// Try to start LSP for a file path if not already running for this filetype.
    fn try_start_lsp(
        file_path: &str,
        lsp_state: &mut LspState,
        lsp_specs: &[crate::editor::lsp::LspSpec],
        userdir: &str,
        verbose: bool,
    ) {
        if lsp_state.transport_id.is_some() {
            return;
        }
        let ext = file_path.rsplit('.').next().unwrap_or("");
        let Some(filetype) = ext_to_lsp_filetype(ext) else {
            return;
        };
        let Some(spec) = find_lsp_spec(filetype, lsp_specs) else {
            return;
        };
        let file_dir = Path::new(file_path)
            .parent()
            .and_then(Path::to_str)
            .unwrap_or(".");
        let root_dir = find_project_root(file_dir, &spec.root_patterns)
            .unwrap_or_else(|| file_dir.to_string());
        let environment = spec
            .env
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let roslyn_log_directory = (filetype == "c#").then(|| {
            let directory = Path::new(userdir).join("logs").join("roslyn");
            if let Err(error) = std::fs::create_dir_all(&directory) {
                log_to_file(
                    userdir,
                    &format!("Failed to create Roslyn log directory: {error}"),
                );
            }
            directory
        });
        let mut last_error = None;
        let candidates = lsp::command_candidates(spec, roslyn_log_directory.as_deref());
        if !candidates
            .iter()
            .any(|command| !lsp_state.rejected_launch_commands.contains(command))
        {
            lsp_state.rejected_launch_commands.clear();
            lsp_state.note_spawn_failure();
            return;
        }
        for command in candidates {
            if lsp_state.rejected_launch_commands.contains(&command) {
                continue;
            }
            match lsp::spawn_transport(&command, &root_dir, &environment) {
                Ok(tid) => {
                    lsp_state.launch_command = command;
                    lsp_state.transport_id = Some(tid);
                    lsp_state.root_uri = path_to_uri(&root_dir);
                    lsp_state.filetype = filetype.to_string();
                    let req_id = lsp_state.next_id();
                    lsp_state
                        .pending_requests
                        .insert(req_id, "initialize".to_string());
                    let _ = lsp::send_message(
                        tid,
                        &lsp_initialize_request_with_options(
                            req_id,
                            &lsp_state.root_uri,
                            spec.initialization_options.as_ref(),
                        ),
                    );
                    return;
                }
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(error) = last_error {
            lsp_state.note_spawn_failure();
            log_to_file(userdir, &format!("Failed to spawn LSP: {error}"));
            if verbose {
                eprintln!("Failed to spawn LSP: {error}");
            }
        }
    }

    /// Send `didOpen` for one document and ask for its diagnostics. Does
    /// nothing if the tab has since closed or the document is already open.
    fn announce_document_to_lsp(
        tid: u64,
        lsp_state: &mut LspState,
        docs: &[crate::editor::open_doc::OpenDoc],
        path: &str,
    ) {
        let uri = path_to_uri(path);
        if lsp_state.opened_documents.contains(&uri) {
            return;
        }
        let Some(doc) = docs.iter().find(|doc| doc.path == path) else {
            return;
        };
        if crate::editor::open_doc::doc_policy(doc).disable_lsp {
            return;
        }
        let Some(buf_id) = doc.view.buffer_id else {
            return;
        };
        let text = buffer::with_buffer(buf_id, |b| Ok(b.lines.join(""))).unwrap_or_default();
        let _ = lsp::send_message(
            tid,
            &lsp_did_open(&uri, lsp_language_id(&lsp_state.filetype), &text),
        );
        lsp_state.opened_documents.insert(uri.clone());
        lsp_state.document_texts.insert(uri.clone(), text);
        request_lsp_diagnostics(tid, lsp_state, &uri);
    }

    /// Measured tab-bar geometry: one entry per tab as
    /// `(x, width, displayed label, full label)`, plus the total width the
    /// tabs would need without truncation.
    ///
    /// Measuring a label means asking the font for its width, and the labels
    /// are built with allocation. Both would otherwise be repeated for every
    /// open tab on every redraw, including redraws caused by a blinking
    /// cursor, so the result is kept until something it depends on changes.
    #[derive(Default)]
    struct TabLayout {
        signature: u64,
        full_total: f64,
        rects: std::sync::Arc<Vec<(f64, f64, String, String)>>,
    }

    /// Everything the measured geometry depends on: the tabs themselves, the
    /// space they are laid out in, and the metrics used to measure them.
    fn tab_layout_signature(
        docs: &[crate::editor::open_doc::OpenDoc],
        width: f64,
        sidebar_w: f64,
        close_w: f64,
        style: &crate::editor::style_ctx::StyleContext,
    ) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        docs.len().hash(&mut hasher);
        for doc in docs {
            doc.name.hash(&mut hasher);
            doc_is_modified(doc).hash(&mut hasher);
        }
        for value in [
            width,
            sidebar_w,
            close_w,
            style.font_height,
            style.padding_x,
            style.divider_size,
        ] {
            value.to_bits().hash(&mut hasher);
        }
        style.font.hash(&mut hasher);
        hasher.finish()
    }

    fn request_lsp_diagnostics(tid: u64, lsp_state: &mut LspState, uri: &str) {
        if !lsp_state.pull_diagnostics {
            return;
        }
        let already_pending = lsp_state.pending_requests.iter().any(|(id, method)| {
            method == "textDocument/diagnostic"
                && lsp_state
                    .pending_request_uris
                    .get(id)
                    .is_some_and(|value| value == uri)
        });
        if already_pending {
            return;
        }
        let previous_result_id = lsp_state.diagnostic_result_ids.get(uri).cloned();
        let request_id = lsp_state.next_id();
        lsp_state
            .pending_requests
            .insert(request_id, "textDocument/diagnostic".to_string());
        lsp_state
            .pending_request_uris
            .insert(request_id, uri.to_string());
        let _ = lsp::send_message(
            tid,
            &lsp_document_diagnostic_request(request_id, uri, previous_result_id.as_deref()),
        );
    }

    // Try to start LSP for the first open file.
    if subsystems.has_lsp() {
        if let Some(doc) = docs.first() {
            try_start_lsp(
                &doc.path,
                &mut lsp_state,
                &lsp_specs,
                userdir,
                config.verbose,
            );
        }
    }

    // Clear any stale shutdown signal from prior runs.
    if crate::signal::shutdown_requested() {
        crate::signal::clear_shutdown();
    }

    // Unified command dispatch. The match body lives in
    // `commands_dispatch.rs` and is pulled in textually via `include!()`
    // so its ~830 lines of arms run in this scope and can read/write
    // every local variable directly. (A `macro_rules!` wrapper would
    // break: its `let cmd: String = $cmd_arg` binding is hygienic, so
    // the included `match cmd.as_str()` can't see it.) The three
    // invocations below each declare a local `cmd: String` before the
    // include so the dispatch body has it in scope.

    loop {
        if crate::signal::shutdown_requested() {
            crate::signal::clear_shutdown();
            if docs.iter().any(doc_is_modified) {
                nag = Nag::UnsavedChanges {
                    message: nag_msg_quit(&docs),
                    tab_to_close: None,
                };
                redraw = true;
            } else {
                quit = true;
            }
        }

        // Idle-drop: after N seconds with no events, release cached
        // glyph bitmaps and command buffers. Next interactive frame
        // rebuilds them lazily. This is most of the benefit of the
        // macOS memory-pressure hook without needing platform FFI.
        if !dropped_caches_for_idle && last_activity.elapsed().as_secs() >= IDLE_DROP_SECS {
            crate::renderer::drop_caches();
            dropped_caches_for_idle = true;
        }

        // macOS memory-pressure probe. `None` on other platforms.
        if last_mem_pressure_check.elapsed().as_secs() >= MEM_PRESSURE_CHECK_SECS {
            last_mem_pressure_check = Instant::now();
            if let Some(level) = crate::renderer::macos_memory_pressure_level() {
                if level >= 1 {
                    // WARN or CRITICAL -- release everything reclaimable.
                    crate::renderer::drop_caches();
                    compiled_syntax_cache.retain(|k, _| {
                        docs.iter()
                            .any(|d| d.path.rsplit('.').next().unwrap_or("") == k)
                    });
                    compiled_syntax_mru.retain(|k| compiled_syntax_cache.contains_key(k));
                }
            }
        }

        // Poll all pending events.
        let mut had_input_events = false;
        while let Some(event) = crate::window::poll_event_native() {
            // Any event counts as activity for idle-drop tracking.
            had_input_events = true;
            last_activity = Instant::now();
            dropped_caches_for_idle = false;
            // Scope each dialog field's undo history to a single open session:
            // while the input is closed its history stays cleared, so reopening
            // starts fresh and Ctrl+Z can never restore text from a previous
            // session into an unrelated field.
            if !find_active {
                find_history.clear();
                replace_history.clear();
            }
            if !cmdview_active {
                cmdview_history.clear();
            }
            if !project_search_active {
                project_search_history.clear();
            }
            if !project_replace_active {
                project_replace_search_history.clear();
                project_replace_with_history.clear();
            }
            if !palette_active {
                palette_history.clear();
            }
            if !notes_search_focused {
                notes_search_history.clear();
            }
            if sidebar_new_file_dir.is_none() {
                sidebar_new_file_history.clear();
            }
            match &event {
                EditorEvent::Quit => {
                    if single_file_mode && docs.iter().any(doc_is_modified) {
                        nag = Nag::UnsavedChanges {
                            message: nag_msg_quit(&docs),
                            tab_to_close: None,
                        };
                        redraw = true;
                    } else {
                        quit = true;
                    }
                }
                EditorEvent::Exposed | EditorEvent::Resized { .. } | EditorEvent::FocusGained => {
                    window_hidden = false;
                    // A covered/minimised window can lose its backing surface
                    // without SDL delivering a matching expose event.  The
                    // retained renderer may still consider every text cell
                    // current, so force a full repaint when the window becomes
                    // usable again.
                    crate::window::force_invalidate();
                    redraw = true;
                }
                EditorEvent::Shown => {
                    window_hidden = false;
                    crate::window::force_invalidate();
                    redraw = true;
                }
                EditorEvent::Occluded | EditorEvent::Hidden => {
                    window_hidden = true;
                    crate::renderer::drop_caches();
                }
                EditorEvent::KeyReleased { key, .. } => {
                    let k = key.as_str();
                    if k == "left shift" || k == "right shift" || k == "lshift" || k == "rshift" {
                        shift_held = false;
                    }
                    continue;
                }
                EditorEvent::KeyPressed { key, modifiers } => {
                    // Snap any in-flight smooth-scroll animation to its target.
                    // The lerp is event-driven (it only ticks on redraws), so
                    // pressing keys after a wheel scroll would otherwise resume
                    // the paused animation one tick at a time per press.
                    // Pressing any key signals "I'm done scrolling", so finalize
                    // the position immediately.
                    if let Some(doc) = docs.get_mut(active_tab) {
                        doc.view.scroll_y = doc.view.target_scroll_y;
                    }
                    // Modifier-only key presses (Ctrl/Shift/Alt/Gui alone) shouldn't
                    // touch the editor at all — no redraw, no blink reset, no scroll
                    // lerp tick. Only update the local shift tracker for shift+click.
                    // SDL reports modifier keys with platform-dependent names
                    // ("left ctrl" / "left control" / "lctrl"; "left gui" /
                    // "left meta" / "left super"), so match the family rather
                    // than a fixed string list.
                    let key_lc = key.as_str();
                    let is_modifier_only = matches!(
                        key_lc,
                        "left shift"
                            | "right shift"
                            | "lshift"
                            | "rshift"
                            | "left ctrl"
                            | "right ctrl"
                            | "lctrl"
                            | "rctrl"
                            | "left control"
                            | "right control"
                            | "left alt"
                            | "right alt"
                            | "lalt"
                            | "ralt"
                            | "left gui"
                            | "right gui"
                            | "lgui"
                            | "rgui"
                            | "left meta"
                            | "right meta"
                            | "lmeta"
                            | "rmeta"
                            | "left super"
                            | "right super"
                            | "lsuper"
                            | "rsuper"
                            | "left win"
                            | "right win"
                    );
                    if is_modifier_only {
                        if key_lc == "left shift"
                            || key_lc == "right shift"
                            || key_lc == "lshift"
                            || key_lc == "rshift"
                        {
                            shift_held = true;
                        }
                        continue;
                    }
                    pending_lsp_hover_request = None;
                    cursor_blink_reset = Instant::now();
                    let mut mods = *modifiers;
                    // On macOS, optionally fold Cmd into Ctrl so Cmd+S acts
                    // like Ctrl+S. See NativeConfig::mac_command_as_ctrl.
                    if cfg!(target_os = "macos") && config.mac_command_as_ctrl && mods.gui {
                        mods.ctrl = true;
                        mods.gui = false;
                    }

                    // Notes-mode sidebar search input.
                    if subsystems.has_notes_mode() && notes_search_focused {
                        if let Some(is_redo) = keymap_field_undo(&keymap, key.as_str(), mods) {
                            let restored = if is_redo {
                                notes_search_history.redo(&notes_search, notes_search.len())
                            } else {
                                notes_search_history.undo(&notes_search, notes_search.len())
                            };
                            if let Some((t, _)) = restored {
                                notes_search = t;
                            }
                            redraw = true;
                            continue;
                        }
                        if let Some(action) = keymap_field_clipboard(&keymap, key.as_str(), mods) {
                            match action {
                                FieldClipboard::Copy => {
                                    if !notes_search.is_empty() {
                                        crate::window::set_clipboard_text(&notes_search);
                                    }
                                }
                                FieldClipboard::Cut => {
                                    if !notes_search.is_empty() {
                                        crate::window::set_clipboard_text(&notes_search);
                                        notes_search_history.record(
                                            &notes_search,
                                            notes_search.len(),
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        notes_search.clear();
                                    }
                                }
                                FieldClipboard::Paste => {
                                    if let Some(clip) = crate::window::get_clipboard_text() {
                                        notes_search_history.record(
                                            &notes_search,
                                            notes_search.len(),
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        append_clipboard_line(&mut notes_search, &clip);
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        match key.as_str() {
                            "backspace" => {
                                if !notes_search.is_empty() {
                                    notes_search_history.record(
                                        &notes_search,
                                        notes_search.len(),
                                        FieldEdit::Delete,
                                        buffer::now_secs(),
                                    );
                                }
                                notes_search.pop();
                                redraw = true;
                                continue;
                            }
                            "escape" => {
                                notes_search.clear();
                                notes_search_focused = false;
                                redraw = true;
                                continue;
                            }
                            "return" | "enter" => {
                                notes_search_focused = false;
                                redraw = true;
                                continue;
                            }
                            _ => {}
                        }
                    }

                    // Tab overflow dropdown: Escape dismisses it.
                    if tab_dropdown_open && key.as_str() == "escape" {
                        tab_dropdown_open = false;
                        redraw = true;
                        continue;
                    }

                    // Context menu intercepts keys when visible.
                    if context_menu.visible {
                        match key.as_str() {
                            "escape" => {
                                context_menu.hide();
                                redraw = true;
                                continue;
                            }
                            "up" => {
                                if let Some(sel) = context_menu.selected {
                                    if sel > 0 {
                                        context_menu.selected = Some(sel - 1);
                                    }
                                } else if !context_menu.items.is_empty() {
                                    context_menu.selected = Some(context_menu.items.len() - 1);
                                }
                                redraw = true;
                                continue;
                            }
                            "down" => {
                                if let Some(sel) = context_menu.selected {
                                    if sel + 1 < context_menu.items.len() {
                                        context_menu.selected = Some(sel + 1);
                                    }
                                } else {
                                    context_menu.selected = Some(0);
                                }
                                redraw = true;
                                continue;
                            }
                            "return" | "keypad enter" => {
                                if let Some(sel) = context_menu.selected {
                                    if let Some(item) = context_menu.items.get(sel) {
                                        if let Some(ref cmd) = item.command {
                                            let cmd = cmd.clone();
                                            context_menu.hide();
                                            {
                                                include!("commands_dispatch.rs");
                                            }
                                        } else {
                                            context_menu.hide();
                                        }
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                            _ => {
                                context_menu.hide();
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    // Completion popup intercepts keys when visible.
                    if completion.visible {
                        match key.as_str() {
                            "escape" => {
                                completion.hide();
                                redraw = true;
                                continue;
                            }
                            "up" => {
                                if completion.selected > 0 {
                                    completion.selected -= 1;
                                }
                                redraw = true;
                                continue;
                            }
                            "down" => {
                                if completion.selected + 1 < completion.items.len() {
                                    completion.selected += 1;
                                }
                                redraw = true;
                                continue;
                            }
                            "return" | "keypad enter" | "tab" => {
                                if let Some((_, _, insert_text)) =
                                    completion.items.get(completion.selected)
                                {
                                    let text = insert_text.clone();
                                    if let Some(doc) = docs.get_mut(active_tab) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                buffer::push_undo(b);
                                                let line = *b.selections.first().unwrap_or(&1);
                                                let col = *b.selections.get(1).unwrap_or(&1);
                                                if line <= b.lines.len() {
                                                    let l = &mut b.lines[line - 1];
                                                    let byte_pos = char_to_byte(l, col - 1);
                                                    l.insert_str(byte_pos, &text);
                                                    let new_col = col + text.chars().count();
                                                    b.selections[0] = line;
                                                    b.selections[1] = new_col;
                                                    b.selections[2] = line;
                                                    b.selections[3] = new_col;
                                                }
                                                Ok(())
                                            });
                                        }
                                    }
                                }
                                completion.hide();
                                redraw = true;
                                continue;
                            }
                            _ => {
                                completion.hide();
                                // Fall through to normal key handling.
                            }
                        }
                    }

                    // Dismiss hover on any keypress.
                    if hover.visible {
                        hover.hide();
                        hovered_diagnostic = None;
                        diagnostic_hover_region_rect = None;
                        diagnostic_hover_leave_at = None;
                        diagnostic_quick_fix_rect = None;
                        diagnostic_hover_action_request = None;
                        diagnostic_hover_actions.clear();
                        redraw = true;
                    }
                    // Dismiss signature help on Escape; it persists while typing
                    // arguments so the parameter hint stays visible.
                    if key.as_str() == "escape" && signature_help.visible {
                        signature_help.hide();
                        redraw = true;
                    }

                    // Inline new-file input in the sidebar intercepts keys.
                    if sidebar_new_file_dir.is_some() && matches!(nag, Nag::None) {
                        if let Some(is_redo) = keymap_field_undo(&keymap, key.as_str(), mods) {
                            let restored = if is_redo {
                                sidebar_new_file_history
                                    .redo(&sidebar_new_file_name, sidebar_new_file_cursor)
                            } else {
                                sidebar_new_file_history
                                    .undo(&sidebar_new_file_name, sidebar_new_file_cursor)
                            };
                            if let Some((t, c)) = restored {
                                sidebar_new_file_name = t;
                                sidebar_new_file_cursor = c.min(sidebar_new_file_name.len());
                            }
                            redraw = true;
                            continue;
                        }
                        if let Some(action) = keymap_field_clipboard(&keymap, key.as_str(), mods) {
                            match action {
                                FieldClipboard::Copy => {
                                    if !sidebar_new_file_name.is_empty() {
                                        crate::window::set_clipboard_text(&sidebar_new_file_name);
                                    }
                                }
                                FieldClipboard::Cut => {
                                    if !sidebar_new_file_name.is_empty() {
                                        crate::window::set_clipboard_text(&sidebar_new_file_name);
                                        sidebar_new_file_history.record(
                                            &sidebar_new_file_name,
                                            sidebar_new_file_cursor,
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        sidebar_new_file_name.clear();
                                        sidebar_new_file_cursor = 0;
                                    }
                                }
                                FieldClipboard::Paste => {
                                    if let Some(clip) = crate::window::get_clipboard_text() {
                                        sidebar_new_file_history.record(
                                            &sidebar_new_file_name,
                                            sidebar_new_file_cursor,
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        sidebar_new_file_cursor = insert_clipboard_line(
                                            &mut sidebar_new_file_name,
                                            sidebar_new_file_cursor,
                                            &clip,
                                        );
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        match key.as_str() {
                            "escape" => {
                                sidebar_new_file_dir = None;
                                sidebar_new_file_name.clear();
                                sidebar_new_file_cursor = 0;
                            }
                            "return" | "keypad enter" => {
                                let name = sidebar_new_file_name.trim().to_string();
                                let dir = sidebar_new_file_dir.take().unwrap_or_default();
                                sidebar_new_file_name.clear();
                                sidebar_new_file_cursor = 0;
                                if !name.is_empty() {
                                    let full_path = std::path::Path::new(&dir)
                                        .join(&name)
                                        .to_string_lossy()
                                        .to_string();
                                    if std::path::Path::new(&full_path).exists() {
                                        info_message = Some((
                                            format!("File already exists: {name}"),
                                            Instant::now(),
                                        ));
                                    } else {
                                        match std::fs::write(&full_path, "") {
                                            Ok(()) => {
                                                if subsystems.has_sidebar()
                                                    && !project_root.is_empty()
                                                {
                                                    // Snapshot in-memory expanded
                                                    // dirs so the rescan doesn't
                                                    // collapse the folder the user
                                                    // just created into.
                                                    let in_memory_expanded: HashSet<String> =
                                                        sidebar_entries
                                                            .iter()
                                                            .filter(|e| e.is_dir && e.expanded)
                                                            .map(|e| e.path.clone())
                                                            .collect();
                                                    sidebar_entries = scan_for_sidebar(
                                                        subsystems.has_notes_mode(),
                                                        &project_root,
                                                        sidebar_show_hidden,
                                                    );
                                                    restore_expanded_folders(
                                                        &mut sidebar_entries,
                                                        userdir_path,
                                                        sidebar_show_hidden,
                                                        &project_session_key(&project_root),
                                                    );
                                                    expand_sidebar_from_set(
                                                        &mut sidebar_entries,
                                                        &in_memory_expanded,
                                                        sidebar_show_hidden,
                                                    );
                                                }
                                                if open_file_into(&full_path, &mut docs, use_git())
                                                {
                                                    autoreload.watch(&full_path);
                                                    active_tab = docs.len() - 1;
                                                    remember_recent_file(
                                                        &mut recent_files,
                                                        &full_path,
                                                        userdir_path,
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                info_message = Some((
                                                    format!("Create failed: {e}"),
                                                    Instant::now(),
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                            "backspace" if sidebar_new_file_cursor > 0 => {
                                sidebar_new_file_history.record(
                                    &sidebar_new_file_name,
                                    sidebar_new_file_cursor,
                                    FieldEdit::Delete,
                                    buffer::now_secs(),
                                );
                                let prev = sidebar_new_file_name[..sidebar_new_file_cursor]
                                    .char_indices()
                                    .next_back()
                                    .map(|(i, _)| i)
                                    .unwrap_or(0);
                                sidebar_new_file_name.drain(prev..sidebar_new_file_cursor);
                                sidebar_new_file_cursor = prev;
                            }
                            "backspace" => {}
                            "delete" if sidebar_new_file_cursor < sidebar_new_file_name.len() => {
                                sidebar_new_file_history.record(
                                    &sidebar_new_file_name,
                                    sidebar_new_file_cursor,
                                    FieldEdit::Delete,
                                    buffer::now_secs(),
                                );
                                let next = sidebar_new_file_name[sidebar_new_file_cursor..]
                                    .char_indices()
                                    .nth(1)
                                    .map(|(i, _)| sidebar_new_file_cursor + i)
                                    .unwrap_or(sidebar_new_file_name.len());
                                sidebar_new_file_name.drain(sidebar_new_file_cursor..next);
                            }
                            "delete" => {}
                            "left" if sidebar_new_file_cursor > 0 => {
                                sidebar_new_file_cursor = sidebar_new_file_name
                                    [..sidebar_new_file_cursor]
                                    .char_indices()
                                    .next_back()
                                    .map(|(i, _)| i)
                                    .unwrap_or(0);
                            }
                            "left" => {}
                            "right" if sidebar_new_file_cursor < sidebar_new_file_name.len() => {
                                sidebar_new_file_cursor = sidebar_new_file_name
                                    [sidebar_new_file_cursor..]
                                    .char_indices()
                                    .nth(1)
                                    .map(|(i, _)| sidebar_new_file_cursor + i)
                                    .unwrap_or(sidebar_new_file_name.len());
                            }
                            "right" => {}
                            "home" => {
                                sidebar_new_file_cursor = 0;
                            }
                            "end" => {
                                sidebar_new_file_cursor = sidebar_new_file_name.len();
                            }
                            _ => {}
                        }
                        redraw = true;
                        continue;
                    }

                    // Command view (file/folder open) intercepts keys — but
                    // only while no nag is active. When a modal nag (overwrite,
                    // create-dir, reload-from-disk) is up the cmdview stays on
                    // screen but its keypress arm must step aside so Y / N /
                    // Enter can reach the nag handler below.
                    if cmdview_active
                        && matches!(nag, Nag::None)
                        && (subsystems.has_picker()
                            || cmdview_mode == CmdViewMode::SaveAs
                            || cmdview_mode == CmdViewMode::OpenFile
                            || cmdview_mode == CmdViewMode::OpenRecent
                            || cmdview_mode == CmdViewMode::Rename)
                    {
                        /// Expand ~ and resolve relative paths to absolute.
                        /// On Windows, treat both `/` and `\` as absolute-path
                        /// indicators (`C:\...`) and use `USERPROFILE` for `~`.
                        fn expand_path(text: &str, project_root: &str) -> String {
                            let home_key = if cfg!(target_os = "windows") {
                                "USERPROFILE"
                            } else {
                                "HOME"
                            };
                            if let Some(rest) = text.strip_prefix('~') {
                                if let Some(home) = std::env::var_os(home_key) {
                                    return format!("{}{rest}", home.to_string_lossy());
                                }
                            }
                            if std::path::Path::new(text).is_absolute() {
                                return text.to_string();
                            }
                            let joined = std::path::Path::new(project_root)
                                .join(text)
                                .to_string_lossy()
                                .into_owned();
                            normalize_path(&joined)
                        }

                        /// Byte index of the previous character before `cursor` in `text`.
                        fn cmdview_prev_char(text: &str, cursor: usize) -> usize {
                            text[..cursor]
                                .char_indices()
                                .next_back()
                                .map(|(i, _)| i)
                                .unwrap_or(0)
                        }
                        /// Byte index of the next character at or after `cursor` in `text`.
                        fn cmdview_next_char(text: &str, cursor: usize) -> usize {
                            if cursor >= text.len() {
                                return text.len();
                            }
                            text[cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| cursor + i)
                                .unwrap_or(text.len())
                        }
                        /// Jump left to the start of the previous path segment.
                        /// Accepts both `/` and `\` as separators so Windows
                        /// paths with backslashes behave the same as Unix
                        /// forward-slash paths.
                        fn cmdview_word_left(text: &str, cursor: usize) -> usize {
                            if cursor == 0 {
                                return 0;
                            }
                            let s = &text[..cursor];
                            let stripped = s.trim_end_matches(['/', '\\']);
                            if let Some(idx) = stripped.rfind(['/', '\\']) {
                                idx + 1
                            } else {
                                0
                            }
                        }
                        /// Jump right to the start of the next path segment.
                        fn cmdview_word_right(text: &str, cursor: usize) -> usize {
                            if cursor >= text.len() {
                                return text.len();
                            }
                            let rest = &text[cursor..];
                            let skip = if rest.starts_with('/') || rest.starts_with('\\') {
                                1
                            } else {
                                0
                            };
                            match rest[skip..].find(['/', '\\']) {
                                Some(idx) => cursor + skip + idx + 1,
                                None => text.len(),
                            }
                        }

                        if let Some(is_redo) = keymap_field_undo(&keymap, key.as_str(), mods) {
                            // Route the undo/redo bindings to the picker input.
                            let restored = if is_redo {
                                cmdview_history.redo(&cmdview_text, cmdview_cursor)
                            } else {
                                cmdview_history.undo(&cmdview_text, cmdview_cursor)
                            };
                            if let Some((t, c)) = restored {
                                cmdview_text = t;
                                cmdview_cursor = c.min(cmdview_text.len());
                                refresh_cmdview_suggestions(
                                    cmdview_mode,
                                    &cmdview_text,
                                    &project_root,
                                    &recent_files,
                                    &recent_projects,
                                    !single_file_mode,
                                    &mut cmdview_suggestions,
                                );
                                cmdview_selected = 0;
                            }
                            redraw = true;
                            continue;
                        }
                        if let Some(action) = keymap_field_clipboard(&keymap, key.as_str(), mods) {
                            match action {
                                FieldClipboard::Copy => {
                                    if !cmdview_text.is_empty() {
                                        crate::window::set_clipboard_text(&cmdview_text);
                                    }
                                }
                                FieldClipboard::Cut => {
                                    if !cmdview_text.is_empty() {
                                        crate::window::set_clipboard_text(&cmdview_text);
                                        cmdview_history.record(
                                            &cmdview_text,
                                            cmdview_cursor,
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        cmdview_text.clear();
                                        cmdview_cursor = 0;
                                        refresh_cmdview_suggestions(
                                            cmdview_mode,
                                            &cmdview_text,
                                            &project_root,
                                            &recent_files,
                                            &recent_projects,
                                            !single_file_mode,
                                            &mut cmdview_suggestions,
                                        );
                                        cmdview_selected = 0;
                                    }
                                }
                                FieldClipboard::Paste => {
                                    if let Some(clip) = crate::window::get_clipboard_text() {
                                        cmdview_history.record(
                                            &cmdview_text,
                                            cmdview_cursor,
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        cmdview_cursor = insert_clipboard_line(
                                            &mut cmdview_text,
                                            cmdview_cursor,
                                            &clip,
                                        );
                                        refresh_cmdview_suggestions(
                                            cmdview_mode,
                                            &cmdview_text,
                                            &project_root,
                                            &recent_files,
                                            &recent_projects,
                                            !single_file_mode,
                                            &mut cmdview_suggestions,
                                        );
                                        cmdview_selected = 0;
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        match key.as_str() {
                            "escape" => {
                                cmdview_active = false;
                            }
                            "return" | "keypad enter" => {
                                // Go-to-line mode: parse number and jump.
                                if cmdview_label.starts_with("Go To Line") {
                                    if let Ok(target) = cmdview_text.trim().parse::<usize>() {
                                        if let Some(doc) = docs.get_mut(active_tab) {
                                            if let Some(buf_id) = doc.view.buffer_id {
                                                let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                    let ln = target.clamp(1, b.lines.len());
                                                    b.selections = vec![ln, 1, ln, 1];
                                                    Ok(())
                                                });
                                                let line_h = style.code_font_height * 1.2;
                                                doc.view.scroll_y = ((target as f64 - 1.0)
                                                    * line_h
                                                    - doc.view.rect().h / 2.0)
                                                    .max(0.0);
                                                doc.view.target_scroll_y = doc.view.scroll_y;
                                            }
                                        }
                                    }
                                    cmdview_active = false;
                                    redraw = true;
                                    continue;
                                }
                                // In Save As, Enter commits exactly what the user
                                // typed — never the highlighted suggestion — so
                                // autocomplete races can't silently retarget the
                                // save onto an existing file. Other modes keep
                                // the old "use suggestion if one is highlighted"
                                // behaviour so Enter on a sidebar match still
                                // works.
                                let chosen = if cmdview_mode == CmdViewMode::SaveAs {
                                    cmdview_text.clone()
                                } else if !cmdview_suggestions.is_empty()
                                    && cmdview_selected < cmdview_suggestions.len()
                                {
                                    cmdview_suggestions[cmdview_selected].clone()
                                } else {
                                    cmdview_text.clone()
                                };
                                let path = expand_path(&chosen, &project_root);
                                let path = path.trim_end_matches('/').to_string();
                                let p = std::path::Path::new(&path);
                                match cmdview_mode {
                                    CmdViewMode::LspRename => {
                                        // The typed text is the new symbol name, not
                                        // a path, so use it directly.
                                        let new_name = cmdview_text.trim().to_string();
                                        if !new_name.is_empty()
                                            && let Some((uri, line0, char0)) = lsp_rename_pos.take()
                                            && subsystems.has_lsp()
                                            && lsp_state.initialized
                                            && let Some(tid) = lsp_state.transport_id
                                        {
                                            let req_id = lsp_state.next_id();
                                            lsp_state
                                                .pending_requests
                                                .insert(req_id, "textDocument/rename".to_string());
                                            let _ = lsp::send_message(
                                                tid,
                                                &lsp_rename_request(
                                                    req_id, &uri, line0, char0, &new_name,
                                                ),
                                            );
                                        }
                                        lsp_rename_pos = None;
                                        cmdview_active = false;
                                        redraw = true;
                                        continue;
                                    }
                                    CmdViewMode::OpenFile => {
                                        // Support path:N to open at a specific line.
                                        let (file_path, goto_line) = split_path_line(&path);
                                        let (actual, line) = if goto_line.is_some()
                                            && !p.is_file()
                                            && std::path::Path::new(file_path).is_file()
                                        {
                                            (file_path.to_string(), goto_line)
                                        } else {
                                            (path.clone(), None)
                                        };
                                        let ap = std::path::Path::new(&actual);
                                        if ap.is_file() {
                                            cmdview_active = false;
                                            if single_file_mode {
                                                // Replace current doc.
                                                for d in &docs { autoreload.unwatch(&d.path); }
                                                docs.clear();
                                                active_tab = 0;
                                            }
                                            match check_file_size_limit(
                                                &actual,
                                                config.large_file.hard_limit_mb,
                                            ) {
                                                Err(msg) => {
                                                    info_message = Some((msg, Instant::now()));
                                                }
                                                Ok(sz) => {
                                                    let configured_bg_threshold =
                                                        crate::editor::open_doc::PerformancePolicy::background_load_threshold();
                                                    if sz > configured_bg_threshold
                                                        && load_job.is_none()
                                                    {
                                                        load_job = Some(spawn_load(&actual, sz));
                                                    } else if open_file_into(&actual, &mut docs, use_git()) {
                                                        active_tab = docs.len() - 1;
                                                        autoreload.watch(&actual);
                                                        remember_recent_file(&mut recent_files, &actual, userdir_path);
                                                        if let Some(ln) = line {
                                                            scroll_new_doc_to_line(
                                                                &mut docs,
                                                                ln,
                                                                style.code_font_height * 1.2,
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        } else if ap.is_dir() {
                                            // Navigate into directory.
                                            cmdview_history.record(
                                                &cmdview_text,
                                                cmdview_cursor,
                                                FieldEdit::Replace,
                                                buffer::now_secs(),
                                            );
                                            cmdview_text = dir_with_trailing_sep(&path);
                                            cmdview_cursor = cmdview_text.len();
                                            cmdview_suggestions =
                                                path_suggest(&cmdview_text, &project_root, false);
                                            cmdview_selected = 0;
                                        }
                                    }
                                    CmdViewMode::OpenFolder => {
                                        if p.is_dir() {
                                            // Check for unsaved changes before switching.
                                            if docs.iter().any(doc_is_modified) {
                                                nag = Nag::UnsavedChanges { message: nag_msg_quit(&docs), tab_to_close: None };
                                            } else {
                                                if subsystems.has_sidebar() {
                                                    save_project_session(
                                                        userdir_path,
                                                        &project_root,
                                                        &docs,
                                                        active_tab,
                                                    );
                                                    save_expanded_folders(&sidebar_entries, userdir_path, &project_session_key(&project_root));
                                                }
                                                for d in &docs {
                                                    autoreload.unwatch(&d.path);
                                                }
                                                docs.clear();
                                                active_tab = 0;
                                                cmdview_active = false;
                                                project_root = path;
                                                if subsystems.has_sidebar() {
                                                    sidebar_watcher.unwatch_all();
                                                    sidebar_entries = scan_for_sidebar(
                                                        subsystems.has_notes_mode(),
                                                        &project_root,
                                                        sidebar_show_hidden,
                                                    );
                                                    restore_expanded_folders(
                                                        &mut sidebar_entries,
                                                        userdir_path,
                                                        sidebar_show_hidden,
                                                        &project_session_key(&project_root),
                                                    );
                                                    sidebar_watcher.watch_dir(&project_root);
                                                    for entry in &sidebar_entries {
                                                        if entry.is_dir && entry.expanded {
                                                            sidebar_watcher
                                                                .watch_dir(&entry.path);
                                                        }
                                                    }
                                                    sidebar_visible = true;
                                                    if let Some(tab) = restore_project_session(
                                                        userdir_path,
                                                        &project_root,
                                                        &mut docs,
                                                        &mut autoreload, use_git(),
                                                    ) {
                                                        active_tab = tab;
                                                    }
                                                }
                                                let abs = std::fs::canonicalize(&project_root)
                                                    .map(|p| p.to_string_lossy().to_string())
                                                    .unwrap_or_else(|_| project_root.clone());
                                                recent_projects.retain(|p| p != &abs);
                                                recent_projects.insert(0, abs);
                                                if recent_projects.len() > 20 {
                                                    recent_projects.truncate(20);
                                                }
                                                let _ = crate::editor::storage::save_text(
                                                    userdir_path,
                                                    "session",
                                                    "recent_projects",
                                                    &serde_json::to_string(&recent_projects)
                                                        .unwrap_or_default(),
                                                );
                                            }
                                        }
                                    }
                                    CmdViewMode::OpenRecent => {
                                        cmdview_active = false;
                                        if p.is_file() {
                                            if open_file_into(&path, &mut docs, use_git()) {
                                                active_tab = docs.len() - 1;
                                                autoreload.watch(&path);
                                                remember_recent_file(&mut recent_files, &path, userdir_path);
                                            }
                                        } else if p.is_dir() {
                                            if docs.iter().any(doc_is_modified) {
                                                nag = Nag::UnsavedChanges { message: nag_msg_quit(&docs), tab_to_close: None };
                                            } else {
                                                if subsystems.has_sidebar() {
                                                    save_project_session(
                                                        userdir_path,
                                                        &project_root,
                                                        &docs,
                                                        active_tab,
                                                    );
                                                    save_expanded_folders(&sidebar_entries, userdir_path, &project_session_key(&project_root));
                                                }
                                                for d in &docs {
                                                    autoreload.unwatch(&d.path);
                                                }
                                                docs.clear();
                                                active_tab = 0;
                                                project_root = path;
                                                if subsystems.has_sidebar() {
                                                    sidebar_watcher.unwatch_all();
                                                    sidebar_entries = scan_for_sidebar(
                                                        subsystems.has_notes_mode(),
                                                        &project_root,
                                                        sidebar_show_hidden,
                                                    );
                                                    restore_expanded_folders(
                                                        &mut sidebar_entries,
                                                        userdir_path,
                                                        sidebar_show_hidden,
                                                        &project_session_key(&project_root),
                                                    );
                                                    sidebar_watcher.watch_dir(&project_root);
                                                    for entry in &sidebar_entries {
                                                        if entry.is_dir && entry.expanded {
                                                            sidebar_watcher
                                                                .watch_dir(&entry.path);
                                                        }
                                                    }
                                                    sidebar_visible = true;
                                                    if let Some(tab) = restore_project_session(
                                                        userdir_path,
                                                        &project_root,
                                                        &mut docs,
                                                        &mut autoreload, use_git(),
                                                    ) {
                                                        active_tab = tab;
                                                    }
                                                }
                                                update_recent(
                                                    &mut recent_projects,
                                                    &project_root,
                                                    20,
                                                );
                                                let _ = crate::editor::storage::save_text(
                                                    userdir_path,
                                                    "session",
                                                    "recent_projects",
                                                    &serde_json::to_string(&recent_projects)
                                                        .unwrap_or_default(),
                                                );
                                            }
                                        } else {
                                            info_message = Some((
                                                format!("Recent item no longer exists: {path}"),
                                                Instant::now(),
                                            ));
                                        }
                                    }
                                    CmdViewMode::SaveAs => {
                                        // Save current document to the chosen path.
                                        let save_path = if p.is_dir() {
                                            // User selected a directory -- stay in cmdview.
                                            cmdview_history.record(
                                                &cmdview_text,
                                                cmdview_cursor,
                                                FieldEdit::Replace,
                                                buffer::now_secs(),
                                            );
                                            cmdview_text = dir_with_trailing_sep(&path);
                                            cmdview_cursor = cmdview_text.len();
                                            cmdview_suggestions = path_suggest(&cmdview_text, &project_root, false);
                                            cmdview_selected = 0;
                                            continue;
                                        } else {
                                            path.clone()
                                        };
                                        // If the parent directory is missing,
                                        // defer the save until the user confirms
                                        // creating the missing directories.
                                        let parent_missing = std::path::Path::new(&save_path)
                                            .parent()
                                            .map(|p| {
                                                !p.as_os_str().is_empty() && !p.exists()
                                            })
                                            .unwrap_or(false);
                                        if parent_missing {
                                            let parent_str = std::path::Path::new(&save_path)
                                                .parent()
                                                .map(|p| p.to_string_lossy().to_string())
                                                .unwrap_or_default();
                                            nag = Nag::CreateDir { parent: parent_str, save_path: save_path.clone(), doc_tab: active_tab, from_save_as: true };
                                            continue;
                                        }
                                        // Warn if the target filename has no
                                        // extension — common typo / forgot-to-
                                        // type-.ext case. Check the last path
                                        // segment so `/etc/hosts` (no ext) still
                                        // nags, and `foo.bar/README` counts the
                                        // filename as having no ext.
                                        let fname = std::path::Path::new(&save_path)
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or("");
                                        let has_ext = fname
                                            .rfind('.')
                                            .is_some_and(|i| i > 0 && i < fname.len() - 1);
                                        if !has_ext {
                                            nag = Nag::NoExtension {
                                                save_path: save_path.clone(),
                                                doc_tab: active_tab,
                                            };
                                            redraw = true;
                                            continue;
                                        }
                                        // If the target exists and isn't the
                                        // current doc's own path, nag for
                                        // overwrite confirmation. This blocks
                                        // the autocomplete-races-Enter case
                                        // where a late-arriving suggestion
                                        // silently retargets the save.
                                        let own_path = docs
                                            .get(active_tab)
                                            .map(|d| d.path.as_str())
                                            .unwrap_or("");
                                        if std::path::Path::new(&save_path).is_file()
                                            && save_path != own_path
                                        {
                                            nag = Nag::OverwriteFile {
                                                save_path: save_path.clone(),
                                                doc_tab: active_tab,
                                            };
                                            redraw = true;
                                            continue;
                                        }
                                        if let Some(doc) = docs.get_mut(active_tab) {
                                            if let Some(buf_id) = doc.view.buffer_id {
                                                let atomic = config.files.atomic_save;
                                                let saved_id = buffer::with_buffer(buf_id, |b| {
                                                    buffer::save_file(b, &save_path, b.crlf, atomic)
                                                        .map_err(|_| buffer::BufferError::UnknownBuffer)?;
                                                    Ok(b.change_id)
                                                });
                                                if let Ok(id) = saved_id {
                                                    doc.saved_change_id = id;
                                                    doc.saved_signature = buffer::with_buffer(buf_id, |b| Ok(buffer::content_signature(&b.lines))).unwrap_or(0);
                                                    doc.path = save_path.clone();
                                                    doc.name = std::path::Path::new(&save_path)
                                                        .file_name()
                                                        .map(|n| n.to_string_lossy().to_string())
                                                        .unwrap_or_else(|| save_path.clone());
                                                    doc.cached_change_id = -1;
                                                    doc.cached_render = std::sync::Arc::new(Vec::new());
                                                    autoreload.watch(&save_path);
                                                    log_to_file(userdir, &format!("Saved {save_path}"));
                                                    info_message = Some((format!("Saved {}", doc.name), Instant::now()));
                                                } else {
                                                    info_message = Some((format!("Failed to save {save_path}"), Instant::now()));
                                                }
                                            }
                                        }
                                        // Save-as can create a new file or land an existing
                                        // buffer at a fresh path — rescan so the sidebar
                                        // picks it up. Gated on project_root prefix so
                                        // saves outside the project don't trigger a scan.
                                        if subsystems.has_sidebar()
                                            && !project_root.is_empty()
                                            && std::path::Path::new(&save_path)
                                                .starts_with(std::path::Path::new(&project_root))
                                        {
                                            sidebar_entries = scan_for_sidebar(
                                                subsystems.has_notes_mode(),
                                                &project_root,
                                                sidebar_show_hidden,
                                            );
                                            restore_expanded_folders(
                                                &mut sidebar_entries,
                                                userdir_path,
                                                sidebar_show_hidden,
                                                &project_session_key(&project_root),
                                            );
                                        }
                                        cmdview_active = false;
                                    }
                                    CmdViewMode::Rename => {
                                        let src = std::mem::take(&mut rename_source);
                                        let dst = path.clone();
                                        cmdview_active = false;
                                        if src.is_empty() || src == dst {
                                            // nothing to do
                                        } else if std::path::Path::new(&dst).exists() {
                                            info_message = Some((
                                                format!("Target exists: {dst}"),
                                                Instant::now(),
                                            ));
                                        } else {
                                            if let Some(parent) =
                                                std::path::Path::new(&dst).parent()
                                            {
                                                let _ = std::fs::create_dir_all(parent);
                                            }
                                            match std::fs::rename(&src, &dst) {
                                                Ok(()) => {
                                                    for d in docs.iter_mut() {
                                                        if d.path == src {
                                                            autoreload.unwatch(&src);
                                                            d.path = dst.clone();
                                                            d.name = std::path::Path::new(&dst)
                                                                .file_name()
                                                                .map(|n| {
                                                                    n.to_string_lossy().to_string()
                                                                })
                                                                .unwrap_or_else(|| dst.clone());
                                                            autoreload.watch(&dst);
                                                        }
                                                    }
                                                    if subsystems.has_sidebar()
                                                        && !project_root.is_empty()
                                                    {
                                                        sidebar_entries = scan_for_sidebar(
                                                            subsystems.has_notes_mode(),
                                                            &project_root,
                                                            sidebar_show_hidden,
                                                        );
                                                        restore_expanded_folders(
                                                            &mut sidebar_entries,
                                                            userdir_path,
                                                            sidebar_show_hidden,
                                                            &project_session_key(&project_root),
                                                        );
                                                    }
                                                    info_message = Some((
                                                        format!("Renamed to {dst}"),
                                                        Instant::now(),
                                                    ));
                                                }
                                                Err(e) => {
                                                    info_message = Some((
                                                        format!("Rename failed: {e}"),
                                                        Instant::now(),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            "tab"
                                // Select current suggestion: replace text, refresh.
                                if !cmdview_suggestions.is_empty()
                                    && cmdview_selected < cmdview_suggestions.len()
                                => {
                                    cmdview_history.record(
                                        &cmdview_text,
                                        cmdview_cursor,
                                        FieldEdit::Replace,
                                        buffer::now_secs(),
                                    );
                                    cmdview_text = cmdview_suggestions[cmdview_selected].clone();
                                    cmdview_cursor = cmdview_text.len();
                                    let dirs_only = cmdview_mode == CmdViewMode::OpenFolder;
                                    cmdview_suggestions =
                                        path_suggest(&cmdview_text, &project_root, dirs_only);
                                    cmdview_selected = 0;
                                }
                            "up" => {
                                if cmdview_selected > 0 {
                                    cmdview_selected -= 1;
                                } else if !cmdview_suggestions.is_empty() {
                                    cmdview_selected = cmdview_suggestions.len() - 1;
                                }
                            }
                            "down"
                                if !cmdview_suggestions.is_empty() => {
                                    cmdview_selected =
                                        (cmdview_selected + 1) % cmdview_suggestions.len();
                                }
                            "left" => {
                                if mods.ctrl {
                                    cmdview_cursor =
                                        cmdview_word_left(&cmdview_text, cmdview_cursor);
                                } else {
                                    cmdview_cursor =
                                        cmdview_prev_char(&cmdview_text, cmdview_cursor);
                                }
                            }
                            "right" => {
                                if mods.ctrl {
                                    cmdview_cursor =
                                        cmdview_word_right(&cmdview_text, cmdview_cursor);
                                } else if cmdview_cursor == cmdview_text.len()
                                    && !cmdview_suggestions.is_empty()
                                    && cmdview_selected < cmdview_suggestions.len()
                                {
                                    // Right-arrow at end of input accepts the
                                    // highlighted suggestion (like Tab) so
                                    // users aren't forced to press Enter —
                                    // which also commits the action and can
                                    // race a late autocomplete update.
                                    cmdview_history.record(
                                        &cmdview_text,
                                        cmdview_cursor,
                                        FieldEdit::Replace,
                                        buffer::now_secs(),
                                    );
                                    cmdview_text =
                                        cmdview_suggestions[cmdview_selected].clone();
                                    cmdview_cursor = cmdview_text.len();
                                    let dirs_only = cmdview_mode == CmdViewMode::OpenFolder;
                                    cmdview_suggestions = path_suggest(
                                        &cmdview_text,
                                        &project_root,
                                        dirs_only,
                                    );
                                    cmdview_selected = 0;
                                } else {
                                    cmdview_cursor =
                                        cmdview_next_char(&cmdview_text, cmdview_cursor);
                                }
                            }
                            "home" => {
                                cmdview_cursor = 0;
                            }
                            "end" => {
                                cmdview_cursor = cmdview_text.len();
                            }
                            "delete"
                                if cmdview_cursor < cmdview_text.len() => {
                                    cmdview_history.record(
                                        &cmdview_text,
                                        cmdview_cursor,
                                        FieldEdit::Delete,
                                        buffer::now_secs(),
                                    );
                                    let next = cmdview_next_char(&cmdview_text, cmdview_cursor);
                                    cmdview_text.replace_range(cmdview_cursor..next, "");
                                    refresh_cmdview_suggestions(
                                        cmdview_mode,
                                        &cmdview_text,
                                        &project_root,
                                        &recent_files,
                                        &recent_projects,
                                        !single_file_mode,
                                        &mut cmdview_suggestions,
                                    );
                                    cmdview_selected = 0;
                                }
                            "backspace" => {
                                if cmdview_cursor > 0 {
                                    cmdview_history.record(
                                        &cmdview_text,
                                        cmdview_cursor,
                                        FieldEdit::Delete,
                                        buffer::now_secs(),
                                    );
                                }
                                if mods.ctrl {
                                    // Delete the previous path segment up to the cursor.
                                    let segment_start =
                                        cmdview_word_left(&cmdview_text, cmdview_cursor);
                                    cmdview_text.replace_range(segment_start..cmdview_cursor, "");
                                    cmdview_cursor = segment_start;
                                } else if cmdview_cursor > 0 {
                                    let prev = cmdview_prev_char(&cmdview_text, cmdview_cursor);
                                    cmdview_text.replace_range(prev..cmdview_cursor, "");
                                    cmdview_cursor = prev;
                                }
                                refresh_cmdview_suggestions(
                                    cmdview_mode,
                                    &cmdview_text,
                                    &project_root,
                                    &recent_files,
                                    &recent_projects,
                                    !single_file_mode,
                                    &mut cmdview_suggestions,
                                );
                                cmdview_selected = 0;
                            }
                            _ => {}
                        }
                        redraw = true;
                        continue;
                    }

                    // Project search intercepts keys when active.
                    if subsystems.has_find_in_files() && project_search_active {
                        if mods.alt && !mods.ctrl {
                            let toggled = match key.as_str() {
                                "r" => {
                                    project_use_regex = !project_use_regex;
                                    true
                                }
                                "w" => {
                                    project_whole_word = !project_whole_word;
                                    true
                                }
                                "i" => {
                                    project_case_insensitive = !project_case_insensitive;
                                    true
                                }
                                _ => false,
                            };
                            if toggled {
                                project_search_results = run_project_search(
                                    &project_search_query,
                                    &project_root,
                                    project_use_regex,
                                    project_whole_word,
                                    project_case_insensitive,
                                );
                                project_search_selected = 0;
                                redraw = true;
                                continue;
                            }
                        }
                        if let Some(is_redo) = keymap_field_undo(&keymap, key.as_str(), mods) {
                            let restored = if is_redo {
                                project_search_history
                                    .redo(&project_search_query, project_search_query.len())
                            } else {
                                project_search_history
                                    .undo(&project_search_query, project_search_query.len())
                            };
                            if let Some((t, _)) = restored {
                                project_search_query = t;
                                project_search_results = run_project_search(
                                    &project_search_query,
                                    &project_root,
                                    project_use_regex,
                                    project_whole_word,
                                    project_case_insensitive,
                                );
                                project_search_selected = 0;
                            }
                            redraw = true;
                            continue;
                        }
                        if let Some(action) = keymap_field_clipboard(&keymap, key.as_str(), mods) {
                            match action {
                                FieldClipboard::Copy => {
                                    if !project_search_query.is_empty() {
                                        crate::window::set_clipboard_text(&project_search_query);
                                    }
                                }
                                FieldClipboard::Cut => {
                                    if !project_search_query.is_empty() {
                                        crate::window::set_clipboard_text(&project_search_query);
                                        project_search_history.record(
                                            &project_search_query,
                                            project_search_query.len(),
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        project_search_query.clear();
                                        project_search_results = run_project_search(
                                            &project_search_query,
                                            &project_root,
                                            project_use_regex,
                                            project_whole_word,
                                            project_case_insensitive,
                                        );
                                        project_search_selected = 0;
                                    }
                                }
                                FieldClipboard::Paste => {
                                    if let Some(clip) = crate::window::get_clipboard_text() {
                                        project_search_history.record(
                                            &project_search_query,
                                            project_search_query.len(),
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        append_clipboard_line(&mut project_search_query, &clip);
                                        project_search_results = run_project_search(
                                            &project_search_query,
                                            &project_root,
                                            project_use_regex,
                                            project_whole_word,
                                            project_case_insensitive,
                                        );
                                        project_search_selected = 0;
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        match key.as_str() {
                            "escape" => {
                                project_search_active = false;
                            }
                            "return" | "keypad enter" => {
                                if let Some((path, line_num, _)) =
                                    project_search_results.get(project_search_selected).cloned()
                                {
                                    project_search_active = false;
                                    // Open or switch to the file.
                                    let tab_idx = docs.iter().position(|d| d.path == path);
                                    let idx = if let Some(i) = tab_idx {
                                        i
                                    } else if open_file_into(&path, &mut docs, use_git()) {
                                        autoreload.watch(&path);
                                        remember_recent_file(
                                            &mut recent_files,
                                            &path,
                                            userdir_path,
                                        );
                                        docs.len() - 1
                                    } else {
                                        redraw = true;
                                        continue;
                                    };
                                    active_tab = idx;
                                    // Move cursor to the matched line.
                                    if let Some(doc) = docs.get_mut(active_tab) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                let target = line_num.min(b.lines.len()).max(1);
                                                b.selections[0] = target;
                                                b.selections[1] = 1;
                                                b.selections[2] = target;
                                                b.selections[3] = 1;
                                                Ok(())
                                            });
                                        }
                                    }
                                }
                            }
                            "up" => {
                                project_search_selected = project_search_selected.saturating_sub(1);
                            }
                            "down" if !project_search_results.is_empty() => {
                                project_search_selected = (project_search_selected + 1)
                                    .min(project_search_results.len() - 1);
                            }
                            "backspace" => {
                                if !project_search_query.is_empty() {
                                    project_search_history.record(
                                        &project_search_query,
                                        project_search_query.len(),
                                        FieldEdit::Delete,
                                        buffer::now_secs(),
                                    );
                                }
                                project_search_query.pop();
                                project_search_results = run_project_search(
                                    &project_search_query,
                                    &project_root,
                                    project_use_regex,
                                    project_whole_word,
                                    project_case_insensitive,
                                );
                                project_search_selected = 0;
                            }
                            _ => {}
                        }
                        redraw = true;
                        continue;
                    }

                    // Project replace intercepts keys when active.
                    if subsystems.has_find_in_files() && project_replace_active {
                        if mods.alt && !mods.ctrl {
                            let toggled = match key.as_str() {
                                "r" => {
                                    project_use_regex = !project_use_regex;
                                    true
                                }
                                "w" => {
                                    project_whole_word = !project_whole_word;
                                    true
                                }
                                "i" => {
                                    project_case_insensitive = !project_case_insensitive;
                                    true
                                }
                                _ => false,
                            };
                            if toggled {
                                project_replace_results = run_project_search(
                                    &project_replace_search,
                                    &project_root,
                                    project_use_regex,
                                    project_whole_word,
                                    project_case_insensitive,
                                );
                                project_replace_selected = 0;
                                redraw = true;
                                continue;
                            }
                        }
                        if let Some(is_redo) = keymap_field_undo(&keymap, key.as_str(), mods) {
                            if project_replace_focus_on_replace {
                                let restored = if is_redo {
                                    project_replace_with_history
                                        .redo(&project_replace_with, project_replace_with.len())
                                } else {
                                    project_replace_with_history
                                        .undo(&project_replace_with, project_replace_with.len())
                                };
                                if let Some((t, _)) = restored {
                                    project_replace_with = t;
                                }
                            } else {
                                let restored = if is_redo {
                                    project_replace_search_history
                                        .redo(&project_replace_search, project_replace_search.len())
                                } else {
                                    project_replace_search_history
                                        .undo(&project_replace_search, project_replace_search.len())
                                };
                                if let Some((t, _)) = restored {
                                    project_replace_search = t;
                                    project_replace_results = run_project_search(
                                        &project_replace_search,
                                        &project_root,
                                        project_use_regex,
                                        project_whole_word,
                                        project_case_insensitive,
                                    );
                                    project_replace_selected = 0;
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        if let Some(action) = keymap_field_clipboard(&keymap, key.as_str(), mods) {
                            match action {
                                FieldClipboard::Copy => {
                                    let src = if project_replace_focus_on_replace {
                                        &project_replace_with
                                    } else {
                                        &project_replace_search
                                    };
                                    if !src.is_empty() {
                                        crate::window::set_clipboard_text(src);
                                    }
                                }
                                FieldClipboard::Cut => {
                                    if project_replace_focus_on_replace {
                                        if !project_replace_with.is_empty() {
                                            crate::window::set_clipboard_text(
                                                &project_replace_with,
                                            );
                                            project_replace_with_history.record(
                                                &project_replace_with,
                                                project_replace_with.len(),
                                                FieldEdit::Replace,
                                                buffer::now_secs(),
                                            );
                                            project_replace_with.clear();
                                        }
                                    } else if !project_replace_search.is_empty() {
                                        crate::window::set_clipboard_text(&project_replace_search);
                                        project_replace_search_history.record(
                                            &project_replace_search,
                                            project_replace_search.len(),
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        project_replace_search.clear();
                                        project_replace_results = run_project_search(
                                            &project_replace_search,
                                            &project_root,
                                            project_use_regex,
                                            project_whole_word,
                                            project_case_insensitive,
                                        );
                                        project_replace_selected = 0;
                                    }
                                }
                                FieldClipboard::Paste => {
                                    if let Some(clip) = crate::window::get_clipboard_text() {
                                        if project_replace_focus_on_replace {
                                            project_replace_with_history.record(
                                                &project_replace_with,
                                                project_replace_with.len(),
                                                FieldEdit::Replace,
                                                buffer::now_secs(),
                                            );
                                            append_clipboard_line(&mut project_replace_with, &clip);
                                        } else {
                                            project_replace_search_history.record(
                                                &project_replace_search,
                                                project_replace_search.len(),
                                                FieldEdit::Replace,
                                                buffer::now_secs(),
                                            );
                                            append_clipboard_line(
                                                &mut project_replace_search,
                                                &clip,
                                            );
                                            project_replace_results = run_project_search(
                                                &project_replace_search,
                                                &project_root,
                                                project_use_regex,
                                                project_whole_word,
                                                project_case_insensitive,
                                            );
                                            project_replace_selected = 0;
                                        }
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        match key.as_str() {
                            "escape" => {
                                project_replace_active = false;
                            }
                            "tab" => {
                                project_replace_focus_on_replace =
                                    !project_replace_focus_on_replace;
                            }
                            "return" | "keypad enter" if mods.ctrl
                                // Execute replace all.
                                && !project_replace_search.is_empty() => {
                                    // Run the project-wide sed on a worker
                                    // thread; its count and the doc reload are
                                    // applied from the per-frame poll.
                                    if replace_job.is_none() {
                                        let root = project_root.clone();
                                        let search = project_replace_search.clone();
                                        let with = project_replace_with.clone();
                                        let use_regex = project_use_regex;
                                        let case_insensitive = project_case_insensitive;
                                        replace_job = Some(std::thread::spawn(move || {
                                            execute_project_replace(
                                                &root,
                                                &search,
                                                &with,
                                                use_regex,
                                                case_insensitive,
                                            )
                                        }));
                                        info_message = Some((
                                            "Replacing across project...".to_string(),
                                            Instant::now(),
                                        ));
                                    }
                                    project_replace_active = false;
                                }
                            "return" | "keypad enter"
                                // Preview: run search to show matches.
                                if !project_replace_search.is_empty() => {
                                    project_replace_results = run_project_search(
                                        &project_replace_search,
                                        &project_root,
                                        project_use_regex,
                                        project_whole_word,
                                        project_case_insensitive,
                                    );
                                    project_replace_selected = 0;
                                }
                            "up" => {
                                project_replace_selected =
                                    project_replace_selected.saturating_sub(1);
                            }
                            "down"
                                if !project_replace_results.is_empty() => {
                                    project_replace_selected = (project_replace_selected + 1)
                                        .min(project_replace_results.len() - 1);
                                }
                            "backspace" => {
                                if project_replace_focus_on_replace {
                                    if !project_replace_with.is_empty() {
                                        project_replace_with_history.record(
                                            &project_replace_with,
                                            project_replace_with.len(),
                                            FieldEdit::Delete,
                                            buffer::now_secs(),
                                        );
                                    }
                                    project_replace_with.pop();
                                } else {
                                    if !project_replace_search.is_empty() {
                                        project_replace_search_history.record(
                                            &project_replace_search,
                                            project_replace_search.len(),
                                            FieldEdit::Delete,
                                            buffer::now_secs(),
                                        );
                                    }
                                    project_replace_search.pop();
                                    project_replace_results = run_project_search(
                                        &project_replace_search,
                                        &project_root,
                                        project_use_regex,
                                        project_whole_word,
                                        project_case_insensitive,
                                    );
                                    project_replace_selected = 0;
                                }
                            }
                            _ => {}
                        }
                        redraw = true;
                        continue;
                    }

                    // Code-action picker intercepts keys.
                    if code_action_active {
                        match key.as_str() {
                            "escape" => {
                                code_action_active = false;
                            }
                            "up" => {
                                code_action_selected = code_action_selected.saturating_sub(1);
                            }
                            "down" if !code_actions.is_empty() => {
                                code_action_selected =
                                    (code_action_selected + 1).min(code_actions.len() - 1);
                            }
                            "return" | "keypad enter" => {
                                if let Some((_, action)) =
                                    code_actions.get(code_action_selected).cloned()
                                {
                                    if let Some(reason) = code_action_disabled_reason(&action) {
                                        info_message = Some((reason.to_string(), Instant::now()));
                                    } else if lsp_state.code_action_resolve_provider
                                        && action.get("data").is_some_and(|v| !v.is_null())
                                        && action.get("edit").is_none_or(|v| v.is_null())
                                        && action.get("command").is_none_or(|v| v.is_null())
                                        && let Some(tid) = lsp_state.transport_id
                                    {
                                        let req_id = lsp_state.next_id();
                                        lsp_state
                                            .pending_requests
                                            .insert(req_id, "codeAction/resolve".to_string());
                                        let _ = lsp::send_message(
                                            tid,
                                            &lsp_code_action_resolve_request(req_id, action),
                                        );
                                    } else {
                                        execute_lsp_code_action(
                                            &action,
                                            &mut docs,
                                            &mut lsp_state,
                                            use_git(),
                                            config.files.atomic_save,
                                        );
                                    }
                                }
                                code_action_active = false;
                            }
                            _ => {}
                        }
                        redraw = true;
                        continue;
                    }

                    // Git status view intercepts keys.
                    if subsystems.has_git() && git_status_active {
                        match key.as_str() {
                            "escape" => {
                                git_status_active = false;
                            }
                            "return" | "keypad enter" => {
                                if let Some((_, path, _)) =
                                    git_status_entries.get(git_status_selected).cloned()
                                {
                                    git_status_active = false;
                                    let full_path = format!("{project_root}/{path}");
                                    let tab_idx = docs.iter().position(|d| d.path == full_path);
                                    let idx = if let Some(i) = tab_idx {
                                        i
                                    } else if open_file_into(&full_path, &mut docs, use_git()) {
                                        autoreload.watch(&full_path);
                                        remember_recent_file(
                                            &mut recent_files,
                                            &full_path,
                                            userdir_path,
                                        );
                                        docs.len() - 1
                                    } else {
                                        redraw = true;
                                        continue;
                                    };
                                    active_tab = idx;
                                }
                            }
                            "up" => {
                                git_status_selected = git_status_selected.saturating_sub(1);
                            }
                            "down" if !git_status_entries.is_empty() => {
                                git_status_selected =
                                    (git_status_selected + 1).min(git_status_entries.len() - 1);
                            }
                            "r" | "R" => {
                                if git_status_job.is_none() {
                                    let root = project_root.clone();
                                    git_status_job =
                                        Some(std::thread::spawn(move || run_git_status(&root)));
                                }
                                git_status_selected = 0;
                            }
                            _ => {}
                        }
                        redraw = true;
                        continue;
                    }

                    // Git log view intercepts keys when active.
                    if subsystems.has_git() && git_log_active {
                        match key.as_str() {
                            "escape" => {
                                git_log_active = false;
                            }
                            "up" => {
                                git_log_selected = git_log_selected.saturating_sub(1);
                            }
                            "down" if !git_log_entries.is_empty() => {
                                git_log_selected =
                                    (git_log_selected + 1).min(git_log_entries.len() - 1);
                            }
                            _ => {}
                        }
                        redraw = true;
                        continue;
                    }

                    // Terminal intercepts all keys when focused.
                    if terminal.visible && terminal.focused {
                        if key == "escape" {
                            terminal.focused = false;
                            redraw = true;
                            continue;
                        }
                        // Ctrl+PageUp/PageDown switch terminal tabs.
                        if mods.ctrl && !mods.alt && !mods.shift {
                            match key.as_str() {
                                "pageup" => {
                                    terminal.prev_tab();
                                    redraw = true;
                                    continue;
                                }
                                "pagedown" => {
                                    terminal.next_tab();
                                    redraw = true;
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        // Terminal Ctrl+Shift+A: select every visible cell
                        // so the user can copy the current viewport
                        // (including whatever scrollback is currently
                        // shown) without dragging across it manually. The
                        // gnome-terminal / xterm convention. Plain Ctrl+A
                        // stays as the shell's "move to line start" so
                        // the shell is still usable.
                        if mods.ctrl && mods.shift && !mods.alt && key == "a" {
                            let (_, wh, _, _) = crate::window::get_window_size();
                            let win_h = wh as f64;
                            let status_h_a = style.font_height + style.padding_y * 2.0;
                            let tab_h_a = if !single_file_mode && !docs.is_empty() {
                                style.font_height + style.padding_y * 3.0
                            } else {
                                0.0
                            };
                            let terminal_h_a = (win_h * 0.3)
                                .min(win_h - tab_h_a - status_h_a - 50.0)
                                .max(80.0);
                            let tab_bar_h_a = if !terminal.terminals.is_empty() {
                                style.font_height + style.padding_y * 3.0
                            } else {
                                0.0
                            };
                            let char_h_a = style.code_font_height * 1.2;
                            let rows_visible = (((terminal_h_a
                                - style.divider_size
                                - tab_bar_h_a
                                - style.padding_y * 2.0)
                                / char_h_a)
                                .floor()
                                .max(1.0)) as usize;
                            if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                                inst.sel_start = Some((0, 0));
                                inst.sel_end = Some((rows_visible.saturating_sub(1), usize::MAX));
                                inst.sel_dragging = false;
                            }
                            redraw = true;
                            continue;
                        }
                        // Terminal copy / paste.
                        //   Ctrl+Shift+C  or  Ctrl+Insert : copy selection
                        //   Ctrl+Shift+V  or  Shift+Insert: paste clipboard
                        // Plain Ctrl+C / Ctrl+V remain sent to the shell
                        // (SIGINT / literal, respectively).
                        let is_copy_combo = mods.ctrl
                            && ((mods.shift && key == "c") || (!mods.shift && key == "insert"));
                        let is_paste_combo = mods.shift
                            && ((mods.ctrl && key == "v") || (!mods.ctrl && key == "insert"));
                        if is_copy_combo {
                            if let Some(inst) = terminal.terminals.get(terminal.active) {
                                if let (Some(s), Some(e)) = (inst.sel_start, inst.sel_end) {
                                    if let Some((a, b)) =
                                        crate::editor::terminal_panel::normalized_selection(s, e)
                                    {
                                        let cap = inst.tbuf.history_len() as f64;
                                        let scrollback_rows =
                                            inst.scrollback.round().max(0.0).min(cap) as usize;
                                        // Recompute rows_visible from current geometry.
                                        let (_, wh, _, _) = crate::window::get_window_size();
                                        let win_h = wh as f64;
                                        let status_h_c = style.font_height + style.padding_y * 2.0;
                                        let tab_h_c = if !single_file_mode && !docs.is_empty() {
                                            style.font_height + style.padding_y * 3.0
                                        } else {
                                            0.0
                                        };
                                        let terminal_h_c = (win_h * 0.3)
                                            .min(win_h - tab_h_c - status_h_c - 50.0)
                                            .max(80.0);
                                        let tab_bar_h_c = if !terminal.terminals.is_empty() {
                                            style.font_height + style.padding_y * 3.0
                                        } else {
                                            0.0
                                        };
                                        let char_h_c = style.code_font_height * 1.2;
                                        let rows_visible = (((terminal_h_c
                                            - style.divider_size
                                            - tab_bar_h_c
                                            - style.padding_y * 2.0)
                                            / char_h_c)
                                            .floor()
                                            .max(1.0))
                                            as usize;
                                        let rows_data =
                                            inst.tbuf.visible_rows(rows_visible, scrollback_rows);
                                        let text =
                                            crate::editor::terminal_panel::extract_selection_text(
                                                &rows_data, a, b,
                                            );
                                        if !text.is_empty() {
                                            crate::window::set_clipboard_text(&text);
                                        }
                                    }
                                }
                            }
                            if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                                inst.sel_start = None;
                                inst.sel_end = None;
                                inst.sel_dragging = false;
                            }
                            redraw = true;
                            continue;
                        }
                        if is_paste_combo {
                            if let Some(text) = crate::window::get_clipboard_text() {
                                if let Some(inst) = terminal.active_terminal() {
                                    let _ = inst.inner.write(text.as_bytes());
                                    inst.scrollback = 0.0;
                                    inst.scrollback_target = 0.0;
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        if let Some(inst) = terminal.active_terminal() {
                            let data = match key.as_str() {
                                "return" | "keypad enter" => Some(b"\r".to_vec()),
                                "backspace" => Some(vec![0x7f]),
                                "tab" => Some(b"\t".to_vec()),
                                "up" => Some(b"\x1b[A".to_vec()),
                                "down" => Some(b"\x1b[B".to_vec()),
                                "right" => Some(b"\x1b[C".to_vec()),
                                "left" => Some(b"\x1b[D".to_vec()),
                                "delete" => Some(b"\x1b[3~".to_vec()),
                                "home" => Some(b"\x1b[H".to_vec()),
                                "end" => Some(b"\x1b[F".to_vec()),
                                _ => {
                                    if key.len() == 1 {
                                        let ch = key.as_bytes()[0];
                                        if mods.ctrl {
                                            // Ctrl+letter -> control char.
                                            let ctrl = ch & 0x1f;
                                            Some(vec![ctrl])
                                        } else {
                                            None // Handled by TextInput
                                        }
                                    } else {
                                        None
                                    }
                                }
                            };
                            if let Some(bytes) = data {
                                let _ = inst.inner.write(&bytes);
                                // Snap to live bottom so the caret is visible.
                                inst.scrollback = 0.0;
                                inst.scrollback_target = 0.0;
                            }
                        }
                        redraw = true;
                        continue;
                    }

                    // Dismiss info message on any key.
                    if info_message.is_some() {
                        info_message = None;
                        redraw = true;
                        if key == "escape" {
                            continue;
                        }
                    }

                    // "No extension detected, save anyway?" prompt. Yes runs
                    // the overwrite check next (and the save if that
                    // passes); No just dismisses the nag so the user can
                    // type `.ext` in the picker and press Enter again.
                    if let Nag::NoExtension { save_path, doc_tab } = &nag {
                        let save_path = save_path.clone();
                        let tab = *doc_tab;
                        eat_next_text_input = true;
                        match key.as_str() {
                            "y" | "Y" | "return" | "keypad enter" => {
                                // Chain into the overwrite path: if the file
                                // exists and isn't the current doc's own
                                // path, hand off to OverwriteFile; otherwise
                                // perform the save directly.
                                let own_path = docs
                                    .get(tab)
                                    .map(|d| d.path.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if std::path::Path::new(&save_path).is_file()
                                    && save_path != own_path
                                {
                                    nag = Nag::OverwriteFile {
                                        save_path,
                                        doc_tab: tab,
                                    };
                                    redraw = true;
                                    continue;
                                }
                                if let Some(doc) = docs.get_mut(tab) {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let atomic = config.files.atomic_save;
                                        let saved_id = buffer::with_buffer(buf_id, |b| {
                                            buffer::save_file(b, &save_path, b.crlf, atomic)
                                                .map_err(|_| buffer::BufferError::UnknownBuffer)?;
                                            Ok(b.change_id)
                                        });
                                        if let Ok(id) = saved_id {
                                            doc.saved_change_id = id;
                                            doc.saved_signature =
                                                buffer::with_buffer(buf_id, |b| {
                                                    Ok(buffer::content_signature(&b.lines))
                                                })
                                                .unwrap_or(0);
                                            doc.path = save_path.clone();
                                            doc.name = std::path::Path::new(&save_path)
                                                .file_name()
                                                .map(|n| n.to_string_lossy().to_string())
                                                .unwrap_or_else(|| save_path.clone());
                                            doc.cached_change_id = -1;
                                            doc.cached_render = std::sync::Arc::new(Vec::new());
                                            autoreload.watch(&save_path);
                                            log_to_file(userdir, &format!("Saved {save_path}"));
                                            info_message = Some((
                                                format!("Saved {}", doc.name),
                                                Instant::now(),
                                            ));
                                        } else {
                                            info_message = Some((
                                                format!("Failed to save {save_path}"),
                                                Instant::now(),
                                            ));
                                        }
                                    }
                                }
                                nag = Nag::None;
                                cmdview_active = false;
                                redraw = true;
                                continue;
                            }
                            "n" | "N" | "escape" => {
                                nag = Nag::None;
                                redraw = true;
                                continue;
                            }
                            _ => {
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    // "Delete FILE?" prompt intercepts keys. Yes removes the
                    // file from disk and any open tab; No dismisses.
                    if let Nag::DeleteFile { path } = &nag {
                        let target = path.clone();
                        eat_next_text_input = true;
                        match key.as_str() {
                            "y" | "Y" | "return" | "keypad enter" => {
                                match std::fs::remove_file(&target) {
                                    Ok(()) => {
                                        autoreload.unwatch(&target);
                                        let mut i = 0;
                                        while i < docs.len() {
                                            if docs[i].path == target {
                                                docs.remove(i);
                                                if active_tab >= docs.len() && !docs.is_empty() {
                                                    active_tab = docs.len() - 1;
                                                } else if docs.is_empty() {
                                                    active_tab = 0;
                                                } else if i < active_tab {
                                                    active_tab = active_tab.saturating_sub(1);
                                                }
                                            } else {
                                                i += 1;
                                            }
                                        }
                                        if subsystems.has_sidebar() && !project_root.is_empty() {
                                            let in_memory_expanded: HashSet<String> =
                                                sidebar_entries
                                                    .iter()
                                                    .filter(|e| e.is_dir && e.expanded)
                                                    .map(|e| e.path.clone())
                                                    .collect();
                                            sidebar_entries = scan_for_sidebar(
                                                subsystems.has_notes_mode(),
                                                &project_root,
                                                sidebar_show_hidden,
                                            );
                                            restore_expanded_folders(
                                                &mut sidebar_entries,
                                                userdir_path,
                                                sidebar_show_hidden,
                                                &project_session_key(&project_root),
                                            );
                                            expand_sidebar_from_set(
                                                &mut sidebar_entries,
                                                &in_memory_expanded,
                                                sidebar_show_hidden,
                                            );
                                        }
                                        info_message =
                                            Some((format!("Deleted {target}"), Instant::now()));
                                    }
                                    Err(e) => {
                                        info_message =
                                            Some((format!("Delete failed: {e}"), Instant::now()));
                                    }
                                }
                                nag = Nag::None;
                                redraw = true;
                                continue;
                            }
                            "n" | "N" | "escape" => {
                                nag = Nag::None;
                                redraw = true;
                                continue;
                            }
                            _ => {
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    // "Overwrite FILE?" prompt intercepts keys. Yes writes
                    // over the existing file; No returns to the Save As
                    // picker so the user can adjust the filename. Escape /N
                    // just dismisses the nag (keeps cmdview open).
                    if let Nag::OverwriteFile { save_path, doc_tab } = &nag {
                        let save_path = save_path.clone();
                        let tab = *doc_tab;
                        eat_next_text_input = true;
                        match key.as_str() {
                            "y" | "Y" | "return" | "keypad enter" => {
                                if let Some(doc) = docs.get_mut(tab) {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let atomic = config.files.atomic_save;
                                        let saved_id = buffer::with_buffer(buf_id, |b| {
                                            buffer::save_file(b, &save_path, b.crlf, atomic)
                                                .map_err(|_| buffer::BufferError::UnknownBuffer)?;
                                            Ok(b.change_id)
                                        });
                                        if let Ok(id) = saved_id {
                                            doc.saved_change_id = id;
                                            doc.saved_signature =
                                                buffer::with_buffer(buf_id, |b| {
                                                    Ok(buffer::content_signature(&b.lines))
                                                })
                                                .unwrap_or(0);
                                            doc.path = save_path.clone();
                                            doc.name = std::path::Path::new(&save_path)
                                                .file_name()
                                                .map(|n| n.to_string_lossy().to_string())
                                                .unwrap_or_else(|| save_path.clone());
                                            doc.cached_change_id = -1;
                                            doc.cached_render = std::sync::Arc::new(Vec::new());
                                            autoreload.watch(&save_path);
                                            log_to_file(userdir, &format!("Saved {save_path}"));
                                            info_message = Some((
                                                format!("Saved {}", doc.name),
                                                Instant::now(),
                                            ));
                                        } else {
                                            info_message = Some((
                                                format!("Failed to save {save_path}"),
                                                Instant::now(),
                                            ));
                                        }
                                    }
                                }
                                nag = Nag::None;
                                cmdview_active = false;
                                redraw = true;
                                continue;
                            }
                            "n" | "N" | "escape" => {
                                // Back off to the picker — cmdview stays
                                // open with the text the user typed so they
                                // can rename.
                                nag = Nag::None;
                                redraw = true;
                                continue;
                            }
                            _ => {
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    // "Create missing directory?" prompt intercepts keys when
                    // active. Yes creates the parent tree and performs the
                    // pending save; No backs off without writing. Escape /N
                    // also closes the originating Save As picker so the user
                    // is not left staring at it.
                    if let Nag::CreateDir {
                        parent: parent_str,
                        save_path,
                        doc_tab,
                        from_save_as,
                    } = &nag
                    {
                        let save_path = save_path.clone();
                        let parent_str = parent_str.clone();
                        let tab = *doc_tab;
                        let is_save_as = *from_save_as;
                        eat_next_text_input = true;
                        match key.as_str() {
                            "y" | "Y" | "return" | "keypad enter" => {
                                let parent = std::path::Path::new(&save_path)
                                    .parent()
                                    .map(|p| p.to_path_buf());
                                let create_ok = match parent {
                                    Some(p) => std::fs::create_dir_all(&p).is_ok(),
                                    None => true,
                                };
                                if !create_ok {
                                    info_message = Some((
                                        format!("Could not create directory {parent_str}"),
                                        Instant::now(),
                                    ));
                                    nag = Nag::None;
                                    if is_save_as {
                                        cmdview_active = false;
                                    }
                                    redraw = true;
                                    continue;
                                }
                                if let Some(doc) = docs.get_mut(tab) {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let atomic = config.files.atomic_save;
                                        let saved_id = buffer::with_buffer(buf_id, |b| {
                                            buffer::save_file(b, &save_path, b.crlf, atomic)
                                                .map_err(|_| buffer::BufferError::UnknownBuffer)?;
                                            Ok(b.change_id)
                                        });
                                        if let Ok(id) = saved_id {
                                            doc.saved_change_id = id;
                                            doc.saved_signature =
                                                buffer::with_buffer(buf_id, |b| {
                                                    Ok(buffer::content_signature(&b.lines))
                                                })
                                                .unwrap_or(0);
                                            if is_save_as {
                                                doc.path = save_path.clone();
                                                doc.name = std::path::Path::new(&save_path)
                                                    .file_name()
                                                    .map(|n| n.to_string_lossy().to_string())
                                                    .unwrap_or_else(|| save_path.clone());
                                                doc.cached_change_id = -1;
                                                doc.cached_render = std::sync::Arc::new(Vec::new());
                                            }
                                            autoreload.watch(&save_path);
                                            log_to_file(userdir, &format!("Saved {save_path}"));
                                            info_message = Some((
                                                format!("Saved {}", doc.name),
                                                Instant::now(),
                                            ));
                                            if !is_save_as && subsystems.has_git() {
                                                // Diff off the UI thread; the
                                                // gutter fills in via drain_diffs.
                                                crate::editor::git::start_diff(&save_path);
                                            }
                                        } else {
                                            info_message = Some((
                                                format!("Failed to save {save_path}"),
                                                Instant::now(),
                                            ));
                                        }
                                    }
                                }
                                nag = Nag::None;
                                if is_save_as {
                                    cmdview_active = false;
                                }
                                redraw = true;
                                continue;
                            }
                            "n" | "N" | "escape" => {
                                nag = Nag::None;
                                if is_save_as {
                                    cmdview_active = false;
                                }
                                redraw = true;
                                continue;
                            }
                            _ => {
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    // Nag view intercepts keys when active; dismiss any overlay.
                    if let Nag::UnsavedChanges { tab_to_close, .. } = &nag {
                        let tab_to_close = *tab_to_close;
                        cmdview_active = false;
                        palette_active = false;
                        eat_next_text_input = true;
                        match key.as_str() {
                            "y" | "Y" | "return" | "keypad enter" => {
                                // Yes: discard unsaved changes and proceed.
                                if let Some(idx) = tab_to_close {
                                    if let Some(d) = docs.get(idx) {
                                        autoreload.unwatch(&d.path);
                                    }
                                    docs.remove(idx);
                                    if docs.is_empty() {
                                        active_tab = 0;
                                    } else if idx <= active_tab {
                                        active_tab = active_tab.saturating_sub(1);
                                    }
                                } else {
                                    quit = true;
                                }
                                nag = Nag::None;
                                redraw = true;
                                continue;
                            }
                            "n" | "N" | "escape" => {
                                // No / Cancel: leave everything as-is.
                                nag = Nag::None;
                                redraw = true;
                                continue;
                            }
                            _ => {
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    // Reload nag intercepts keys when active.
                    if let Nag::ReloadFromDisk { path } = &nag {
                        let rpath = path.clone();
                        // Every arm here resolves the keystroke, so swallow
                        // the follow-on TextInput regardless of which arm
                        // matches.
                        eat_next_text_input = true;
                        match key.as_str() {
                            "y" | "Y" => {
                                // Reload from disk.
                                if let Some(doc) = docs.iter_mut().find(|d| d.path == rpath) {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let _ = buffer::with_buffer_mut(buf_id, |b| {
                                            let mut buf_state = buffer::default_buffer_state();
                                            if buffer::load_file(&mut buf_state, &rpath).is_ok() {
                                                b.lines = buf_state.lines;
                                                // See autoreload path: bump change_id past
                                                // its current value so the render cache
                                                // doesn't hit on the stale lines.
                                                b.change_id = b.change_id.wrapping_add(1).max(1);
                                            }
                                            Ok(())
                                        });
                                        doc.cached_change_id = -1;
                                        doc.cached_render = std::sync::Arc::new(Vec::new());
                                        crate::window::force_invalidate();
                                        if let Ok((cid, sig)) = buffer::with_buffer(buf_id, |b| {
                                            Ok((b.change_id, buffer::content_signature(&b.lines)))
                                        }) {
                                            doc.saved_change_id = cid;
                                            doc.saved_signature = sig;
                                        }
                                    }
                                }
                                nag = Nag::None;
                                redraw = true;
                                continue;
                            }
                            "n" | "N" | "escape" => {
                                nag = Nag::None;
                                redraw = true;
                                continue;
                            }
                            _ => {
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    // Command palette intercepts keys when active.
                    if palette_active {
                        if let Some(is_redo) = keymap_field_undo(&keymap, key.as_str(), mods) {
                            let restored = if is_redo {
                                palette_history.redo(&palette_query, palette_query.len())
                            } else {
                                palette_history.undo(&palette_query, palette_query.len())
                            };
                            if let Some((t, _)) = restored {
                                palette_query = t;
                                palette_results =
                                    fuzzy_filter_commands(&palette_query, &all_commands);
                                palette_selected =
                                    palette_selected.min(palette_results.len().saturating_sub(1));
                            }
                            redraw = true;
                            continue;
                        }
                        if let Some(action) = keymap_field_clipboard(&keymap, key.as_str(), mods) {
                            match action {
                                FieldClipboard::Copy => {
                                    if !palette_query.is_empty() {
                                        crate::window::set_clipboard_text(&palette_query);
                                    }
                                }
                                FieldClipboard::Cut => {
                                    if !palette_query.is_empty() {
                                        crate::window::set_clipboard_text(&palette_query);
                                        palette_history.record(
                                            &palette_query,
                                            palette_query.len(),
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        palette_query.clear();
                                        palette_results =
                                            fuzzy_filter_commands(&palette_query, &all_commands);
                                        palette_selected = palette_selected
                                            .min(palette_results.len().saturating_sub(1));
                                    }
                                }
                                FieldClipboard::Paste => {
                                    if let Some(clip) = crate::window::get_clipboard_text() {
                                        palette_history.record(
                                            &palette_query,
                                            palette_query.len(),
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        append_clipboard_line(&mut palette_query, &clip);
                                        palette_results =
                                            fuzzy_filter_commands(&palette_query, &all_commands);
                                        palette_selected = palette_selected
                                            .min(palette_results.len().saturating_sub(1));
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        match key.as_str() {
                            "escape" => {
                                palette_active = false;
                                redraw = true;
                                continue;
                            }
                            "return" | "keypad enter" => {
                                if let Some((cmd, _)) = palette_results.get(palette_selected) {
                                    let cmd = cmd.clone();
                                    palette_active = false;
                                    // If the selected item is a file path, open it.
                                    if cmd.starts_with('/') && std::path::Path::new(&cmd).is_file()
                                    {
                                        if open_file_into(&cmd, &mut docs, use_git()) {
                                            active_tab = docs.len() - 1;
                                            autoreload.watch(&cmd);
                                            remember_recent_file(
                                                &mut recent_files,
                                                &cmd,
                                                userdir_path,
                                            );
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    // Execute the selected command.
                                    {
                                        let cmd: String = cmd;
                                        include!("commands_dispatch.rs");
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                            "backspace" => {
                                if !palette_query.is_empty() {
                                    palette_history.record(
                                        &palette_query,
                                        palette_query.len(),
                                        FieldEdit::Delete,
                                        buffer::now_secs(),
                                    );
                                }
                                palette_query.pop();
                            }
                            "up" => {
                                palette_selected = palette_selected.saturating_sub(1);
                            }
                            "down" => {
                                if palette_selected + 1 < palette_results.len() {
                                    palette_selected += 1;
                                }
                            }
                            _ => {
                                continue;
                            }
                        }
                        // Filter commands with fuzzy matching.
                        palette_results = fuzzy_filter_commands(&palette_query, &all_commands);
                        palette_selected =
                            palette_selected.min(palette_results.len().saturating_sub(1));
                        redraw = true;
                        continue;
                    }

                    // Find bar intercepts keys when active.
                    if find_active {
                        // Alt-chorded toggles apply regardless of which input has focus.
                        if mods.alt && !mods.ctrl {
                            let toggled = match key.as_str() {
                                "r" => {
                                    find_use_regex = !find_use_regex;
                                    true
                                }
                                "w" => {
                                    find_whole_word = !find_whole_word;
                                    true
                                }
                                "i" => {
                                    find_case_insensitive = !find_case_insensitive;
                                    true
                                }
                                "s" => {
                                    find_in_selection = !find_in_selection;
                                    if find_in_selection && find_selection_range.is_none() {
                                        // Capture current selection if we don't already have one.
                                        if let Some(doc) = docs.get(active_tab) {
                                            let a = doc_anchor(&doc.view);
                                            let c = doc_cursor(&doc.view);
                                            if a.0 != c.0 {
                                                let (sl, sc) = if a < c { a } else { c };
                                                let (el, ec) = if a < c { c } else { a };
                                                find_selection_range = Some((sl, sc, el, ec));
                                            } else {
                                                // Single-line selection; not meaningful for
                                                // find-in-selection. Disable again.
                                                find_in_selection = false;
                                            }
                                        }
                                    }
                                    true
                                }
                                _ => false,
                            };
                            if toggled {
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    let dv = &mut doc.view;
                                    let sel = if find_in_selection {
                                        find_selection_range
                                    } else {
                                        None
                                    };
                                    find_matches = compute_find_matches_filtered(
                                        dv,
                                        &find_query,
                                        find_use_regex,
                                        find_whole_word,
                                        find_case_insensitive,
                                        sel,
                                    );
                                    find_current = find_match_at_or_after(
                                        &find_matches,
                                        find_anchor.0,
                                        find_anchor.1,
                                    );
                                    if let Some(i) = find_current {
                                        select_find_match(dv, find_matches[i], replace_active);
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                        }
                        if let Some(is_redo) = keymap_field_undo(&keymap, key.as_str(), mods) {
                            // Route the undo/redo bindings to the focused field
                            // instead of letting them leak through to the document.
                            if find_focus_on_replace {
                                let restored = if is_redo {
                                    replace_history.redo(&replace_query, replace_query.len())
                                } else {
                                    replace_history.undo(&replace_query, replace_query.len())
                                };
                                if let Some((t, _)) = restored {
                                    replace_query = t;
                                }
                            } else {
                                let restored = if is_redo {
                                    find_history.redo(&find_query, find_query.len())
                                } else {
                                    find_history.undo(&find_query, find_query.len())
                                };
                                if let Some((t, _)) = restored {
                                    find_query = t;
                                    if let Some(doc) = docs.get_mut(active_tab) {
                                        let dv = &mut doc.view;
                                        let sel = if find_in_selection {
                                            find_selection_range
                                        } else {
                                            None
                                        };
                                        find_matches = compute_find_matches_filtered(
                                            dv,
                                            &find_query,
                                            find_use_regex,
                                            find_whole_word,
                                            find_case_insensitive,
                                            sel,
                                        );
                                        find_current = find_match_at_or_after(
                                            &find_matches,
                                            find_anchor.0,
                                            find_anchor.1,
                                        );
                                        if let Some(i) = find_current {
                                            select_find_match(dv, find_matches[i], replace_active);
                                        }
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        if let Some(action) = keymap_field_clipboard(&keymap, key.as_str(), mods) {
                            // Clipboard ops belong to the find bar while it holds
                            // focus, not the document.
                            match action {
                                FieldClipboard::Copy => {
                                    let src = if find_focus_on_replace {
                                        &replace_query
                                    } else {
                                        &find_query
                                    };
                                    if !src.is_empty() {
                                        crate::window::set_clipboard_text(src);
                                    }
                                }
                                FieldClipboard::Cut => {
                                    if find_focus_on_replace {
                                        if !replace_query.is_empty() {
                                            crate::window::set_clipboard_text(&replace_query);
                                            replace_history.record(
                                                &replace_query,
                                                replace_query.len(),
                                                FieldEdit::Replace,
                                                buffer::now_secs(),
                                            );
                                            replace_query.clear();
                                        }
                                    } else if !find_query.is_empty() {
                                        crate::window::set_clipboard_text(&find_query);
                                        find_history.record(
                                            &find_query,
                                            find_query.len(),
                                            FieldEdit::Replace,
                                            buffer::now_secs(),
                                        );
                                        find_query.clear();
                                        if let Some(doc) = docs.get_mut(active_tab) {
                                            let dv = &mut doc.view;
                                            let sel = if find_in_selection {
                                                find_selection_range
                                            } else {
                                                None
                                            };
                                            find_matches = compute_find_matches_filtered(
                                                dv,
                                                &find_query,
                                                find_use_regex,
                                                find_whole_word,
                                                find_case_insensitive,
                                                sel,
                                            );
                                            find_current = find_match_at_or_after(
                                                &find_matches,
                                                find_anchor.0,
                                                find_anchor.1,
                                            );
                                            if let Some(i) = find_current {
                                                select_find_match(
                                                    dv,
                                                    find_matches[i],
                                                    replace_active,
                                                );
                                            }
                                        }
                                    }
                                }
                                FieldClipboard::Paste => {
                                    if let Some(clip) = crate::window::get_clipboard_text() {
                                        if find_focus_on_replace {
                                            replace_history.record(
                                                &replace_query,
                                                replace_query.len(),
                                                FieldEdit::Replace,
                                                buffer::now_secs(),
                                            );
                                            append_clipboard_line(&mut replace_query, &clip);
                                        } else {
                                            find_history.record(
                                                &find_query,
                                                find_query.len(),
                                                FieldEdit::Replace,
                                                buffer::now_secs(),
                                            );
                                            append_clipboard_line(&mut find_query, &clip);
                                            if let Some(doc) = docs.get_mut(active_tab) {
                                                let dv = &mut doc.view;
                                                let sel = if find_in_selection {
                                                    find_selection_range
                                                } else {
                                                    None
                                                };
                                                find_matches = compute_find_matches_filtered(
                                                    dv,
                                                    &find_query,
                                                    find_use_regex,
                                                    find_whole_word,
                                                    find_case_insensitive,
                                                    sel,
                                                );
                                                find_current = find_match_at_or_after(
                                                    &find_matches,
                                                    find_anchor.0,
                                                    find_anchor.1,
                                                );
                                                if let Some(i) = find_current {
                                                    select_find_match(
                                                        dv,
                                                        find_matches[i],
                                                        replace_active,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
                        match key.as_str() {
                            "escape" => {
                                find_active = false;
                                replace_active = false;
                                find_focus_on_replace = false;
                                // Free the resident full-document search subject
                                // now that the find bar is closed; it is rebuilt
                                // lazily on the next search.
                                if let Some(doc) = docs.get(active_tab) {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let _ = buffer::with_buffer_mut(buf_id, |b| {
                                            b.search_cache = None;
                                            Ok(())
                                        });
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                            "tab" if replace_active => {
                                find_focus_on_replace = !find_focus_on_replace;
                                redraw = true;
                                continue;
                            }
                            "f3" => {
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    let dv = &mut doc.view;
                                    if !find_matches.is_empty() {
                                        let idx = if mods.shift {
                                            let (al, ac) = doc_anchor(dv);
                                            find_match_before(&find_matches, al, ac)
                                                .unwrap_or(find_matches.len() - 1)
                                        } else {
                                            let (cl, cc) = doc_cursor(dv);
                                            find_match_at_or_after(&find_matches, cl, cc)
                                                .unwrap_or(0)
                                        };
                                        find_current = Some(idx);
                                        select_find_match(dv, find_matches[idx], replace_active);
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                            "return" | "keypad enter"
                                if mods.ctrl && mods.shift && replace_active =>
                            {
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    let dv = &mut doc.view;
                                    let mut count = 0usize;
                                    loop {
                                        let sel = if find_in_selection {
                                            find_selection_range
                                        } else {
                                            None
                                        };
                                        let matches = compute_find_matches_filtered(
                                            dv,
                                            &find_query,
                                            find_use_regex,
                                            find_whole_word,
                                            find_case_insensitive,
                                            sel,
                                        );
                                        if matches.is_empty() {
                                            break;
                                        }
                                        select_find_match(dv, matches[0], replace_active);
                                        replace_current_match(dv, &find_query, &replace_query);
                                        count += 1;
                                        if count > 100_000 {
                                            break;
                                        }
                                    }
                                    find_matches.clear();
                                    find_current = None;
                                    info_message = Some((
                                        format!("Replaced {count} occurrence(s)"),
                                        Instant::now(),
                                    ));
                                }
                                redraw = true;
                                continue;
                            }
                            "return" | "keypad enter"
                                if mods.ctrl && !mods.shift && replace_active =>
                            {
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    let dv = &mut doc.view;
                                    replace_current_match(dv, &find_query, &replace_query);
                                    let sel = if find_in_selection {
                                        find_selection_range
                                    } else {
                                        None
                                    };
                                    find_matches = compute_find_matches_filtered(
                                        dv,
                                        &find_query,
                                        find_use_regex,
                                        find_whole_word,
                                        find_case_insensitive,
                                        sel,
                                    );
                                    if !find_matches.is_empty() {
                                        let (cl, cc) = doc_cursor(dv);
                                        let idx = find_match_at_or_after(&find_matches, cl, cc)
                                            .unwrap_or(0);
                                        find_current = Some(idx);
                                        select_find_match(dv, find_matches[idx], replace_active);
                                    } else {
                                        find_current = None;
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                            "r" if mods.alt && replace_active => {
                                // Alt+R: replace current match (NoteSquirrel parity).
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    let dv = &mut doc.view;
                                    replace_current_match(dv, &find_query, &replace_query);
                                    let sel = if find_in_selection {
                                        find_selection_range
                                    } else {
                                        None
                                    };
                                    find_matches = compute_find_matches_filtered(
                                        dv,
                                        &find_query,
                                        find_use_regex,
                                        find_whole_word,
                                        find_case_insensitive,
                                        sel,
                                    );
                                    if !find_matches.is_empty() {
                                        let (cl, cc) = doc_cursor(dv);
                                        let idx = find_match_at_or_after(&find_matches, cl, cc)
                                            .unwrap_or(0);
                                        find_current = Some(idx);
                                        select_find_match(dv, find_matches[idx], replace_active);
                                    } else {
                                        find_current = None;
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                            "a" if mods.alt && replace_active => {
                                // Alt+A: replace all matches (NoteSquirrel parity).
                                // Drives `replace_current_match` in a loop
                                // since lite-anvil doesn't have a separate
                                // bulk-replace primitive for the in-buffer
                                // find bar.
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    let dv = &mut doc.view;
                                    let mut count = 0usize;
                                    loop {
                                        let sel = if find_in_selection {
                                            find_selection_range
                                        } else {
                                            None
                                        };
                                        let matches = compute_find_matches_filtered(
                                            dv,
                                            &find_query,
                                            find_use_regex,
                                            find_whole_word,
                                            find_case_insensitive,
                                            sel,
                                        );
                                        if matches.is_empty() {
                                            break;
                                        }
                                        select_find_match(dv, matches[0], replace_active);
                                        replace_current_match(dv, &find_query, &replace_query);
                                        count += 1;
                                        if count > 100_000 {
                                            break;
                                        }
                                    }
                                    find_matches.clear();
                                    find_current = None;
                                    info_message = Some((
                                        format!("Replaced {count} occurrence(s)"),
                                        Instant::now(),
                                    ));
                                }
                                redraw = true;
                                continue;
                            }
                            "return" | "keypad enter" => {
                                // Shift+Enter = previous, Enter = next.
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    let dv = &mut doc.view;
                                    if !find_matches.is_empty() {
                                        let idx = if mods.shift {
                                            let (al, ac) = doc_anchor(dv);
                                            find_match_before(&find_matches, al, ac)
                                                .unwrap_or(find_matches.len() - 1)
                                        } else {
                                            let (cl, cc) = doc_cursor(dv);
                                            find_match_at_or_after(&find_matches, cl, cc)
                                                .unwrap_or(0)
                                        };
                                        find_current = Some(idx);
                                        select_find_match(dv, find_matches[idx], replace_active);
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                            "backspace" => {
                                if find_focus_on_replace {
                                    if !replace_query.is_empty() {
                                        replace_history.record(
                                            &replace_query,
                                            replace_query.len(),
                                            FieldEdit::Delete,
                                            buffer::now_secs(),
                                        );
                                    }
                                    replace_query.pop();
                                } else {
                                    if !find_query.is_empty() {
                                        find_history.record(
                                            &find_query,
                                            find_query.len(),
                                            FieldEdit::Delete,
                                            buffer::now_secs(),
                                        );
                                    }
                                    find_query.pop();
                                    if let Some(doc) = docs.get_mut(active_tab) {
                                        let dv = &mut doc.view;
                                        let sel = if find_in_selection {
                                            find_selection_range
                                        } else {
                                            None
                                        };
                                        find_matches = compute_find_matches_filtered(
                                            dv,
                                            &find_query,
                                            find_use_regex,
                                            find_whole_word,
                                            find_case_insensitive,
                                            sel,
                                        );
                                        find_current = find_match_at_or_after(
                                            &find_matches,
                                            find_anchor.0,
                                            find_anchor.1,
                                        );
                                        if let Some(i) = find_current {
                                            select_find_match(dv, find_matches[i], replace_active);
                                        }
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                            _ => {
                                // Unhandled keys (Home, End, arrow keys, page
                                // up/down, etc.) fall through to the main keymap
                                // dispatch so doc navigation keeps working while
                                // the find bar is visible. Bare letters reach
                                // the keymap with no binding and become no-ops;
                                // the paired TextInput event still appends them
                                // to the find query input below.
                            }
                        }
                    }

                    // Insert key toggles overwrite mode.
                    if key == "insert" && !mods.ctrl && !mods.alt && !mods.shift {
                        overwrite_mode = !overwrite_mode;
                        redraw = true;
                        continue;
                    }

                    // Direct Ctrl+=/- handling (SDL key names vary by platform).
                    if mods.ctrl && !mods.alt && !mods.shift {
                        let scale_cmd = match key.as_str() {
                            "=" | "+" | "equals" | "keypad +" => Some("scale:increase"),
                            "-" | "minus" | "keypad -" => Some("scale:decrease"),
                            "0" | "keypad 0" => Some("scale:reset"),
                            _ => None,
                        };
                        if let Some(cmd) = scale_cmd {
                            let current_logical = config.fonts.ui.size as i32;
                            let new_logical = match cmd {
                                "scale:increase" => (current_logical + 1).min(48),
                                "scale:decrease" => (current_logical - 1).max(6),
                                _ => 15, // reset
                            };
                            let new_size = new_logical as f32 * display_scale as f32;
                            let mut new_config = config.clone();
                            new_config.fonts.ui.size = new_logical as u32;
                            new_config.fonts.code.size = new_logical as u32;
                            if let Ok(new_ctx) = load_fonts(&new_config) {
                                config = new_config.clone();
                                draw_ctx = new_ctx;
                                style = build_style(&config, &draw_ctx);
                                style.scale = display_scale;
                                style.padding_x *= display_scale;
                                style.padding_y *= display_scale;
                                style.divider_size = (style.divider_size * display_scale).ceil();
                                style.scrollbar_size *= display_scale;
                                style.caret_width = (style.caret_width * display_scale).ceil();
                                style.tab_width *= display_scale;
                                let tp = Path::new(datadir)
                                    .join("assets")
                                    .join("themes")
                                    .join(format!("{}.json", config.theme));
                                if let Ok(palette) =
                                    crate::editor::style::load_theme_palette(&tp.to_string_lossy())
                                {
                                    apply_theme_to_style(&mut style, &palette);
                                }
                                crate::editor::style_ctx::set_current_style(style.clone());
                                let _ = crate::editor::storage::save_text(
                                    userdir_path,
                                    "session",
                                    "font_size",
                                    &new_size.to_string(),
                                );
                            }
                            redraw = true;
                            continue;
                        }
                    }

                    // Direct Ctrl+` handling for terminal toggle.
                    if subsystems.has_terminal() {
                        if mods.ctrl
                            && !mods.alt
                            && !mods.shift
                            && (key == "`" || key == "grave" || key == "backquote")
                        {
                            terminal.visible = !terminal.visible;
                            if terminal.visible && terminal.terminals.is_empty() {
                                let active_doc_path =
                                    docs.get(active_tab).map(|d| d.path.as_str()).unwrap_or("");
                                let cwd = crate::editor::terminal_panel::resolve_terminal_cwd(
                                    active_doc_path,
                                    &project_root,
                                );
                                if terminal.spawn(&cwd) {
                                    let n = terminal.terminals.len();
                                    let cd_payload =
                                        crate::editor::terminal_panel::terminal_cd_payload(&cwd);
                                    if let Some(t) = terminal.active_terminal() {
                                        t.title =
                                            crate::editor::terminal_panel::terminal_title(n, &cwd);
                                        let _ = t.inner.write(cd_payload.as_bytes());
                                    }
                                }
                            }
                            terminal.focused = terminal.visible;
                            redraw = true;
                            continue;
                        }

                        // Direct Ctrl+Shift+T for new terminal.
                        if mods.ctrl && mods.shift && !mods.alt && key == "t" {
                            let active_doc_path =
                                docs.get(active_tab).map(|d| d.path.as_str()).unwrap_or("");
                            let cwd = crate::editor::terminal_panel::resolve_terminal_cwd(
                                active_doc_path,
                                &project_root,
                            );
                            let ok = terminal.spawn(&cwd);
                            if ok {
                                let n = terminal.terminals.len();
                                let cd_payload =
                                    crate::editor::terminal_panel::terminal_cd_payload(&cwd);
                                if let Some(t) = terminal.active_terminal() {
                                    t.title =
                                        crate::editor::terminal_panel::terminal_title(n, &cwd);
                                    let _ = t.inner.write(cd_payload.as_bytes());
                                }
                            }
                            redraw = true;
                            continue;
                        }
                    }

                    // Markdown preview keyboard scrolling. While the pane
                    // holds focus the navigation keys scroll it, Escape hands
                    // focus back to the editor, and the keys that would type
                    // or erase are swallowed so the pane stays read-only.
                    if let Some(doc) = docs.get_mut(active_tab)
                        && doc.preview.enabled
                        && doc.preview.focused
                    {
                        if key == "escape" {
                            doc.preview.focused = false;
                            redraw = true;
                            continue;
                        }
                        if crate::editor::markdown_preview::scroll_key(
                            &mut doc.preview,
                            key,
                            &style,
                        ) || crate::editor::markdown_preview::swallows_edit_key(key, &mods)
                        {
                            redraw = true;
                            continue;
                        }
                    }

                    if let Some(cmds) = keymap.on_key_pressed(key, mods) {
                        for cmd in Vec::from(cmds) {
                            {
                                let cmd: String = cmd;
                                include!("commands_dispatch.rs");
                            }
                        }
                    }
                    redraw = true;
                }
                EditorEvent::TextInput(text) => {
                    cursor_blink_reset = Instant::now();
                    // The KeyDown handler already consumed this key
                    // (e.g. Y / N resolving a nag); drop the paired
                    // TextInput so it can't land in the document.
                    if eat_next_text_input {
                        eat_next_text_input = false;
                        redraw = true;
                        continue;
                    }
                    // Block text input while *any* nag is active —
                    // characters typed before the user presses Y / N
                    // must not leak into the doc.
                    if !matches!(nag, Nag::None) {
                        cmdview_active = false;
                        palette_active = false;
                        redraw = true;
                        continue;
                    }
                    // Route typing into the Note Anvil sidebar search when focused.
                    if subsystems.has_notes_mode() && notes_search_focused {
                        notes_search_history.record(
                            &notes_search,
                            notes_search.len(),
                            FieldEdit::Insert,
                            buffer::now_secs(),
                        );
                        notes_search.push_str(text);
                        redraw = true;
                        continue;
                    }
                    // Forward text to terminal when focused.
                    if subsystems.has_terminal() && terminal.visible && terminal.focused {
                        if let Some(inst) = terminal.active_terminal() {
                            let _ = inst.inner.write(text.as_bytes());
                            inst.scrollback = 0.0;
                            inst.scrollback_target = 0.0;
                        }
                        redraw = true;
                        continue;
                    }
                    // Route typed characters into the inline new-file input.
                    if sidebar_new_file_dir.is_some() {
                        sidebar_new_file_history.record(
                            &sidebar_new_file_name,
                            sidebar_new_file_cursor,
                            FieldEdit::Insert,
                            buffer::now_secs(),
                        );
                        sidebar_new_file_name.insert_str(sidebar_new_file_cursor, text);
                        sidebar_new_file_cursor += text.len();
                        redraw = true;
                        continue;
                    }
                    if cmdview_active
                        && (subsystems.has_picker()
                            || cmdview_mode == CmdViewMode::SaveAs
                            || cmdview_mode == CmdViewMode::OpenFile
                            || cmdview_mode == CmdViewMode::OpenRecent
                            || cmdview_mode == CmdViewMode::Rename)
                    {
                        let prev_text = cmdview_text.clone();
                        cmdview_history.record(
                            &cmdview_text,
                            cmdview_cursor,
                            FieldEdit::Insert,
                            buffer::now_secs(),
                        );
                        // Insert at the caret rather than appending so left/right/home/end
                        // editing is preserved while typing.
                        cmdview_text.insert_str(cmdview_cursor, text);
                        cmdview_cursor += text.len();
                        let dirs_only = cmdview_mode == CmdViewMode::OpenFolder;
                        if cmdview_mode == CmdViewMode::OpenRecent {
                            let query = cmdview_text.to_lowercase();
                            let mut combined: Vec<String> = Vec::new();
                            if !single_file_mode {
                                for p in &recent_projects {
                                    if !combined.contains(p) {
                                        combined.push(p.clone());
                                    }
                                }
                            }
                            for p in &recent_files {
                                if !combined.contains(p) {
                                    combined.push(p.clone());
                                }
                            }
                            cmdview_suggestions = if query.is_empty() {
                                combined
                            } else {
                                combined
                                    .into_iter()
                                    .filter(|p| p.to_lowercase().contains(&query))
                                    .collect()
                            };
                        } else if cmdview_text.is_empty() {
                            cmdview_suggestions = if dirs_only {
                                recent_projects.clone()
                            } else {
                                recent_files.clone()
                            };
                        } else {
                            cmdview_suggestions =
                                path_suggest(&cmdview_text, &project_root, dirs_only);
                        }
                        cmdview_selected = 0;
                        // Typeahead: auto-fill when exactly one suggestion matches.
                        // Disabled for SaveAs -- suggestions are shown as options
                        // but must not overwrite what the user is typing. Also
                        // disabled in OpenRecent where suggestions are filtered
                        // by substring, not prefix.
                        if cmdview_mode != CmdViewMode::SaveAs
                            && cmdview_mode != CmdViewMode::OpenRecent
                            && cmdview_mode != CmdViewMode::Rename
                            && cmdview_suggestions.len() == 1
                            && cmdview_cursor == cmdview_text.len()
                            && cmdview_text.len() > prev_text.len()
                            && !cmdview_text.ends_with('/')
                        {
                            let suggestion = &cmdview_suggestions[0];
                            if suggestion.starts_with(&cmdview_text) {
                                cmdview_text = suggestion.clone();
                                cmdview_cursor = cmdview_text.len();
                            }
                        }
                        redraw = true;
                        continue;
                    }
                    if subsystems.has_find_in_files() && project_search_active {
                        project_search_history.record(
                            &project_search_query,
                            project_search_query.len(),
                            FieldEdit::Insert,
                            buffer::now_secs(),
                        );
                        project_search_query.push_str(text);
                        project_search_results = run_project_search(
                            &project_search_query,
                            &project_root,
                            project_use_regex,
                            project_whole_word,
                            project_case_insensitive,
                        );
                        project_search_selected = 0;
                        redraw = true;
                        continue;
                    }
                    if subsystems.has_find_in_files() && project_replace_active {
                        if project_replace_focus_on_replace {
                            project_replace_with_history.record(
                                &project_replace_with,
                                project_replace_with.len(),
                                FieldEdit::Insert,
                                buffer::now_secs(),
                            );
                            project_replace_with.push_str(text);
                        } else {
                            project_replace_search_history.record(
                                &project_replace_search,
                                project_replace_search.len(),
                                FieldEdit::Insert,
                                buffer::now_secs(),
                            );
                            project_replace_search.push_str(text);
                            project_replace_results = run_project_search(
                                &project_replace_search,
                                &project_root,
                                project_use_regex,
                                project_whole_word,
                                project_case_insensitive,
                            );
                            project_replace_selected = 0;
                        }
                        redraw = true;
                        continue;
                    }
                    if palette_active {
                        palette_history.record(
                            &palette_query,
                            palette_query.len(),
                            FieldEdit::Insert,
                            buffer::now_secs(),
                        );
                        palette_query.push_str(text);
                        palette_results = fuzzy_filter_commands(&palette_query, &all_commands);
                        palette_selected = 0;
                        redraw = true;
                        continue;
                    }
                    if nag.is_unsaved() {
                        cmdview_active = false;
                        palette_active = false;
                        redraw = true;
                        continue;
                    }
                    if find_active {
                        if find_focus_on_replace {
                            replace_history.record(
                                &replace_query,
                                replace_query.len(),
                                FieldEdit::Insert,
                                buffer::now_secs(),
                            );
                            replace_query.push_str(text);
                        } else {
                            find_history.record(
                                &find_query,
                                find_query.len(),
                                FieldEdit::Insert,
                                buffer::now_secs(),
                            );
                            find_query.push_str(text);
                            if let Some(doc) = docs.get_mut(active_tab) {
                                let dv = &mut doc.view;
                                let sel = if find_in_selection {
                                    find_selection_range
                                } else {
                                    None
                                };
                                find_matches = compute_find_matches_filtered(
                                    dv,
                                    &find_query,
                                    find_use_regex,
                                    find_whole_word,
                                    find_case_insensitive,
                                    sel,
                                );
                                find_current = find_match_at_or_after(
                                    &find_matches,
                                    find_anchor.0,
                                    find_anchor.1,
                                );
                                if let Some(i) = find_current {
                                    select_find_match(dv, find_matches[i], replace_active);
                                }
                            }
                        }
                        redraw = true;
                        continue;
                    }
                    // The markdown preview is read-only: typing while it holds
                    // focus scrolls or does nothing, never edits the source.
                    if docs
                        .get(active_tab)
                        .is_some_and(|doc| doc.preview.enabled && doc.preview.focused)
                    {
                        redraw = true;
                        continue;
                    }
                    if docs
                        .get(active_tab)
                        .is_some_and(|doc| crate::editor::open_doc::doc_policy(doc).read_only)
                    {
                        info_message = Some((
                            "Large file is read-only by configuration".to_string(),
                            Instant::now(),
                        ));
                        redraw = true;
                        continue;
                    }
                    if let Some(doc) = docs.get_mut(active_tab) {
                        let dv = &mut doc.view;
                        if let Some(buf_id) = dv.buffer_id {
                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                let is_single_char = text.chars().count() == 1;
                                let has_sel = b.selections.len() >= 4
                                    && (b.selections[0] != b.selections[2]
                                        || b.selections[1] != b.selections[3]);
                                if is_single_char
                                    && !has_sel
                                    && !overwrite_mode
                                    && buffer::cursor_count(b) == 1
                                {
                                    let line = *b.selections.first().unwrap_or(&1);
                                    let col = *b.selections.get(1).unwrap_or(&1);
                                    buffer::push_undo_mergeable(b, line, col, false);
                                } else {
                                    buffer::push_undo(b);
                                }
                                // Typing over an active selection replaces it. Only the
                                // single-cursor case is handled here; multi-cursor selection
                                // replacement would need per-cursor reverse-order deletion.
                                if has_sel && buffer::cursor_count(b) == 1 {
                                    buffer::delete_selection(b);
                                }
                                // Collect cursor positions, sorted bottom-to-top so
                                // insertions don't shift earlier cursor positions.
                                let n = buffer::cursor_count(b);
                                let mut cursor_positions: Vec<(usize, usize, usize)> = (0..n)
                                    .map(|i| {
                                        let base = i * 4;
                                        (i, b.selections[base + 2], b.selections[base + 3])
                                    })
                                    .collect();
                                cursor_positions
                                    .sort_by(|a, b_pos| b_pos.1.cmp(&a.1).then(b_pos.2.cmp(&a.2)));
                                let text_len = text.chars().count();
                                for &(idx, cline, ccol) in &cursor_positions {
                                    let _ = idx;
                                    if cline <= b.lines.len() {
                                        let l = &mut b.lines[cline - 1];
                                        let byte_pos = char_to_byte(l, ccol - 1);
                                        // In overwrite mode, delete the char at cursor before inserting.
                                        if overwrite_mode {
                                            let trimmed = l.trim_end_matches('\n');
                                            if byte_pos < trimmed.len() {
                                                let end = l
                                                    .char_indices()
                                                    .nth(ccol)
                                                    .map(|(i, _)| i)
                                                    .unwrap_or(trimmed.len());
                                                l.replace_range(byte_pos..end, "");
                                            }
                                        }
                                        let l = &mut b.lines[cline - 1];
                                        let byte_pos = char_to_byte(l, ccol - 1);
                                        l.insert_str(byte_pos, text);
                                    }
                                }
                                // Update all cursor positions after insertion.
                                // Re-sort top-to-bottom to adjust for same-line shifts.
                                cursor_positions
                                    .sort_by(|a, b_pos| a.1.cmp(&b_pos.1).then(a.2.cmp(&b_pos.2)));
                                let mut col_offset_on_line: Vec<(usize, usize)> = Vec::new();
                                for &(idx, cline, ccol) in &cursor_positions {
                                    let extra: usize = col_offset_on_line
                                        .iter()
                                        .filter(|(l, _)| *l == cline)
                                        .map(|(_, o)| o)
                                        .sum();
                                    let new_col = ccol + extra + text_len;
                                    let base = idx * 4;
                                    b.selections[base] = cline;
                                    b.selections[base + 1] = new_col;
                                    b.selections[base + 2] = cline;
                                    b.selections[base + 3] = new_col;
                                    col_offset_on_line.push((cline, text_len));
                                }
                                Ok(())
                            });
                        }
                        if subsystems.has_lsp() {
                            // Buffer-mutation marking happens generically in
                            // the per-frame change_id watcher; nothing to do
                            // here on the typing path beyond completion
                            // triggers below.
                            //
                            // Trigger LSP completion after trigger characters.
                            let trigger = text == "." || text == ":" || text == "(";
                            let word_char = text
                                .chars()
                                .next()
                                .map(|c| c.is_alphanumeric() || c == '_')
                                .unwrap_or(false);
                            if (trigger || word_char)
                                && lsp_state.transport_id.is_some()
                                && lsp_state.initialized
                                && docs
                                    .get(active_tab)
                                    .map(|doc| {
                                        !(crate::editor::open_doc::doc_policy(doc)
                                            .disable_autocomplete)
                                    })
                                    .unwrap_or(false)
                            {
                                if let Some(doc) = docs.get(active_tab) {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        if !doc.path.is_empty() {
                                            let tid = lsp_state.transport_id.unwrap();
                                            let (cl, cc) = buffer::with_buffer(buf_id, |b| {
                                                let l = *b.selections.get(2).unwrap_or(&1);
                                                let c = *b.selections.get(3).unwrap_or(&1);
                                                Ok((l, c))
                                            })
                                            .unwrap_or((1, 1));
                                            let uri = path_to_uri(&doc.path);
                                            let req_id = lsp_state.next_id();
                                            lsp_state.pending_requests.insert(
                                                req_id,
                                                "textDocument/completion".to_string(),
                                            );
                                            let _ = lsp::send_message(
                                                tid,
                                                &lsp_completion_request(
                                                    req_id,
                                                    &uri,
                                                    cl - 1,
                                                    cc - 1,
                                                ),
                                            );
                                            completion.line = cl;
                                            completion.col = cc;
                                            completion.latest_request_id = req_id;
                                        }
                                    }
                                }
                            }
                            // Trigger signature help after '(' or ','; hide on ')'.
                            if text == ")" {
                                signature_help.hide();
                            } else if (text == "(" || text == ",")
                                && lsp_state.transport_id.is_some()
                                && lsp_state.initialized
                                && let Some(doc) = docs.get(active_tab)
                                && let Some(buf_id) = doc.view.buffer_id
                                && !doc.path.is_empty()
                            {
                                let tid = lsp_state.transport_id.unwrap();
                                let (cl, cc) = buffer::with_buffer(buf_id, |b| {
                                    Ok((
                                        *b.selections.get(2).unwrap_or(&1),
                                        *b.selections.get(3).unwrap_or(&1),
                                    ))
                                })
                                .unwrap_or((1, 1));
                                let uri = path_to_uri(&doc.path);
                                let req_id = lsp_state.next_id();
                                lsp_state
                                    .pending_requests
                                    .insert(req_id, "textDocument/signatureHelp".to_string());
                                let _ = lsp::send_message(
                                    tid,
                                    &lsp_signature_help_request(req_id, &uri, cl - 1, cc - 1),
                                );
                                signature_help.line = cl;
                                signature_help.col = cc;
                            }
                        }
                    }
                    redraw = true;
                }
                EditorEvent::MousePressed {
                    button,
                    x,
                    y,
                    clicks,
                    modifiers,
                    ..
                } => {
                    cursor_blink_reset = Instant::now();
                    // Any mouse click cancels pending scroll animation so the
                    // view never jumps unexpectedly.
                    if let Some(doc) = docs.get_mut(active_tab) {
                        doc.view.target_scroll_y = doc.view.scroll_y;
                        // Keyboard focus follows the click: inside the preview
                        // pane it scrolls with the navigation keys, anywhere
                        // else (the editor, sidebar, tabs) returns to editing.
                        if doc.preview.enabled {
                            // The content rect excludes the split divider, so
                            // dragging the divider doesn't steal focus.
                            let pr = doc.preview.content_rect;
                            doc.preview.focused = pr.w > 0.0
                                && *x >= pr.x
                                && *x < pr.x + pr.w
                                && *y >= pr.y
                                && *y < pr.y + pr.h;
                        }
                    }
                    if *button == MouseButton::Left {
                        let contains = |rect: crate::editor::types::Rect| {
                            *x >= rect.x
                                && *x <= rect.x + rect.w
                                && *y >= rect.y
                                && *y <= rect.y + rect.h
                        };
                        if code_action_active
                            && let Some(action_idx) = code_action_picker_row_at(
                                *x,
                                *y,
                                &style,
                                code_actions.len(),
                                code_action_selected,
                            )
                            && let Some((_, action)) = code_actions.get(action_idx).cloned()
                        {
                            code_action_selected = action_idx;
                            if let Some(reason) = code_action_disabled_reason(&action) {
                                info_message = Some((reason.to_string(), Instant::now()));
                            } else if lsp_state.code_action_resolve_provider
                                && action.get("data").is_some_and(|v| !v.is_null())
                                && action.get("edit").is_none_or(|v| v.is_null())
                                && action.get("command").is_none_or(|v| v.is_null())
                                && let Some(tid) = lsp_state.transport_id
                            {
                                let req_id = lsp_state.next_id();
                                lsp_state
                                    .pending_requests
                                    .insert(req_id, "codeAction/resolve".to_string());
                                let _ = lsp::send_message(
                                    tid,
                                    &lsp_code_action_resolve_request(req_id, action),
                                );
                            } else {
                                execute_lsp_code_action(
                                    &action,
                                    &mut docs,
                                    &mut lsp_state,
                                    use_git(),
                                    config.files.atomic_save,
                                );
                            }
                            code_action_active = false;
                            redraw = true;
                            continue;
                        }
                        let quick_fix = diagnostic_quick_fix_rect.is_some_and(contains);
                        if quick_fix
                            && let Some((path, start_line, start_col, end_line, end_col)) =
                                hovered_diagnostic.clone()
                            && let Some(tab) = docs.iter().position(|doc| doc.path == path)
                        {
                            active_tab = tab;
                            if let Some(doc) = docs.get_mut(active_tab)
                                && let Some(buffer_id) = doc.view.buffer_id
                            {
                                let _ = buffer::with_buffer_mut(buffer_id, |buffer| {
                                    buffer.selections = vec![
                                        start_line + 1,
                                        start_col + 1,
                                        end_line + 1,
                                        end_col + 1,
                                    ];
                                    Ok(())
                                });
                                scroll_to_cursor(&mut doc.view);
                            }
                            code_actions.clone_from(&diagnostic_hover_actions);
                            code_action_selected = 0;
                            code_action_active = !code_actions.is_empty();
                            hover.hide();
                            hovered_diagnostic = None;
                            diagnostic_hover_region_rect = None;
                            diagnostic_hover_leave_at = None;
                            diagnostic_quick_fix_rect = None;
                            diagnostic_hover_action_request = None;
                            diagnostic_hover_actions.clear();
                            redraw = true;
                            continue;
                        }
                    }
                    // Nag bar button click handling.
                    if let Nag::UnsavedChanges {
                        message,
                        tab_to_close,
                    } = &nag
                    {
                        if *button == MouseButton::Left {
                            let message = message.clone();
                            let tab_to_close = *tab_to_close;
                            use crate::editor::view::DrawContext as _;
                            let bar_h = style.font_height + style.padding_y * 2.0;
                            if *y < bar_h {
                                let msg_w = draw_ctx.font_width(style.font, &message);
                                let btn_pad = style.padding_x;
                                let mut bx = style.padding_x + msg_w + btn_pad * 2.0;
                                for (i, label) in ["Yes", "No"].iter().enumerate() {
                                    let lw = draw_ctx.font_width(style.font, label) + btn_pad * 2.0;
                                    if *x >= bx && *x <= bx + lw {
                                        if i == 0 {
                                            // Yes: discard unsaved changes and proceed.
                                            if let Some(idx) = tab_to_close {
                                                if let Some(d) = docs.get(idx) {
                                                    autoreload.unwatch(&d.path);
                                                }
                                                docs.remove(idx);
                                                if active_tab >= docs.len() && !docs.is_empty() {
                                                    active_tab = docs.len() - 1;
                                                }
                                            } else {
                                                quit = true;
                                            }
                                        }
                                        // No (i == 1): just dismiss the nag.
                                        nag = Nag::None;
                                        #[allow(unused_assignments)]
                                        {
                                            redraw = true;
                                        }
                                        continue;
                                    }
                                    bx += lw + btn_pad;
                                }
                            }
                        }
                    }

                    // Context menu: left-click outside dismisses, right-click shows.
                    if context_menu.visible && *button == MouseButton::Left {
                        use crate::editor::view::DrawContext as _;
                        // Check if click is inside the context menu area.
                        let item_h = style.font_height + style.padding_y;
                        let menu_h = item_h * context_menu.items.len() as f64 + style.padding_y;
                        let menu_x = context_menu.position.x;
                        let menu_y = context_menu.position.y;
                        // Compute the actual menu width from its items so hits
                        // don't fall outside the drawn area (the old 200px
                        // approximation missed wider labels like
                        // "Close All to the Right").
                        let mut menu_w = 0.0_f64;
                        for it in &context_menu.items {
                            let w = draw_ctx.font_width(style.font, &it.text);
                            let total_w = if let Some(ref info) = it.info {
                                w + draw_ctx.font_width(style.font, info) + style.padding_x * 3.0
                            } else {
                                w + style.padding_x * 2.0
                            };
                            menu_w = menu_w.max(total_w);
                        }
                        menu_w = menu_w.max(120.0);
                        if *x >= menu_x
                            && *x <= menu_x + menu_w
                            && *y >= menu_y
                            && *y <= menu_y + menu_h
                        {
                            let idx =
                                ((*y - menu_y - style.padding_y / 2.0) / item_h).floor() as usize;
                            if let Some(item) = context_menu.items.get(idx) {
                                if let Some(ref cmd) = item.command {
                                    let cmd = cmd.clone();
                                    context_menu.hide();
                                    if cmd == "sidebar:new" {
                                        if let Some((path, is_dir)) = sidebar_menu_target.take() {
                                            let dir = if is_dir {
                                                path
                                            } else {
                                                std::path::Path::new(&path)
                                                    .parent()
                                                    .map(|p| p.to_string_lossy().to_string())
                                                    .unwrap_or_else(|| project_root.clone())
                                            };
                                            // Expand the target directory in the sidebar if
                                            // it isn't already so the inline input is visible.
                                            if let Some(dir_idx) = sidebar_entries
                                                .iter()
                                                .position(|e| e.is_dir && e.path == dir)
                                            {
                                                if !sidebar_entries[dir_idx].expanded {
                                                    sidebar_entries[dir_idx].expanded = true;
                                                    let depth = sidebar_entries[dir_idx].depth;
                                                    let children = scan_directory(
                                                        &dir,
                                                        depth + 1,
                                                        sidebar_show_hidden,
                                                    );
                                                    for (i, child) in
                                                        children.into_iter().enumerate()
                                                    {
                                                        sidebar_entries
                                                            .insert(dir_idx + 1 + i, child);
                                                    }
                                                    sidebar_watcher.watch_dir(&dir);
                                                }
                                            }
                                            sidebar_new_file_dir = Some(dir);
                                            sidebar_new_file_name.clear();
                                            sidebar_new_file_cursor = 0;
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    if cmd == "sidebar:rename" {
                                        if let Some((path, _is_dir)) = sidebar_menu_target.take() {
                                            rename_source = path.clone();
                                            cmdview_active = true;
                                            cmdview_mode = CmdViewMode::Rename;
                                            cmdview_text = path;
                                            cmdview_cursor = cmdview_text.len();
                                            cmdview_label = "Rename:".to_string();
                                            cmdview_suggestions = Vec::new();
                                            cmdview_selected = 0;
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    if cmd == "sidebar:delete" {
                                        if let Some((path, is_dir)) = sidebar_menu_target.take() {
                                            if !is_dir {
                                                nag = Nag::DeleteFile { path };
                                            }
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    if cmd == "sidebar:copy-path" {
                                        if let Some((path, _)) = sidebar_menu_target.take() {
                                            crate::window::set_clipboard_text(&path);
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    if cmd == "sidebar:copy-relative-path" {
                                        if let Some((path, _)) = sidebar_menu_target.take() {
                                            let rel = std::path::Path::new(&path)
                                                .strip_prefix(&project_root)
                                                .map(|p| p.to_string_lossy().into_owned())
                                                .unwrap_or_else(|_| path.clone());
                                            crate::window::set_clipboard_text(&rel);
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    if cmd.starts_with("test:") {
                                        let cmd: String = cmd;
                                        include!("commands_dispatch.rs");
                                        redraw = true;
                                        continue;
                                    }
                                    if cmd == "tab:copy-path" {
                                        if let Some(target) = tab_menu_target.take() {
                                            if let Some(d) = docs.get(target) {
                                                if !d.path.is_empty() {
                                                    crate::window::set_clipboard_text(&d.path);
                                                }
                                            }
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    if cmd == "tab:copy-relative-path" {
                                        if let Some(target) = tab_menu_target.take() {
                                            if let Some(d) = docs.get(target) {
                                                if !d.path.is_empty() {
                                                    let rel = std::path::Path::new(&d.path)
                                                        .strip_prefix(&project_root)
                                                        .map(|p| p.to_string_lossy().into_owned())
                                                        .unwrap_or_else(|_| d.path.clone());
                                                    crate::window::set_clipboard_text(&rel);
                                                }
                                            }
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    if cmd.starts_with("tab:close") {
                                        if let Some(target) = tab_menu_target.take() {
                                            // `indices` is built in reverse so
                                            // removing by index stays valid as the
                                            // list shrinks.
                                            let total = docs.len();
                                            let indices: Vec<usize> = match cmd.as_str() {
                                                "tab:close" => {
                                                    if target < total {
                                                        vec![target]
                                                    } else {
                                                        vec![]
                                                    }
                                                }
                                                "tab:close-right" => {
                                                    ((target + 1)..total).rev().collect()
                                                }
                                                "tab:close-left" => (0..target).rev().collect(),
                                                "tab:close-all" => (0..total).rev().collect(),
                                                _ => vec![],
                                            };
                                            // If any targeted doc is modified, nag
                                            // on the first modified one and skip
                                            // the rest — matches the close-button
                                            // safety net so we don't silently drop
                                            // unsaved buffers in a batch op.
                                            let first_mod =
                                                indices.iter().rev().copied().find(|&i| {
                                                    docs.get(i).is_some_and(doc_is_modified)
                                                });
                                            if let Some(i) = first_mod {
                                                let name = docs[i].name.clone();
                                                nag = Nag::UnsavedChanges {
                                                    message: nag_msg_close(&name),
                                                    tab_to_close: Some(i),
                                                };
                                            } else {
                                                for i in indices {
                                                    if let Some(d) = docs.get(i) {
                                                        autoreload.unwatch(&d.path);
                                                        if !d.path.is_empty() {
                                                            closed_tabs.retain(|p| p != &d.path);
                                                            closed_tabs.push(d.path.clone());
                                                            if closed_tabs.len() > 25 {
                                                                closed_tabs.remove(0);
                                                            }
                                                        }
                                                    }
                                                    docs.remove(i);
                                                }
                                                if active_tab >= docs.len() && !docs.is_empty() {
                                                    active_tab = docs.len() - 1;
                                                } else if docs.is_empty() {
                                                    active_tab = 0;
                                                } else if cmd == "tab:close-left" {
                                                    // The active tab's index
                                                    // shifted by the number of
                                                    // docs removed from the left.
                                                    active_tab = active_tab.saturating_sub(target);
                                                }
                                            }
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    {
                                        include!("commands_dispatch.rs");
                                    }
                                    redraw = true;
                                    continue;
                                }
                            }
                        }
                        context_menu.hide();
                        redraw = true;
                        continue;
                    }

                    if *button == MouseButton::Right {
                        // Right-click on a tab: show the tab context menu (Close /
                        // Close others left|right / Close all). Clicks on the
                        // dropdown button or empty tab-bar space are swallowed so
                        // the doc Cut/Copy/Paste menu doesn't spawn off-screen at
                        // the far right of the window.
                        let tab_h_rc = if !single_file_mode && !docs.is_empty() {
                            style.font_height + style.padding_y * 3.0
                        } else {
                            0.0
                        };
                        if *y < tab_h_rc {
                            use crate::editor::view::DrawContext as _;
                            let sidebar_w_tab = if subsystems.has_sidebar() && sidebar_visible {
                                sidebar_width
                            } else {
                                0.0
                            };
                            let (ww_tr, wh_tr, _, _) = crate::window::get_window_size();
                            let win_w_tr = ww_tr as f64;
                            let win_h_tr = wh_tr as f64;
                            let close_btn_w =
                                draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                            let dropdown_btn_w = (style.font_height + style.padding_x * 2.0).ceil();
                            let avail_full = (win_w_tr - sidebar_w_tab).max(0.0);
                            let mut full_total = 0.0_f64;
                            for doc in docs.iter() {
                                let label = if doc_is_modified(doc) {
                                    format!("*{}", doc.name)
                                } else {
                                    doc.name.clone()
                                };
                                full_total += draw_ctx.font_width(style.font, &label)
                                    + style.padding_x * 2.0
                                    + close_btn_w
                                    + style.divider_size;
                            }
                            let tabs_overflow = full_total > avail_full;
                            let tabs_right_limit = if tabs_overflow {
                                (win_w_tr - dropdown_btn_w).max(sidebar_w_tab)
                            } else {
                                win_w_tr
                            };

                            // Walk tabs in the same order / widths as the draw
                            // pass, find the one under the click.
                            let mut tx = sidebar_w_tab;
                            let mut hit: Option<usize> = None;
                            for (i, doc) in docs.iter().enumerate() {
                                let display_label = if tabs_overflow {
                                    let base = truncate_tab_name(&doc.name, 10);
                                    if doc_is_modified(doc) {
                                        format!("*{base}")
                                    } else {
                                        base
                                    }
                                } else if doc_is_modified(doc) {
                                    format!("*{}", doc.name)
                                } else {
                                    doc.name.clone()
                                };
                                let tw = draw_ctx.font_width(style.font, &display_label)
                                    + style.padding_x * 2.0
                                    + close_btn_w
                                    + style.divider_size;
                                let hit_right = (tx + tw).min(tabs_right_limit);
                                if *x >= tx && *x < hit_right {
                                    hit = Some(i);
                                    break;
                                }
                                tx += tw;
                                if tx >= tabs_right_limit {
                                    break;
                                }
                            }
                            if let Some(i) = hit {
                                tab_menu_target = Some(i);
                                let total = docs.len();
                                let mut items = vec![MenuItem {
                                    text: "Close".into(),
                                    info: None,
                                    command: Some("tab:close".into()),
                                    separator: false,
                                }];
                                if i + 1 < total {
                                    items.push(MenuItem {
                                        text: "Close All to the Right".into(),
                                        info: None,
                                        command: Some("tab:close-right".into()),
                                        separator: false,
                                    });
                                }
                                if i > 0 {
                                    items.push(MenuItem {
                                        text: "Close All to the Left".into(),
                                        info: None,
                                        command: Some("tab:close-left".into()),
                                        separator: false,
                                    });
                                }
                                if total > 1 {
                                    items.push(MenuItem {
                                        text: "Close All".into(),
                                        info: None,
                                        command: Some("tab:close-all".into()),
                                        separator: false,
                                    });
                                }
                                // Copy-path entries only make sense for an
                                // on-disk file (the doc has a path). Untitled
                                // buffers fall through with just the close
                                // group. The leading item with `separator:
                                // true` is a divider row, not a label; the
                                // real entries follow.
                                if docs.get(i).is_some_and(|d| !d.path.is_empty()) {
                                    push_path_copy_items(
                                        &mut items,
                                        "tab:copy-path",
                                        "tab:copy-relative-path",
                                    );
                                }
                                // Estimate the menu size and clamp its origin so
                                // it never renders off-screen. The context menu's
                                // draw_native sizes itself to the widest label.
                                let item_h = style.font_height + style.padding_y;
                                let menu_h = item_h * items.len() as f64 + style.padding_y;
                                let mut menu_w = 0.0_f64;
                                for it in &items {
                                    menu_w = menu_w.max(
                                        draw_ctx.font_width(style.font, &it.text)
                                            + style.padding_x * 2.0,
                                    );
                                }
                                let menu_x = x.min(win_w_tr - menu_w - 2.0).max(0.0);
                                let menu_y = y.min(win_h_tr - menu_h - 2.0).max(tab_h_rc);
                                context_menu.show(menu_x, menu_y, items);
                            }
                            redraw = true;
                            continue;
                        }
                        // Right-click on a sidebar entry: show a rename menu
                        // for that entry rather than the editor context menu.
                        let sidebar_w_rc = if subsystems.has_sidebar() && sidebar_visible {
                            sidebar_width
                        } else {
                            0.0
                        };
                        if subsystems.has_sidebar() && sidebar_visible && *x < sidebar_w_rc {
                            let entry_h = style.font_height + style.padding_y;
                            let sidebar_toolbar_h_rc = if subsystems.has_toolbar() {
                                style.font_height + style.padding_y * 2.0
                            } else {
                                0.0
                            };
                            let sidebar_dir_header_h = style.font_height + style.padding_y;
                            let notes_ui_h_rc = if subsystems.has_notes_mode() {
                                (style.font_height + style.padding_y * 2.0) * 2.0
                            } else {
                                0.0
                            };
                            let notes_display_rc: Vec<usize> = if subsystems.has_notes_mode() {
                                compute_notes_display_order(
                                    &sidebar_entries,
                                    &notes_search,
                                    notes_sort_mode,
                                )
                            } else {
                                (0..sidebar_entries.len()).collect()
                            };
                            let disp_idx =
                                ((*y - sidebar_toolbar_h_rc - sidebar_dir_header_h - notes_ui_h_rc
                                    + sidebar_scroll)
                                    / entry_h)
                                    .floor() as i64;
                            let click_idx: i64 =
                                if disp_idx >= 0 && (disp_idx as usize) < notes_display_rc.len() {
                                    notes_display_rc[disp_idx as usize] as i64
                                } else {
                                    -1
                                };
                            if click_idx >= 0 && (click_idx as usize) < sidebar_entries.len() {
                                let entry = &sidebar_entries[click_idx as usize];
                                sidebar_menu_target = Some((entry.path.clone(), entry.is_dir));
                                let mut items = vec![MenuItem {
                                    text: "New".into(),
                                    info: None,
                                    command: Some("sidebar:new".into()),
                                    separator: false,
                                }];
                                // Rename / Delete are only offered for files;
                                // directories would need recursive path-fixup
                                // across open tabs.
                                if !entry.is_dir {
                                    items.push(MenuItem {
                                        text: String::new(),
                                        info: None,
                                        command: None,
                                        separator: true,
                                    });
                                    items.push(MenuItem {
                                        text: "Rename".into(),
                                        info: None,
                                        command: Some("sidebar:rename".into()),
                                        separator: false,
                                    });
                                    items.push(MenuItem {
                                        text: "Delete".into(),
                                        info: None,
                                        command: Some("sidebar:delete".into()),
                                        separator: false,
                                    });
                                }
                                push_path_copy_items(
                                    &mut items,
                                    "sidebar:copy-path",
                                    "sidebar:copy-relative-path",
                                );
                                context_menu.show(*x, *y, items);
                                redraw = true;
                                continue;
                            }
                        }
                        let mut items = vec![
                            MenuItem {
                                text: "Undo".into(),
                                info: Some("Ctrl+Z".into()),
                                command: Some("doc:undo".into()),
                                separator: false,
                            },
                            MenuItem {
                                text: "Redo".into(),
                                info: Some("Ctrl+Y".into()),
                                command: Some("doc:redo".into()),
                                separator: false,
                            },
                            MenuItem {
                                text: String::new(),
                                info: None,
                                command: None,
                                separator: true,
                            },
                            MenuItem {
                                text: "Cut".into(),
                                info: Some("Ctrl+X".into()),
                                command: Some("doc:cut".into()),
                                separator: false,
                            },
                            MenuItem {
                                text: "Copy".into(),
                                info: Some("Ctrl+C".into()),
                                command: Some("doc:copy".into()),
                                separator: false,
                            },
                            MenuItem {
                                text: "Paste".into(),
                                info: Some("Ctrl+V".into()),
                                command: Some("doc:paste".into()),
                                separator: false,
                            },
                            MenuItem {
                                text: String::new(),
                                info: None,
                                command: None,
                                separator: true,
                            },
                            MenuItem {
                                text: "Select All".into(),
                                info: Some("Ctrl+A".into()),
                                command: Some("doc:select-all".into()),
                                separator: false,
                            },
                        ];
                        let active_doc_path =
                            docs.get(active_tab).map(|d| d.path.as_str()).unwrap_or("");
                        if !active_doc_path.is_empty() {
                            push_path_copy_items(
                                &mut items,
                                "doc:copy-path",
                                "doc:copy-relative-path",
                            );
                        }
                        if lsp_state.initialized {
                            items.push(MenuItem {
                                text: String::new(),
                                info: None,
                                command: None,
                                separator: true,
                            });
                            items.push(MenuItem {
                                text: "Go to Definition".into(),
                                info: None,
                                command: Some("lsp:go-to-definition".into()),
                                separator: false,
                            });
                            items.push(MenuItem {
                                text: "Find References".into(),
                                info: None,
                                command: Some("lsp:find-references".into()),
                                separator: false,
                            });
                        }
                        if subsystems.has_terminal()
                            && crate::editor::test_runner::detect_runner_with_fallback(
                                &project_root,
                                active_doc_path,
                            )
                            .is_some()
                        {
                            items.push(MenuItem {
                                text: String::new(),
                                info: None,
                                command: None,
                                separator: true,
                            });
                            items.push(MenuItem {
                                text: "Run All Tests".into(),
                                info: None,
                                command: Some("test:run-all".into()),
                                separator: false,
                            });
                            items.push(MenuItem {
                                text: "Run All Tests in Current File".into(),
                                info: None,
                                command: Some("test:run-in-current-file".into()),
                                separator: false,
                            });
                        }
                        use crate::editor::view::DrawContext as _;
                        let (ww_doc, wh_doc, _, _) = crate::window::get_window_size();
                        let win_w_doc = ww_doc as f64;
                        let win_h_doc = wh_doc as f64;
                        let item_h = style.font_height + style.padding_y;
                        let menu_h = item_h * items.len() as f64 + style.padding_y;
                        let mut menu_w = 0.0_f64;
                        for it in &items {
                            let w = draw_ctx.font_width(style.font, &it.text);
                            let total_w = if let Some(ref info) = it.info {
                                w + draw_ctx.font_width(style.font, info) + style.padding_x * 3.0
                            } else {
                                w + style.padding_x * 2.0
                            };
                            menu_w = menu_w.max(total_w);
                        }
                        let menu_x = x.min(win_w_doc - menu_w - 2.0).max(0.0);
                        let menu_y = y.min(win_h_doc - menu_h - 2.0).max(0.0);
                        context_menu.show(menu_x, menu_y, items);
                        redraw = true;
                        continue;
                    }

                    let sidebar_w = if sidebar_visible { sidebar_width } else { 0.0 };

                    // Sidebar scrollbar grab (lite-xl style). Must run before
                    // sidebar resize and sidebar click handlers, since the
                    // scrollbar lives inside the sidebar rect on the right.
                    if subsystems.has_sidebar()
                        && sidebar_visible
                        && *button == MouseButton::Left
                        && sidebar_content_h > sidebar_sb_h
                        && sidebar_sb_h > 0.0
                    {
                        let sb_w = style.scrollbar_size;
                        let sb_x = sidebar_w - style.divider_size - sb_w;
                        if *x >= sb_x
                            && *x < sb_x + sb_w
                            && *y >= sidebar_sb_top
                            && *y < sidebar_sb_top + sidebar_sb_h
                        {
                            let ratio = sidebar_sb_h / sidebar_content_h;
                            let min_thumb = style.scrollbar_size * 2.0;
                            let thumb_h = (sidebar_sb_h * ratio).max(min_thumb).min(sidebar_sb_h);
                            let max_scroll = (sidebar_content_h - sidebar_sb_h).max(1.0);
                            let scroll_frac = (sidebar_scroll / max_scroll).clamp(0.0, 1.0);
                            let thumb_y = sidebar_sb_top + scroll_frac * (sidebar_sb_h - thumb_h);
                            if *y >= thumb_y && *y < thumb_y + thumb_h {
                                sidebar_sb_drag_offset = *y - thumb_y;
                            } else {
                                sidebar_sb_drag_offset = thumb_h / 2.0;
                                let new_top = (*y - thumb_h / 2.0)
                                    .clamp(sidebar_sb_top, sidebar_sb_top + sidebar_sb_h - thumb_h);
                                let travel = (sidebar_sb_h - thumb_h).max(1.0);
                                let new_frac = (new_top - sidebar_sb_top) / travel;
                                sidebar_scroll = (new_frac * max_scroll).max(0.0);
                            }
                            sidebar_sb_dragging = true;
                            redraw = true;
                            continue;
                        }
                    }

                    // Sidebar resize drag: click near the right edge.
                    if subsystems.has_sidebar()
                        && sidebar_visible
                        && (*x - sidebar_w).abs() < 5.0
                        && *button == MouseButton::Left
                    {
                        sidebar_dragging = true;
                        redraw = true;
                        continue;
                    }

                    // Markdown preview resize drag: click near the editor|preview
                    // divider (the left edge of the preview pane).
                    if *button == MouseButton::Left
                        && docs
                            .get(active_tab)
                            .map(|d| {
                                d.preview.enabled
                                    && d.preview.rect.w > 0.0
                                    && (*x - d.preview.rect.x).abs() < 5.0
                            })
                            .unwrap_or(false)
                    {
                        preview_dragging = true;
                        redraw = true;
                        continue;
                    }

                    // When the inline new-file input is active, route left clicks:
                    // clicking into the editor commits the new file; clicking
                    // anywhere in the sidebar cancels it.
                    if sidebar_new_file_dir.is_some() && *button == MouseButton::Left {
                        let snap_w = if subsystems.has_sidebar() && sidebar_visible {
                            sidebar_width
                        } else {
                            0.0
                        };
                        if *x >= snap_w {
                            // Commit: create the file and open it.
                            let name = sidebar_new_file_name.trim().to_string();
                            let dir = sidebar_new_file_dir.take().unwrap_or_default();
                            sidebar_new_file_name.clear();
                            sidebar_new_file_cursor = 0;
                            if !name.is_empty() {
                                let full_path = std::path::Path::new(&dir)
                                    .join(&name)
                                    .to_string_lossy()
                                    .to_string();
                                if std::path::Path::new(&full_path).exists() {
                                    info_message = Some((
                                        format!("File already exists: {name}"),
                                        Instant::now(),
                                    ));
                                } else {
                                    match std::fs::write(&full_path, "") {
                                        Ok(()) => {
                                            if subsystems.has_sidebar() && !project_root.is_empty()
                                            {
                                                let in_memory_expanded: HashSet<String> =
                                                    sidebar_entries
                                                        .iter()
                                                        .filter(|e| e.is_dir && e.expanded)
                                                        .map(|e| e.path.clone())
                                                        .collect();
                                                sidebar_entries = scan_for_sidebar(
                                                    subsystems.has_notes_mode(),
                                                    &project_root,
                                                    sidebar_show_hidden,
                                                );
                                                restore_expanded_folders(
                                                    &mut sidebar_entries,
                                                    userdir_path,
                                                    sidebar_show_hidden,
                                                    &project_session_key(&project_root),
                                                );
                                                expand_sidebar_from_set(
                                                    &mut sidebar_entries,
                                                    &in_memory_expanded,
                                                    sidebar_show_hidden,
                                                );
                                            }
                                            if open_file_into(&full_path, &mut docs, use_git()) {
                                                autoreload.watch(&full_path);
                                                active_tab = docs.len() - 1;
                                                remember_recent_file(
                                                    &mut recent_files,
                                                    &full_path,
                                                    userdir_path,
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            info_message = Some((
                                                format!("Create failed: {e}"),
                                                Instant::now(),
                                            ));
                                        }
                                    }
                                }
                            }
                            // Fall through so the click still lands in the editor.
                        } else {
                            // Cancel and swallow the click.
                            sidebar_new_file_dir = None;
                            sidebar_new_file_name.clear();
                            sidebar_new_file_cursor = 0;
                            redraw = true;
                            continue;
                        }
                    }

                    // Sidebar click detection.
                    if subsystems.has_sidebar()
                        && sidebar_visible
                        && *button == MouseButton::Left
                        && *x < sidebar_w
                    {
                        use crate::editor::view::DrawContext as _;
                        let ibf = style.icon_big_font;
                        let sidebar_toolbar_h = if subsystems.has_toolbar() {
                            draw_ctx.font_height(ibf) + style.padding_y * 2.0
                        } else {
                            0.0
                        };

                        // Toolbar button click (only when toolbar is enabled).
                        if subsystems.has_toolbar() && *y < sidebar_toolbar_h {
                            let toolbar_buttons: &[(&str, &str)] = &[
                                ("f", "core:new-doc"),
                                ("D", "core:open-file"),
                                ("S", "doc:save"),
                                ("L", "find-replace:find"),
                                ("B", "core:find-command"),
                                ("P", "core:open-user-settings"),
                            ];
                            let mut bx = style.padding_x;
                            let icon_spacing = style.padding_x;
                            let mut clicked_cmd: Option<&str> = None;
                            for (icon, cmd) in toolbar_buttons {
                                let iw = draw_ctx.font_width(ibf, icon);
                                if *x >= bx && *x < bx + iw {
                                    clicked_cmd = Some(cmd);
                                    break;
                                }
                                bx += iw + icon_spacing;
                            }
                            if let Some(cmd) = clicked_cmd {
                                let cmd = cmd.to_string();
                                {
                                    let cmd: String = cmd;
                                    include!("commands_dispatch.rs");
                                }
                            }
                            redraw = true;
                            continue;
                        }

                        let entry_h = style.font_height + style.padding_y;
                        let sidebar_dir_header_h = style.font_height + style.padding_y;
                        // Notes-mode sort/search rows sit between the directory
                        // header and the file list.
                        let notes_sort_row_h = style.font_height + style.padding_y * 2.0;
                        let notes_search_row_h = style.font_height + style.padding_y * 2.0;
                        let notes_ui_h = if subsystems.has_notes_mode() {
                            notes_sort_row_h + notes_search_row_h
                        } else {
                            0.0
                        };
                        if subsystems.has_notes_mode() {
                            let sort_y0 = sidebar_toolbar_h + sidebar_dir_header_h;
                            let sort_y1 = sort_y0 + notes_sort_row_h;
                            let search_y1 = sort_y1 + notes_search_row_h;
                            if *y >= sort_y0 && *y < sort_y1 {
                                // Sort-mode toggle. Left half = A-Z, right = Recent.
                                let half = (sidebar_w / 2.0).floor();
                                if *x < half {
                                    // A-Z: toggle between asc (0) and desc (1).
                                    notes_sort_mode = if notes_sort_mode == 0 { 1 } else { 0 };
                                } else {
                                    // Recent: toggle between newest-first (2)
                                    // and oldest-first (3).
                                    notes_sort_mode = if notes_sort_mode == 2 { 3 } else { 2 };
                                }
                                notes_search_focused = false;
                                let _ = crate::editor::storage::save_text(
                                    userdir_path,
                                    "session",
                                    "notes_sort_mode",
                                    &notes_sort_mode.to_string(),
                                );
                                redraw = true;
                                continue;
                            }
                            if *y >= sort_y1 && *y < search_y1 {
                                notes_search_focused = true;
                                redraw = true;
                                continue;
                            }
                            // Click outside the search row unfocuses it.
                            notes_search_focused = false;
                        }
                        let sidebar_content_top =
                            sidebar_toolbar_h + sidebar_dir_header_h + notes_ui_h;
                        if *y < sidebar_content_top {
                            redraw = true;
                            continue;
                        }
                        let notes_display_click: Vec<usize> = if subsystems.has_notes_mode() {
                            compute_notes_display_order(
                                &sidebar_entries,
                                &notes_search,
                                notes_sort_mode,
                            )
                        } else {
                            (0..sidebar_entries.len()).collect()
                        };
                        let disp_click_idx = ((*y - sidebar_content_top + sidebar_scroll) / entry_h)
                            .floor() as usize;
                        let click_idx = notes_display_click
                            .get(disp_click_idx)
                            .copied()
                            .unwrap_or(sidebar_entries.len());
                        if click_idx < sidebar_entries.len() {
                            let entry = &sidebar_entries[click_idx];
                            if entry.is_dir {
                                let was_expanded = sidebar_entries[click_idx].expanded;
                                let path = sidebar_entries[click_idx].path.clone();
                                let depth = sidebar_entries[click_idx].depth;
                                if was_expanded {
                                    // Collapse: remove children.
                                    sidebar_entries[click_idx].expanded = false;
                                    let remove_start = click_idx + 1;
                                    let mut remove_end = remove_start;
                                    while remove_end < sidebar_entries.len()
                                        && sidebar_entries[remove_end].depth > depth
                                    {
                                        remove_end += 1;
                                    }
                                    sidebar_watcher.unwatch_dir(&path);
                                    for entry in
                                        sidebar_entries.iter().take(remove_end).skip(remove_start)
                                    {
                                        if entry.is_dir && entry.expanded {
                                            sidebar_watcher.unwatch_dir(&entry.path.clone());
                                        }
                                    }
                                    sidebar_entries.drain(remove_start..remove_end);
                                } else {
                                    // Expand: insert children.
                                    sidebar_entries[click_idx].expanded = true;
                                    let children =
                                        scan_directory(&path, depth + 1, sidebar_show_hidden);
                                    let insert_at = click_idx + 1;
                                    for (i, child) in children.into_iter().enumerate() {
                                        sidebar_entries.insert(insert_at + i, child);
                                    }
                                    sidebar_watcher.watch_dir(&path);
                                }
                            } else {
                                // Open file as new tab (if not already open).
                                let entry_path = entry.path.clone();
                                let already = docs.iter().position(|d| d.path == entry_path);
                                if let Some(idx) = already {
                                    active_tab = idx;
                                } else {
                                    // Notes mode is single-note-at-a-time —
                                    // close any other notes before opening
                                    // the new one. Autosave will have
                                    // persisted the outgoing buffer
                                    // already, so just drop the tab.
                                    if subsystems.has_notes_mode() {
                                        for d in &docs {
                                            autoreload.unwatch(&d.path);
                                        }
                                        docs.clear();
                                        active_tab = 0;
                                    }
                                    if crate::editor::open_doc::open_file_into_user_requested(
                                        &entry_path,
                                        &mut docs,
                                        use_git(),
                                    ) {
                                        autoreload.watch(&entry_path);
                                        active_tab = docs.len() - 1;
                                        remember_recent_file(
                                            &mut recent_files,
                                            &entry_path,
                                            userdir_path,
                                        );
                                    }
                                }
                                // Ensure the switched-to tab has no pending animation.
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    doc.view.target_scroll_y = doc.view.scroll_y;
                                }
                            }
                        }
                        redraw = true;
                        continue;
                    }

                    // Tab bar click detection.
                    let tab_h = if !single_file_mode && !docs.is_empty() {
                        style.font_height + style.padding_y * 3.0
                    } else {
                        0.0
                    };

                    // Overflow dropdown handling: if the list is open, clicks inside
                    // the list pick that tab; clicks elsewhere close it. If it's
                    // closed, a click on the dropdown button opens it. Left-click
                    // only — right-click in the tab bar should fall through to the
                    // regular context menu path, not toggle the dropdown.
                    if !docs.is_empty() && !single_file_mode && *button == MouseButton::Left {
                        use crate::editor::view::DrawContext as _;
                        let (ww_tab, _, _, _) = crate::window::get_window_size();
                        let width = ww_tab as f64;
                        let close_btn_w =
                            draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                        let dropdown_btn_w = (style.font_height + style.padding_x * 2.0).ceil();
                        let avail_full = (width - sidebar_w).max(0.0);
                        let mut full_total = 0.0_f64;
                        for doc in docs.iter() {
                            let label = if doc_is_modified(doc) {
                                format!("*{}", doc.name)
                            } else {
                                doc.name.clone()
                            };
                            full_total += draw_ctx.font_width(style.font, &label)
                                + style.padding_x * 2.0
                                + close_btn_w
                                + style.divider_size;
                        }
                        let tabs_overflow = full_total > avail_full;

                        if tab_dropdown_open && tabs_overflow {
                            let item_h = style.font_height + style.padding_y;
                            let mut list_w = 0.0_f64;
                            for doc in docs.iter() {
                                let label = if doc_is_modified(doc) {
                                    format!("*{}", doc.name)
                                } else {
                                    doc.name.clone()
                                };
                                list_w = list_w.max(
                                    draw_ctx.font_width(style.font, &label) + style.padding_x * 3.0,
                                );
                            }
                            let (_, wh_tab, _, _) = crate::window::get_window_size();
                            let height = wh_tab as f64;
                            let avail_list_w = (width - sidebar_w - 4.0).max(40.0);
                            list_w = list_w.max(120.0).min(avail_list_w);
                            let mut list_x = width - list_w - 2.0;
                            if list_x < sidebar_w + 2.0 {
                                list_x = sidebar_w + 2.0;
                            }
                            let max_list_h = (height - tab_h - 4.0).max(item_h);
                            let raw_list_h = item_h * docs.len() as f64 + style.padding_y;
                            let list_h = raw_list_h.min(max_list_h);
                            let list_y = tab_h + 1.0;
                            if *x >= list_x
                                && *x < list_x + list_w
                                && *y >= list_y
                                && *y < list_y + list_h
                            {
                                let rel = (*y - list_y - style.padding_y / 2.0) / item_h;
                                let idx = rel.floor().max(0.0) as usize;
                                if idx < docs.len() {
                                    active_tab = idx;
                                    if let Some(doc) = docs.get_mut(idx) {
                                        doc.view.target_scroll_y = doc.view.scroll_y;
                                    }
                                }
                                tab_dropdown_open = false;
                                redraw = true;
                                continue;
                            }
                            // Click outside the list dismisses it; also dismiss on a
                            // click on the dropdown button itself (toggle behavior).
                            tab_dropdown_open = false;
                            let btn_x = width - dropdown_btn_w;
                            if *y < tab_h && *x >= btn_x {
                                redraw = true;
                                continue;
                            }
                        } else if tabs_overflow && *y < tab_h {
                            let btn_x = width - dropdown_btn_w;
                            if *x >= btn_x {
                                tab_dropdown_open = true;
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    if *y < tab_h && !docs.is_empty() {
                        use crate::editor::view::DrawContext as _;
                        let (ww_tab_click, _, _, _) = crate::window::get_window_size();
                        let width = ww_tab_click as f64;
                        let close_btn_w =
                            draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                        let dropdown_btn_w = (style.font_height + style.padding_x * 2.0).ceil();

                        // Recompute overflow decision to match the draw pass, and
                        // truncate labels accordingly.
                        let avail_full = (width - sidebar_w).max(0.0);
                        let mut full_total = 0.0_f64;
                        for doc in docs.iter() {
                            let label = if doc_is_modified(doc) {
                                format!("*{}", doc.name)
                            } else {
                                doc.name.clone()
                            };
                            full_total += draw_ctx.font_width(style.font, &label)
                                + style.padding_x * 2.0
                                + close_btn_w
                                + style.divider_size;
                        }
                        let tabs_overflow = full_total > avail_full;
                        let tabs_right_limit = if tabs_overflow {
                            (width - dropdown_btn_w).max(sidebar_w)
                        } else {
                            width
                        };

                        let mut tx = sidebar_w;
                        let mut clicked_close = false;
                        for (i, doc) in docs.iter().enumerate() {
                            let display_label = if tabs_overflow {
                                let base = truncate_tab_name(&doc.name, 10);
                                if doc_is_modified(doc) {
                                    format!("*{base}")
                                } else {
                                    base
                                }
                            } else if doc_is_modified(doc) {
                                format!("*{}", doc.name)
                            } else {
                                doc.name.clone()
                            };
                            let tw = draw_ctx.font_width(style.font, &display_label)
                                + style.padding_x * 2.0
                                + close_btn_w
                                + style.divider_size;
                            // Clip clickable area to the visible region.
                            let click_right = (tx + tw).min(tabs_right_limit);
                            if *x >= tx && *x < click_right {
                                // Check if click is on the close button area (only
                                // when the close icon is actually on-screen).
                                let close_x = tx + tw - close_btn_w - style.divider_size;
                                if *x >= close_x && close_x + close_btn_w <= tabs_right_limit {
                                    if doc_is_modified(doc) {
                                        nag = Nag::UnsavedChanges {
                                            message: nag_msg_close(&doc.name),
                                            tab_to_close: Some(i),
                                        };
                                    } else {
                                        autoreload.unwatch(&doc.path);
                                        if !doc.path.is_empty() {
                                            closed_tabs.retain(|p| p != &doc.path);
                                            closed_tabs.push(doc.path.clone());
                                            if closed_tabs.len() > 25 {
                                                closed_tabs.remove(0);
                                            }
                                        }
                                        docs.remove(i);
                                        if active_tab >= docs.len() && !docs.is_empty() {
                                            active_tab = docs.len() - 1;
                                        }
                                    }
                                    clicked_close = true;
                                } else {
                                    active_tab = i;
                                    tab_tooltip_suppressed = true;
                                    tab_dragging = Some(i);
                                    if let Some(doc) = docs.get_mut(i) {
                                        doc.view.target_scroll_y = doc.view.scroll_y;
                                    }
                                }
                                break;
                            }
                            tx += tw;
                            if tx >= tabs_right_limit {
                                break;
                            }
                        }
                        let _ = clicked_close;
                        redraw = true;
                        continue;
                    }
                    // Terminal click: focus the terminal panel, handle tab/close clicks.
                    if terminal.visible {
                        let (ww, wh, _, _) = crate::window::get_window_size();
                        let win_w = ww as f64;
                        let win_h = wh as f64;
                        let status_h_click = style.font_height + style.padding_y * 2.0;
                        let terminal_h_click = (win_h * 0.3)
                            .min(win_h - tab_h - status_h_click - 50.0)
                            .max(80.0);
                        let term_y_click = win_h - terminal_h_click - status_h_click;
                        let sidebar_w_click = if subsystems.has_sidebar() && sidebar_visible {
                            sidebar_width
                        } else {
                            0.0
                        };
                        let term_x_click = sidebar_w_click;
                        let term_w_click = win_w - sidebar_w_click;
                        let tab_bar_h_click = if !terminal.terminals.is_empty() {
                            style.font_height + style.padding_y * 3.0
                        } else {
                            0.0
                        };
                        let tab_bar_y = term_y_click + style.divider_size;

                        // Tab bar click (switch / close).
                        if tab_bar_h_click > 0.0
                            && *y >= tab_bar_y
                            && *y < tab_bar_y + tab_bar_h_click
                            && *x >= term_x_click
                            && *x < term_x_click + term_w_click
                        {
                            use crate::editor::view::DrawContext as _;
                            let close_w =
                                draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                            let mut tx = term_x_click;
                            let mut handled = false;
                            let n = terminal.terminals.len();
                            for i in 0..n {
                                let label_w =
                                    draw_ctx.font_width(style.font, &terminal.terminals[i].title);
                                let tw = label_w + style.padding_x * 2.0 + close_w;
                                let close_x = tx + tw - close_w;
                                if *x >= close_x && *x < close_x + close_w {
                                    // Close this terminal.
                                    terminal.active = i;
                                    terminal.close_active();
                                    crate::window::force_invalidate();
                                    handled = true;
                                    break;
                                }
                                if *x >= tx && *x < tx + tw {
                                    terminal.active = i;
                                    terminal.focused = true;
                                    handled = true;
                                    break;
                                }
                                tx += tw + style.divider_size;
                            }
                            if handled {
                                redraw = true;
                                continue;
                            }
                        }

                        if *y >= term_y_click && *y < win_h - status_h_click {
                            terminal.focused = true;
                            // Clicking inside the terminal viewport starts a
                            // text selection (mouse-drag copy). If the click
                            // lands on the scrollbar strip on the right
                            // edge, fall through so the dedicated scrollbar
                            // handler below can grab the thumb.
                            use crate::editor::view::DrawContext as _;
                            let char_h_v = style.code_font_height * 1.2;
                            let char_w_v = draw_ctx.font_width(style.code_font, "m");
                            let ty_start =
                                term_y_click + style.divider_size + tab_bar_h_click + 2.0;
                            let visible_h =
                                (term_y_click + terminal_h_click - ty_start - style.padding_y)
                                    .max(0.0);
                            let rows_visible = (visible_h / char_h_v).floor().max(1.0) as usize;
                            let sb_w_v = style.scrollbar_size.max(6.0);
                            let on_scrollbar = *x >= term_x_click + term_w_click - sb_w_v
                                && *x < term_x_click + term_w_click
                                && *y >= ty_start
                                && *y < ty_start + char_h_v * rows_visible as f64;
                            if on_scrollbar {
                                // Do not consume -- let the scrollbar
                                // handler below pick this up.
                            } else {
                                let in_viewport = *y >= ty_start
                                    && *y < ty_start + char_h_v * rows_visible as f64
                                    && *x >= term_x_click
                                    && *x < term_x_click + term_w_click - sb_w_v
                                    && char_w_v > 0.0;
                                if in_viewport && *button == MouseButton::Left {
                                    let col = (((*x - term_x_click - style.padding_x) / char_w_v)
                                        .floor()
                                        as i64)
                                        .max(0)
                                        as usize;
                                    let vis_row = (((*y - ty_start) / char_h_v).floor() as i64)
                                        .max(0)
                                        as usize;
                                    if let Some(inst) = terminal.terminals.get_mut(terminal.active)
                                    {
                                        inst.sel_start = Some((vis_row, col));
                                        inst.sel_end = Some((vis_row, col));
                                        inst.sel_dragging = true;
                                    }
                                }
                                // Middle-click pastes the X11 PRIMARY selection
                                // into the shell, matching Linux terminals.
                                if in_viewport
                                    && *button == MouseButton::Middle
                                    && let Some(text) = crate::window::get_primary_selection_text()
                                    && let Some(inst) = terminal.active_terminal()
                                {
                                    let _ = inst.inner.write(text.as_bytes());
                                    inst.scrollback = 0.0;
                                    inst.scrollback_target = 0.0;
                                }
                                redraw = true;
                                continue;
                            }
                        } else {
                            terminal.focused = false;
                        }
                        let _ = ww;
                    }

                    // Minimap click: scroll to the clicked line.
                    if minimap_visible {
                        let (ww, _, _, _) = crate::window::get_window_size();
                        let win_w = ww as f64;
                        let mm_w = 120.0_f64;
                        let mm_x = win_w - mm_w;
                        if *x >= mm_x {
                            let mlh = 4.0_f64;
                            let mm_y = tab_h;
                            let mm_h = {
                                let (_, wh, _, _) = crate::window::get_window_size();
                                let st_h = style.font_height + style.padding_y * 2.0;
                                let terminal_h_click = if terminal.visible {
                                    (wh as f64 * 0.3)
                                        .min(wh as f64 - tab_h - st_h - 50.0)
                                        .max(80.0)
                                } else {
                                    0.0
                                };
                                wh as f64 - tab_h - terminal_h_click - st_h
                            };
                            if let Some(doc) = docs.get_mut(active_tab) {
                                let dv = &mut doc.view;
                                let total_lines =
                                    buffer::with_buffer(dv.buffer_id.unwrap_or(0), |b| {
                                        Ok(b.lines.len())
                                    })
                                    .unwrap_or(0);
                                if total_lines > 0 {
                                    let doc_line_h = style.code_font_height * 1.2;
                                    let visible_lines = (dv.rect().h / doc_line_h).ceil() as usize;
                                    let first_visible =
                                        (dv.scroll_y / doc_line_h).floor() as usize + 1;
                                    let last_visible = first_visible + visible_lines;
                                    let vis_center = (first_visible + last_visible) / 2;
                                    let lines_that_fit = (mm_h / mlh).floor() as usize;
                                    let minimap_start = if total_lines <= lines_that_fit {
                                        1
                                    } else {
                                        let half = lines_that_fit / 2;
                                        let start = vis_center.saturating_sub(half).max(1);
                                        start.min(total_lines.saturating_sub(lines_that_fit) + 1)
                                    };
                                    let relative_y = *y - mm_y;
                                    let clicked_line_offset = (relative_y / mlh).floor() as usize;
                                    let target_line =
                                        (minimap_start + clicked_line_offset).clamp(1, total_lines);
                                    let new_scroll = ((target_line as f64 - 1.0) * doc_line_h
                                        - dv.rect().h / 2.0)
                                        .max(0.0);
                                    dv.scroll_y = new_scroll;
                                    dv.target_scroll_y = new_scroll;
                                }
                            }
                            redraw = true;
                            continue;
                        }
                    }

                    // Markdown preview click routing: if the click is in
                    // the preview pane, check checkbox regions first (they
                    // are small targets), then link regions, then bail out
                    // so the click doesn't fall through to the editor.
                    if let Some(doc) = docs.get_mut(active_tab) {
                        if doc.preview.enabled && *button == MouseButton::Left {
                            let pr = doc.preview.rect;
                            if pr.w > 0.0
                                && *x >= pr.x
                                && *x < pr.x + pr.w
                                && *y >= pr.y
                                && *y < pr.y + pr.h
                            {
                                // Scrollbars first: they sit on top of the
                                // content, so a click there is never a
                                // checkbox or link click. A press on the
                                // thumb grabs it; a press on the empty track
                                // centers the thumb under the cursor and then
                                // grabs, matching the editor scrollbar.
                                let v_bar = crate::editor::markdown_preview::vertical_scrollbar(
                                    &doc.preview,
                                    &style,
                                );
                                let h_bar = crate::editor::markdown_preview::horizontal_scrollbar(
                                    &doc.preview,
                                    &style,
                                );
                                if let Some(bar) = v_bar
                                    && bar.track_contains(*x, *y)
                                {
                                    if bar.thumb_contains(*x, *y) {
                                        preview_sb_v_drag_offset = *y - bar.thumb.y;
                                    } else {
                                        preview_sb_v_drag_offset = bar.thumb.h / 2.0;
                                        crate::editor::markdown_preview::scroll_to_thumb_top(
                                            &mut doc.preview,
                                            &style,
                                            *y - bar.thumb.h / 2.0,
                                        );
                                    }
                                    preview_sb_v_dragging = true;
                                    redraw = true;
                                    continue;
                                }
                                if let Some(bar) = h_bar
                                    && bar.track_contains(*x, *y)
                                {
                                    if bar.thumb_contains(*x, *y) {
                                        preview_sb_h_drag_offset = *x - bar.thumb.x;
                                    } else {
                                        preview_sb_h_drag_offset = bar.thumb.w / 2.0;
                                        crate::editor::markdown_preview::scroll_to_thumb_left(
                                            &mut doc.preview,
                                            &style,
                                            *x - bar.thumb.w / 2.0,
                                        );
                                    }
                                    preview_sb_h_dragging = true;
                                    redraw = true;
                                    continue;
                                }
                                // Checkbox next.
                                let cb = doc
                                    .preview
                                    .checkbox_regions
                                    .iter()
                                    .find(|r| *x >= r.x1 && *x <= r.x2 && *y >= r.y1 && *y <= r.y2)
                                    .cloned();
                                if let Some(cb) = cb {
                                    let large_read_only =
                                        crate::editor::open_doc::doc_policy(doc).read_only;
                                    if large_read_only {
                                        info_message = Some((
                                            "Large file is read-only by configuration".to_string(),
                                            Instant::now(),
                                        ));
                                    } else if let Some(buf_id) = doc.view.buffer_id {
                                        let src =
                                            buffer::with_buffer(buf_id, |b| Ok(b.lines.join("")))
                                                .unwrap_or_default();
                                        if let Some((line, col, ch)) =
                                            crate::editor::markdown_preview::toggle_task_at(
                                                &src,
                                                cb.source_start,
                                                cb.checked,
                                            )
                                        {
                                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                buffer::push_undo(b);
                                                if line <= b.lines.len() {
                                                    let l = &mut b.lines[line - 1];
                                                    let byte_pos = char_to_byte(l, col - 1);
                                                    if byte_pos < l.len() {
                                                        l.replace_range(
                                                            byte_pos..byte_pos + 1,
                                                            &ch.to_string(),
                                                        );
                                                        b.change_id += 1;
                                                    }
                                                }
                                                Ok(())
                                            });
                                            // Force reparse next draw so the
                                            // checkbox visibly fills/clears.
                                            doc.preview.cached_change_id = -1;
                                        }
                                    }
                                    redraw = true;
                                    continue;
                                }
                                // Link next.
                                if let Some(lr) =
                                    doc.preview.link_regions.iter().find(|r| {
                                        *x >= r.x1 && *x <= r.x2 && *y >= r.y1 && *y <= r.y2
                                    })
                                {
                                    crate::editor::markdown_preview::open_url(&lr.href);
                                }
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    // Editor scrollbar mouse-down: grab the thumb (lite-xl
                    // style). If the click is on the thumb itself, we keep
                    // the existing scroll and remember the offset within the
                    // thumb so dragging feels like grabbing a handle. If the
                    // click is on the empty track, we jump so the thumb
                    // centers under the cursor, then grab for the drag.
                    if let Some(doc) = docs.get_mut(active_tab) {
                        let dv_rect = doc.view.rect();
                        let sb_w = style.scrollbar_size;
                        let sb_x = dv_rect.x + dv_rect.w - sb_w;
                        if *x >= sb_x
                            && *x < sb_x + sb_w
                            && *y >= dv_rect.y
                            && *y < dv_rect.y + dv_rect.h
                            && dv_rect.h > 0.0
                        {
                            let line_h_sb = style.code_font_height * 1.2;
                            let total_lines = doc
                                .view
                                .buffer_id
                                .and_then(|id| buffer::with_buffer(id, |b| Ok(b.lines.len())).ok())
                                .unwrap_or(1) as f64;
                            let total_h = total_lines * line_h_sb;
                            if total_h > dv_rect.h {
                                let ratio = dv_rect.h / total_h;
                                let min_thumb = style.scrollbar_size * 2.0;
                                let thumb_h = (dv_rect.h * ratio).max(min_thumb).min(dv_rect.h);
                                let scroll_frac =
                                    doc.view.scroll_y / (total_h - dv_rect.h).max(1.0);
                                let thumb_y = dv_rect.y + scroll_frac * (dv_rect.h - thumb_h);
                                if *y >= thumb_y && *y < thumb_y + thumb_h {
                                    editor_sb_drag_offset = *y - thumb_y;
                                } else {
                                    editor_sb_drag_offset = thumb_h / 2.0;
                                    let new_top = (*y - thumb_h / 2.0)
                                        .clamp(dv_rect.y, dv_rect.y + dv_rect.h - thumb_h);
                                    let new_frac = (new_top - dv_rect.y) / (dv_rect.h - thumb_h);
                                    let new_scroll = (new_frac * (total_h - dv_rect.h)).max(0.0);
                                    doc.view.target_scroll_y = new_scroll;
                                    doc.view.scroll_y = new_scroll;
                                }
                                editor_sb_dragging = true;
                                redraw = true;
                                continue;
                            }
                        }
                    }

                    // Terminal scrollbar click: set scrollback_target by the
                    // clicked fraction of the track.
                    if subsystems.has_terminal() && terminal.visible {
                        let (ww, wh, _, _) = crate::window::get_window_size();
                        let win_w = ww as f64;
                        let win_h = wh as f64;
                        let status_h_sc = style.font_height + style.padding_y * 2.0;
                        let tab_h_sc = if !single_file_mode && !docs.is_empty() {
                            style.font_height + style.padding_y * 3.0
                        } else {
                            0.0
                        };
                        let terminal_h_sc = (win_h * 0.3)
                            .min(win_h - tab_h_sc - status_h_sc - 50.0)
                            .max(80.0);
                        let term_y_sc = win_h - terminal_h_sc - status_h_sc;
                        let sidebar_w_sc = if subsystems.has_sidebar() && sidebar_visible {
                            sidebar_width
                        } else {
                            0.0
                        };
                        let term_x_sc = sidebar_w_sc;
                        let term_w_sc = win_w - sidebar_w_sc;
                        let tab_bar_h_sc = if !terminal.terminals.is_empty() {
                            style.font_height + style.padding_y * 3.0
                        } else {
                            0.0
                        };
                        let char_h_sc = style.code_font_height * 1.2;
                        let ty_start = term_y_sc + style.divider_size + tab_bar_h_sc + 2.0;
                        let visible_h =
                            (term_y_sc + terminal_h_sc - ty_start - style.padding_y).max(0.0);
                        let rows_visible = (visible_h / char_h_sc).floor().max(1.0) as usize;
                        let sb_w_sc = style.scrollbar_size.max(6.0);
                        let sb_x_sc = term_x_sc + term_w_sc - sb_w_sc;
                        let sb_h_sc = char_h_sc * rows_visible as f64;
                        if *x >= sb_x_sc
                            && *x < sb_x_sc + sb_w_sc
                            && *y >= ty_start
                            && *y < ty_start + sb_h_sc
                        {
                            if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                                let cap = inst.tbuf.history_len() as f64;
                                if cap > 0.0 && sb_h_sc > 0.0 {
                                    let total = cap + rows_visible as f64;
                                    let ratio = (rows_visible as f64 / total).clamp(0.0, 1.0);
                                    let min_thumb = sb_w_sc * 2.0;
                                    let thumb_h = (sb_h_sc * ratio).max(min_thumb).min(sb_h_sc);
                                    let pos_from_top = (cap - inst.scrollback_target) / cap;
                                    let thumb_y = ty_start + pos_from_top * (sb_h_sc - thumb_h);
                                    if *y >= thumb_y && *y < thumb_y + thumb_h {
                                        terminal_sb_drag_offset = *y - thumb_y;
                                    } else {
                                        terminal_sb_drag_offset = thumb_h / 2.0;
                                        let new_top = (*y - thumb_h / 2.0)
                                            .clamp(ty_start, ty_start + sb_h_sc - thumb_h);
                                        let travel = (sb_h_sc - thumb_h).max(1.0);
                                        let new_from_top = (new_top - ty_start) / travel;
                                        inst.scrollback_target = (1.0 - new_from_top) * cap;
                                    }
                                    terminal_sb_dragging = true;
                                    redraw = true;
                                    continue;
                                }
                            }
                        }
                    }

                    // Test-runner badge hit-test: if the click lands on
                    // one of the inline "Run test" hints, dispatch a
                    // single-test run and skip caret placement.
                    if !test_badges.is_empty() {
                        let hit = test_badges
                            .iter()
                            .find(|r| *x >= r.x1 && *x < r.x2 && *y >= r.y1 && *y < r.y2);
                        if let Some(region) = hit {
                            if let Some(test) = active_tests.get(region.test_index) {
                                let doc_path = docs
                                    .get(active_tab)
                                    .map(|d| d.path.clone())
                                    .unwrap_or_default();
                                if !doc_path.is_empty() {
                                    pending_single_test = Some((doc_path, test.name.clone()));
                                    {
                                        let cmd: String = "test:run-single".to_string();
                                        include!("commands_dispatch.rs");
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
                    }

                    // Middle-click pastes the X11 PRIMARY selection at the
                    // click point, the standard Linux convention. Only acts
                    // inside the editor viewport rect; consumes the event so it
                    // never falls through to cursor placement / drag-select.
                    if *button == MouseButton::Middle {
                        if let Some(text) = crate::window::get_primary_selection_text()
                            && let Some(doc) = docs.get(active_tab)
                        {
                            let dv = &doc.view;
                            if let Some(buf_id) = dv.buffer_id {
                                let dvr = dv.rect();
                                let in_editor = *x >= dvr.x
                                    && *x < dvr.x + dvr.w
                                    && *y >= dvr.y
                                    && *y < dvr.y + dvr.h;
                                let large_read_only =
                                    crate::editor::open_doc::doc_policy(doc).read_only;
                                if in_editor && !large_read_only {
                                    let line_h = style.code_font_height * 1.2;
                                    let gutter_w = dv.gutter_width;
                                    let text_x_start =
                                        dv.rect().x + gutter_w + style.padding_x - dv.scroll_x;
                                    let (click_line, click_col) = click_to_doc_pos(
                                        dv,
                                        buf_id,
                                        &doc.cached_render,
                                        *x,
                                        *y,
                                        text_x_start,
                                        line_h,
                                        &style,
                                        &mut draw_ctx,
                                    );
                                    let text = if config.format_on_paste {
                                        convert_paste_indent(
                                            &text,
                                            &doc.indent_type,
                                            doc.indent_size,
                                        )
                                    } else {
                                        text
                                    };
                                    let _ = buffer::with_buffer_mut(buf_id, |b| {
                                        let line = click_line.min(b.lines.len()).max(1);
                                        let max_col =
                                            char_count(b.lines[line - 1].trim_end_matches('\n'))
                                                + 1;
                                        let col = click_col.min(max_col);
                                        b.selections = vec![line, col, line, col];
                                        insert_text_at_caret(b, &text);
                                        Ok(())
                                    });
                                } else if in_editor {
                                    info_message = Some((
                                        "Large file is read-only by configuration".to_string(),
                                        Instant::now(),
                                    ));
                                }
                            }
                        }
                        redraw = true;
                        continue;
                    }

                    if let Some(doc) = docs.get_mut(active_tab) {
                        let dv = &mut doc.view;
                        if let Some(buf_id) = dv.buffer_id {
                            // When the editor is split-paned with a preview,
                            // reject clicks that land outside its rect so
                            // cursor/selection math isn't fed stray coords.
                            let dvr = dv.rect();
                            if *x < dvr.x || *x >= dvr.x + dvr.w {
                                redraw = true;
                                continue;
                            }
                            let line_h = style.code_font_height * 1.2;
                            let gutter_w = dv.gutter_width;
                            let text_x_start =
                                dv.rect().x + gutter_w + style.padding_x - dv.scroll_x;
                            let (click_line, click_col) = click_to_doc_pos(
                                dv,
                                buf_id,
                                &doc.cached_render,
                                *x,
                                *y,
                                text_x_start,
                                line_h,
                                &style,
                                &mut draw_ctx,
                            );
                            let extending = shift_held || modifiers.shift;
                            let n_clicks = *clicks;
                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                let line = click_line.min(b.lines.len()).max(1);
                                let max_col =
                                    char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                                let col = click_col.min(max_col);
                                if n_clicks >= 3 && !extending {
                                    // Triple-click selects the whole clicked
                                    // line, matching Lite-XL's
                                    // `doc:set-cursor-line` binding.
                                    b.selections = vec![line, 1, line, max_col];
                                } else if n_clicks == 2 && !extending {
                                    // Double-click selects the word under the
                                    // cursor. Word chars are alphanumeric or
                                    // '_', matching the existing
                                    // word-movement commands.
                                    let text = b.lines[line - 1].trim_end_matches('\n');
                                    let chars: Vec<char> = text.chars().collect();
                                    let is_word = |c: char| c.is_alphanumeric() || c == '_';
                                    let idx = (col - 1).min(chars.len());
                                    if idx < chars.len() && is_word(chars[idx]) {
                                        let mut start = idx;
                                        while start > 0 && is_word(chars[start - 1]) {
                                            start -= 1;
                                        }
                                        let mut end = idx;
                                        while end < chars.len() && is_word(chars[end]) {
                                            end += 1;
                                        }
                                        b.selections = vec![line, start + 1, line, end + 1];
                                    } else if idx > 0 && is_word(chars[idx - 1]) {
                                        // Click landed just past the end of a
                                        // word (e.g. on trailing whitespace);
                                        // still select that word.
                                        let mut start = idx - 1;
                                        while start > 0 && is_word(chars[start - 1]) {
                                            start -= 1;
                                        }
                                        b.selections = vec![line, start + 1, line, idx + 1];
                                    } else {
                                        b.selections = vec![line, col, line, col];
                                    }
                                } else if extending && b.selections.len() >= 4 {
                                    // Shift+click extends the existing selection: keep the
                                    // anchor (selections[0..2]) and only move the cursor end.
                                    b.selections.truncate(4);
                                    b.selections[2] = line;
                                    b.selections[3] = col;
                                } else {
                                    b.selections = vec![line, col, line, col];
                                }
                                Ok(())
                            });
                            editor_mouse_down = true;
                        }
                    }
                    redraw = true;
                }
                EditorEvent::MouseMoved { x, y, .. } => {
                    mouse_x = *x;
                    mouse_y = *y;
                    // Hover highlight for the context menu (right-click on a
                    // tab, sidebar entry, doc area, or the tab-overflow
                    // dropdown). Without this `selected` only changes via
                    // keyboard up/down, so a freshly-opened menu had no
                    // active-row indicator.
                    if context_menu.visible {
                        use crate::editor::view::DrawContext as _;
                        let item_h = style.font_height + style.padding_y;
                        let menu_x = context_menu.position.x;
                        let menu_y = context_menu.position.y;
                        let mut menu_w = 0.0_f64;
                        for it in &context_menu.items {
                            let w = draw_ctx.font_width(style.font, &it.text);
                            let total_w = if let Some(ref info) = it.info {
                                w + draw_ctx.font_width(style.font, info) + style.padding_x * 3.0
                            } else {
                                w + style.padding_x * 2.0
                            };
                            menu_w = menu_w.max(total_w);
                        }
                        menu_w = menu_w.max(120.0);
                        let menu_h = item_h * context_menu.items.len() as f64 + style.padding_y;
                        let new_sel = if *x >= menu_x
                            && *x <= menu_x + menu_w
                            && *y >= menu_y
                            && *y <= menu_y + menu_h
                        {
                            let rel = (*y - menu_y - style.padding_y / 2.0) / item_h;
                            let idx = rel.floor().max(0.0) as usize;
                            if idx < context_menu.items.len() && !context_menu.items[idx].separator
                            {
                                Some(idx)
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        if context_menu.selected != new_sel {
                            context_menu.selected = new_sel;
                            redraw = true;
                        }
                    }
                    // Tab drag reorder.
                    if let Some(drag_idx) = tab_dragging {
                        let tab_h = style.font_height + style.padding_y * 3.0;
                        if *y < tab_h {
                            use crate::editor::view::DrawContext as _;
                            let sidebar_w = if sidebar_visible { sidebar_width } else { 0.0 };
                            let close_w =
                                draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                            // Match the draw pass: if the tab bar overflows, labels
                            // are truncated, so the drag hit-test must use the same
                            // widths or reorder lands on the wrong tab.
                            let (ww_dr, _, _, _) = crate::window::get_window_size();
                            let width = ww_dr as f64;
                            let dropdown_btn_w = (style.font_height + style.padding_x * 2.0).ceil();
                            let avail_full = (width - sidebar_w).max(0.0);
                            let mut full_total = 0.0_f64;
                            for doc in docs.iter() {
                                let l = if doc_is_modified(doc) {
                                    format!("*{}", doc.name)
                                } else {
                                    doc.name.clone()
                                };
                                full_total += draw_ctx.font_width(style.font, &l)
                                    + style.padding_x * 2.0
                                    + close_w
                                    + style.divider_size;
                            }
                            let tabs_overflow = full_total > avail_full;
                            let tabs_right_limit = if tabs_overflow {
                                (width - dropdown_btn_w).max(sidebar_w)
                            } else {
                                width
                            };
                            let mut tx = sidebar_w;
                            for (i, doc) in docs.iter().enumerate() {
                                let label = if tabs_overflow {
                                    let base = truncate_tab_name(&doc.name, 10);
                                    if doc_is_modified(doc) {
                                        format!("*{base}")
                                    } else {
                                        base
                                    }
                                } else if doc_is_modified(doc) {
                                    format!("*{}", doc.name)
                                } else {
                                    doc.name.clone()
                                };
                                let tw = draw_ctx.font_width(style.font, &label)
                                    + style.padding_x * 2.0
                                    + close_w
                                    + style.divider_size;
                                let hit_right = (tx + tw).min(tabs_right_limit);
                                if *x >= tx && *x < hit_right && i != drag_idx {
                                    docs.swap(i, drag_idx);
                                    tab_dragging = Some(i);
                                    active_tab = i;
                                    redraw = true;
                                    break;
                                }
                                tx += tw;
                                if tx >= tabs_right_limit {
                                    break;
                                }
                            }
                        }
                        continue;
                    }
                    // Markdown preview scrollbar drags. Same grip model as the
                    // editor: the point the thumb was grabbed at stays under
                    // the cursor.
                    if preview_sb_v_dragging || preview_sb_h_dragging {
                        if let Some(doc) = docs.get_mut(active_tab) {
                            if preview_sb_v_dragging {
                                crate::editor::markdown_preview::scroll_to_thumb_top(
                                    &mut doc.preview,
                                    &style,
                                    *y - preview_sb_v_drag_offset,
                                );
                            } else {
                                crate::editor::markdown_preview::scroll_to_thumb_left(
                                    &mut doc.preview,
                                    &style,
                                    *x - preview_sb_h_drag_offset,
                                );
                            }
                            redraw = true;
                        }
                        continue;
                    }

                    // Editor scrollbar drag: move the thumb so its grabbed
                    // point stays under the cursor, then derive scroll.
                    if editor_sb_dragging {
                        if let Some(doc) = docs.get_mut(active_tab) {
                            let dv_rect = doc.view.rect();
                            let line_h_sb = style.code_font_height * 1.2;
                            let total_lines = doc
                                .view
                                .buffer_id
                                .and_then(|id| buffer::with_buffer(id, |b| Ok(b.lines.len())).ok())
                                .unwrap_or(1) as f64;
                            let total_h = total_lines * line_h_sb;
                            if total_h > dv_rect.h && dv_rect.h > 0.0 {
                                let ratio = dv_rect.h / total_h;
                                let min_thumb = style.scrollbar_size * 2.0;
                                let thumb_h = (dv_rect.h * ratio).max(min_thumb).min(dv_rect.h);
                                let new_top = (*y - editor_sb_drag_offset)
                                    .clamp(dv_rect.y, dv_rect.y + dv_rect.h - thumb_h);
                                let travel = (dv_rect.h - thumb_h).max(1.0);
                                let new_frac = (new_top - dv_rect.y) / travel;
                                let new_scroll = (new_frac * (total_h - dv_rect.h)).max(0.0);
                                doc.view.target_scroll_y = new_scroll;
                                doc.view.scroll_y = new_scroll;
                                redraw = true;
                            }
                        }
                        continue;
                    }

                    // Terminal scrollbar drag: recompute scrollback from
                    // mouse y. Must come before the selection drag, so a
                    // mouse-down on the track doesn't turn into a cell
                    // selection on drag.
                    if terminal_sb_dragging && subsystems.has_terminal() && terminal.visible {
                        let (_, wh, _, _) = crate::window::get_window_size();
                        let win_h = wh as f64;
                        let status_h_sm = style.font_height + style.padding_y * 2.0;
                        let tab_h_sm = if !single_file_mode && !docs.is_empty() {
                            style.font_height + style.padding_y * 3.0
                        } else {
                            0.0
                        };
                        let terminal_h_sm = (win_h * 0.3)
                            .min(win_h - tab_h_sm - status_h_sm - 50.0)
                            .max(80.0);
                        let term_y_sm = win_h - terminal_h_sm - status_h_sm;
                        let tab_bar_h_sm = if !terminal.terminals.is_empty() {
                            style.font_height + style.padding_y * 3.0
                        } else {
                            0.0
                        };
                        let char_h_sm = style.code_font_height * 1.2;
                        let ty_start = term_y_sm + style.divider_size + tab_bar_h_sm + 2.0;
                        let visible_h =
                            (term_y_sm + terminal_h_sm - ty_start - style.padding_y).max(0.0);
                        let rows_visible = (visible_h / char_h_sm).floor().max(1.0) as usize;
                        let sb_h = char_h_sm * rows_visible as f64;
                        let sb_w_sm = style.scrollbar_size.max(6.0);
                        if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                            let cap = inst.tbuf.history_len() as f64;
                            if cap > 0.0 && sb_h > 0.0 {
                                let total = cap + rows_visible as f64;
                                let ratio = (rows_visible as f64 / total).clamp(0.0, 1.0);
                                let min_thumb = sb_w_sm * 2.0;
                                let thumb_h = (sb_h * ratio).max(min_thumb).min(sb_h);
                                let new_top = (*y - terminal_sb_drag_offset)
                                    .clamp(ty_start, ty_start + sb_h - thumb_h);
                                let travel = (sb_h - thumb_h).max(1.0);
                                let new_from_top = (new_top - ty_start) / travel;
                                inst.scrollback_target = (1.0 - new_from_top) * cap;
                                redraw = true;
                            }
                        }
                        continue;
                    }

                    // Terminal: extend the active selection while drag is in
                    // progress. Done before any other mouse-move branch
                    // because the terminal sits at the bottom of the
                    // window and its drag shouldn't trigger sidebar resize
                    // or editor caret drag.
                    if subsystems.has_terminal() && terminal.visible {
                        use crate::editor::view::DrawContext as _;
                        let (_, wh, _, _) = crate::window::get_window_size();
                        let win_h = wh as f64;
                        let status_h_m = style.font_height + style.padding_y * 2.0;
                        let tab_h_m = if !single_file_mode && !docs.is_empty() {
                            style.font_height + style.padding_y * 3.0
                        } else {
                            0.0
                        };
                        let terminal_h_m = (win_h * 0.3)
                            .min(win_h - tab_h_m - status_h_m - 50.0)
                            .max(80.0);
                        let term_y_m = win_h - terminal_h_m - status_h_m;
                        let sidebar_w_m = if subsystems.has_sidebar() && sidebar_visible {
                            sidebar_width
                        } else {
                            0.0
                        };
                        let term_x_m = sidebar_w_m;
                        let tab_bar_h_m = if !terminal.terminals.is_empty() {
                            style.font_height + style.padding_y * 3.0
                        } else {
                            0.0
                        };
                        let char_h_m = style.code_font_height * 1.2;
                        let char_w_m = draw_ctx.font_width(style.code_font, "m");
                        let ty_start = term_y_m + style.divider_size + tab_bar_h_m + 2.0;
                        let visible_h =
                            (term_y_m + terminal_h_m - ty_start - style.padding_y).max(0.0);
                        let rows_visible = (visible_h / char_h_m).floor().max(1.0) as usize;
                        if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                            if inst.sel_dragging && char_w_m > 0.0 {
                                let col = ((*x - term_x_m - style.padding_x) / char_w_m)
                                    .floor()
                                    .max(0.0) as usize;
                                let vis_row = (((*y - ty_start) / char_h_m).floor().max(0.0)
                                    as usize)
                                    .min(rows_visible.saturating_sub(1));
                                inst.sel_end = Some((vis_row, col));
                                redraw = true;
                            }
                        }
                    }
                    if sidebar_sb_dragging {
                        if sidebar_content_h > sidebar_sb_h && sidebar_sb_h > 0.0 {
                            let ratio = sidebar_sb_h / sidebar_content_h;
                            let min_thumb = style.scrollbar_size * 2.0;
                            let thumb_h = (sidebar_sb_h * ratio).max(min_thumb).min(sidebar_sb_h);
                            let new_top = (*y - sidebar_sb_drag_offset)
                                .clamp(sidebar_sb_top, sidebar_sb_top + sidebar_sb_h - thumb_h);
                            let travel = (sidebar_sb_h - thumb_h).max(1.0);
                            let new_frac = (new_top - sidebar_sb_top) / travel;
                            let max_scroll = (sidebar_content_h - sidebar_sb_h).max(1.0);
                            sidebar_scroll = (new_frac * max_scroll).max(0.0);
                            redraw = true;
                        }
                        continue;
                    }
                    if sidebar_dragging {
                        let (ww, _, _, _) = crate::window::get_window_size();
                        let max_sidebar = (ww as f64 * 0.9).max(MIN_SIDEBAR_W);
                        sidebar_width = x.clamp(MIN_SIDEBAR_W, max_sidebar);
                        redraw = true;
                    } else if preview_dragging {
                        // Recover the shared content area from the editor (left)
                        // and preview (right) rects so the split is expressed as
                        // a window-relative fraction that survives resizes.
                        if let Some(doc) = docs.get(active_tab) {
                            let content_x = doc.view.rect().x;
                            let content_right = doc.preview.rect.x + doc.preview.rect.w;
                            let content_w = (content_right - content_x).max(1.0);
                            preview_split = ((*x - content_x) / content_w)
                                .clamp(MIN_PREVIEW_SPLIT, MAX_PREVIEW_SPLIT);
                        }
                        redraw = true;
                    } else if editor_mouse_down {
                        // Drag selection: update cursor position while keeping anchor.
                        if let Some(doc) = docs.get_mut(active_tab) {
                            let dv = &mut doc.view;
                            if let Some(buf_id) = dv.buffer_id {
                                let line_h = style.code_font_height * 1.2;
                                let gutter_w = dv.gutter_width;
                                let text_x_start =
                                    dv.rect().x + gutter_w + style.padding_x - dv.scroll_x;
                                let (drag_line, drag_col) = click_to_doc_pos(
                                    dv,
                                    buf_id,
                                    &doc.cached_render,
                                    *x,
                                    *y,
                                    text_x_start,
                                    line_h,
                                    &style,
                                    &mut draw_ctx,
                                );
                                let _ = buffer::with_buffer_mut(buf_id, |b| {
                                    let line = drag_line.min(b.lines.len()).max(1);
                                    let max_col =
                                        char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                                    b.selections[2] = line;
                                    b.selections[3] = drag_col.min(max_col);
                                    Ok(())
                                });
                                redraw = true;
                            }
                        }
                    }
                    let sidebar_w = if sidebar_visible { sidebar_width } else { 0.0 };
                    // Hand cursor when hovering a markdown preview link.
                    let hover_link =
                        docs.get(active_tab)
                            .map(|d| {
                                d.preview.enabled
                                    && d.preview.link_regions.iter().any(|r| {
                                        *x >= r.x1 && *x <= r.x2 && *y >= r.y1 && *y <= r.y2
                                    })
                            })
                            .unwrap_or(false);
                    let hover_preview_divider = docs
                        .get(active_tab)
                        .map(|d| {
                            d.preview.enabled
                                && d.preview.rect.w > 0.0
                                && (*x - d.preview.rect.x).abs() < 5.0
                        })
                        .unwrap_or(false);
                    if hover_link {
                        crate::window::set_cursor("hand");
                    } else if (subsystems.has_sidebar()
                        && sidebar_visible
                        && (*x - sidebar_w).abs() < 5.0)
                        || hover_preview_divider
                        || preview_dragging
                    {
                        crate::window::set_cursor("sizeh");
                    } else if !sidebar_dragging && !editor_mouse_down && !preview_dragging {
                        crate::window::set_cursor("arrow");
                    } else if editor_mouse_down {
                        crate::window::set_cursor("ibeam");
                    }

                    // Hover tooltip tracking: map the cursor to a
                    // (line, col) over the active doc. If a diagnostic
                    // is under the cursor, surface its message
                    // immediately. Otherwise note the position + time
                    // so the debounce loop below can fire a deferred
                    // `textDocument/hover` request.
                    if diagnostic_hover_region_rect.is_some_and(|rect| point_in_rect(rect, *x, *y))
                    {
                        diagnostic_hover_leave_at = None;
                        continue;
                    }
                    let new_doc_pos: Option<(usize, usize)> = (|| {
                        if editor_mouse_down || sidebar_dragging {
                            return None;
                        }
                        let doc = docs.get(active_tab)?;
                        let buf_id = doc.view.buffer_id?;
                        let dv = &doc.view;
                        let dvr = dv.rect();
                        if *x < dvr.x || *x >= dvr.x + dvr.w || *y < dvr.y || *y >= dvr.y + dvr.h {
                            return None;
                        }
                        let line_h = style.code_font_height * 1.2;
                        let gutter_w = dv.gutter_width;
                        let text_x_start = dv.rect().x + gutter_w + style.padding_x - dv.scroll_x;
                        if *x < text_x_start - style.padding_x {
                            return None;
                        }
                        let (line, col) = click_to_doc_pos(
                            dv,
                            buf_id,
                            &doc.cached_render,
                            *x,
                            *y,
                            text_x_start,
                            line_h,
                            &style,
                            &mut draw_ctx,
                        );
                        Some((line, col))
                    })();
                    if new_doc_pos != mouse_doc_pos {
                        mouse_doc_pos = new_doc_pos;
                        mouse_idle_since = Some(Instant::now());
                        pending_lsp_hover_request = None;
                        if hover.visible && hovered_diagnostic.is_none() {
                            hover.hide();
                            hovered_diagnostic = None;
                            diagnostic_hover_region_rect = None;
                            diagnostic_hover_leave_at = None;
                            diagnostic_quick_fix_rect = None;
                            diagnostic_hover_action_request = None;
                            diagnostic_hover_actions.clear();
                            redraw = true;
                        } else if hovered_diagnostic.is_some() {
                            // Match VS Code's sticky hover behavior: pointer
                            // movement toward the popup gets a short grace
                            // period instead of dismissing on the first pixel
                            // outside the diagnostic range.
                            diagnostic_hover_leave_at =
                                Some(Instant::now() + std::time::Duration::from_millis(450));
                            mouse_idle_since = None;
                        }
                        // Immediate diagnostic tooltip.
                        let matched_diagnostic = if let Some((line, col)) = new_doc_pos
                            && subsystems.has_lsp()
                            && let Some(doc) = docs.get(active_tab)
                            && let Some(diags) = lsp_state.diagnostics.get(&doc.path)
                        {
                            let l0 = line.saturating_sub(1);
                            let c0 = col.saturating_sub(1);
                            diags.iter().find_map(|d| {
                                let in_line =
                                    d.start_line <= l0 && l0 <= d.end_line.max(d.start_line);
                                let span_end = d.end_col.max(d.start_col + 1);
                                let in_col = if d.start_line == d.end_line && d.start_line == l0 {
                                    c0 >= d.start_col && c0 < span_end
                                } else if l0 == d.start_line {
                                    c0 >= d.start_col
                                } else if l0 == d.end_line {
                                    c0 < d.end_col
                                } else {
                                    true
                                };
                                if in_line && in_col && !d.message.is_empty() {
                                    Some((
                                        d.message.clone(),
                                        doc.path.clone(),
                                        d.start_line,
                                        d.start_col,
                                        d.end_line,
                                        d.end_col,
                                        d.raw.clone(),
                                    ))
                                } else {
                                    None
                                }
                            })
                        } else {
                            None
                        };
                        if let (Some((line, col)), Some((message, path, sl, sc, el, ec, raw))) =
                            (new_doc_pos, matched_diagnostic)
                        {
                            let diagnostic_identity = (path.clone(), sl, sc, el, ec);
                            let changed_diagnostic =
                                hovered_diagnostic.as_ref() != Some(&diagnostic_identity);
                            hover.text = message;
                            hover.line = line;
                            hover.col = col;
                            hover.visible = true;
                            hovered_diagnostic = Some(diagnostic_identity);
                            diagnostic_hover_leave_at = None;
                            if changed_diagnostic {
                                diagnostic_hover_actions.clear();
                                diagnostic_hover_action_request = None;
                            }
                            if changed_diagnostic
                                && lsp_state.initialized
                                && let Some(tid) = lsp_state.transport_id
                            {
                                let request_id = lsp_state.next_id();
                                lsp_state.pending_requests.insert(
                                    request_id,
                                    "textDocument/codeAction/hover".to_string(),
                                );
                                let _ = lsp::send_message(
                                    tid,
                                    &lsp_code_action_request(
                                        request_id,
                                        &path_to_uri(&path),
                                        sl,
                                        sc,
                                        el,
                                        ec,
                                        serde_json::Value::Array(vec![raw]),
                                    ),
                                );
                                diagnostic_hover_action_request = Some(request_id);
                            }
                            // Do not also fire LSP hover for this position.
                            last_lsp_hover_pos = Some((line, col));
                            mouse_idle_since = None;
                            redraw = true;
                        }
                    }
                    continue;
                }
                EditorEvent::MouseReleased { .. } => {
                    if sidebar_dragging {
                        sidebar_dragging = false;
                        let _ = crate::editor::storage::save_text(
                            userdir_path,
                            "session",
                            "sidebar_width",
                            &sidebar_width.to_string(),
                        );
                    }
                    if preview_dragging {
                        preview_dragging = false;
                        let _ = crate::editor::storage::save_text(
                            userdir_path,
                            "session",
                            "preview_split",
                            &preview_split.to_string(),
                        );
                    }
                    editor_mouse_down = false;
                    tab_dragging = None;
                    editor_sb_dragging = false;
                    terminal_sb_dragging = false;
                    sidebar_sb_dragging = false;
                    preview_sb_v_dragging = false;
                    preview_sb_h_dragging = false;
                    // End terminal selection drag; the selection itself
                    // stays visible until dismissed by another click or
                    // the escape / copy key.
                    if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                        inst.sel_dragging = false;
                    }
                    redraw = true;
                    continue;
                }
                EditorEvent::MouseWheel { x: wheel_x, y } => {
                    let line_h = style.code_font_height * 1.2;
                    let scroll_amt = y * line_h * 3.0;
                    let h_scroll_amt = wheel_x * line_h * 3.0;
                    // Wheel routes to the terminal panel when the cursor is over it.
                    let over_terminal = subsystems.has_terminal() && terminal.visible && {
                        let (_, wh, _, _) = crate::window::get_window_size();
                        let win_h = wh as f64;
                        let status_h_c = style.font_height + style.padding_y * 2.0;
                        let tab_h_c = if !single_file_mode && !docs.is_empty() {
                            style.font_height + style.padding_y * 3.0
                        } else {
                            0.0
                        };
                        let terminal_h_c = (win_h * 0.3)
                            .min(win_h - tab_h_c - status_h_c - 50.0)
                            .max(80.0);
                        let term_y_c = win_h - terminal_h_c - status_h_c;
                        mouse_y >= term_y_c && mouse_y < win_h - status_h_c
                    };
                    if over_terminal {
                        if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                            // Positive wheel y walks up into history.
                            let delta = *y * 3.0;
                            let cap = inst.tbuf.history_len() as f64;
                            inst.scrollback_target =
                                (inst.scrollback_target + delta).clamp(0.0, cap);
                        }
                        redraw = true;
                        continue;
                    }
                    if subsystems.has_sidebar() && sidebar_visible && mouse_x < sidebar_width {
                        // Mouse is over the sidebar -- scroll sidebar only.
                        let max_scroll = (sidebar_content_h - sidebar_sb_h).max(0.0);
                        sidebar_scroll = (sidebar_scroll - scroll_amt).clamp(0.0, max_scroll);
                    } else if let Some(doc) = docs.get_mut(active_tab) {
                        // Route wheel to whichever pane the cursor is over
                        // in split preview mode.
                        let over_preview = doc.preview.enabled
                            && doc.preview.rect.w > 0.0
                            && mouse_x >= doc.preview.rect.x
                            && mouse_x < doc.preview.rect.x + doc.preview.rect.w;
                        if over_preview {
                            // Shift turns a vertical wheel into horizontal
                            // scrolling, the usual convention for panes with
                            // both axes; a horizontal wheel or trackpad swipe
                            // scrolls sideways on its own.
                            let (dx, dy) = if shift_held {
                                (h_scroll_amt - scroll_amt, 0.0)
                            } else {
                                (h_scroll_amt, -scroll_amt)
                            };
                            crate::editor::markdown_preview::scroll_by(&mut doc.preview, dx, dy);
                        } else {
                            let dv = &mut doc.view;
                            dv.target_scroll_y = (dv.target_scroll_y - scroll_amt).max(0.0);
                        }
                    }
                    redraw = true;
                }
                _ => {
                    redraw = true;
                }
            }
        }

        // Mirror the active editor selection into the X11 PRIMARY selection so
        // middle-click paste (here and in other apps) uses the current
        // selection, matching standard Linux behavior. A no-op on platforms
        // without a primary selection.
        if had_input_events
            && let Some(doc) = docs.get(active_tab)
            && let Some(buf_id) = doc.view.buffer_id
        {
            let coords =
                buffer::with_buffer(buf_id, |b| Ok(b.selections.clone())).unwrap_or_default();
            let key = (buf_id, coords);
            if key != last_primary_key {
                last_primary_key = key;
                let selected = buffer::with_buffer(buf_id, |b| Ok(buffer::get_selected_text(b)))
                    .unwrap_or_default();
                if !selected.is_empty() {
                    crate::window::set_primary_selection_text(&selected);
                }
            }
        }

        // The active state is scoped to one language and project root. Keep
        // inactive states alive so tab switches reuse their indexed server.
        if subsystems.has_lsp()
            && let Some(doc) = docs.get(active_tab)
            && !doc.path.is_empty()
        {
            let extension = doc.path.rsplit('.').next().unwrap_or("");
            let active_filetype = ext_to_lsp_filetype(extension);
            let desired_root_uri = active_filetype
                .and_then(|filetype| find_lsp_spec(filetype, &lsp_specs))
                .map(|spec| {
                    let file_dir = Path::new(&doc.path)
                        .parent()
                        .and_then(Path::to_str)
                        .unwrap_or(".");
                    path_to_uri(
                        &find_project_root(file_dir, &spec.root_patterns)
                            .unwrap_or_else(|| file_dir.to_string()),
                    )
                });
            if let (Some(filetype), Some(root_uri)) = (active_filetype, desired_root_uri)
                && (filetype != lsp_state.filetype || root_uri != lsp_state.root_uri)
            {
                activate_lsp_state(&mut lsp_state, &mut inactive_lsp_states, filetype, root_uri);
            }
        }

        // LSP: auto-start for the active file if no transport is running.
        if subsystems.has_lsp()
            && lsp_state.transport_id.is_none()
            && lsp_state.should_attempt_spawn()
            && let Some(doc) = docs.get(active_tab)
            && !doc.path.is_empty()
            && !(crate::editor::open_doc::doc_policy(doc).disable_lsp)
        {
            try_start_lsp(
                &doc.path,
                &mut lsp_state,
                &lsp_specs,
                userdir,
                config.verbose,
            );
        }

        // Poll background file load. If the thread is done, swap the buffer in.
        if let Some(job) = load_job.as_mut() {
            // Always redraw while a load is active so the progress bar animates.
            redraw = true;
            let finished = job.handle.as_ref().map(|h| h.is_finished()).unwrap_or(true);
            if finished {
                let mut j = load_job.take().unwrap();
                match j.handle.take().unwrap().join() {
                    Ok(Ok(state)) => {
                        let (indent_type, indent_size, _score) =
                            crate::editor::picker::detect_indent(&state.lines, 100, 2);
                        let initial_change_id = state.change_id;
                        let buf_id = buffer::insert_buffer(state);
                        let mut dv = DocView::new();
                        dv.buffer_id = Some(buf_id);
                        dv.indent_size = indent_size;
                        let saved_sig = buffer::with_buffer(buf_id, |b| {
                            Ok(buffer::content_signature(&b.lines))
                        })
                        .unwrap_or(0);
                        docs.push(OpenDoc {
                            view: dv,
                            path: j.path.clone(),
                            name: j.name.clone(),
                            saved_change_id: initial_change_id,
                            saved_signature: saved_sig,
                            indent_type: indent_type.to_string(),
                            indent_size,
                            git_changes: std::collections::HashMap::new(),
                            cached_render: std::sync::Arc::new(Vec::new()),
                            cached_change_id: -1,
                            cached_scroll_y: -1.0,
                            cached_hint_count: 0,
                            cached_rect_w: -1.0,
                            cached_rect_h: -1.0,
                            dirty_cache: std::cell::Cell::new(None),
                            canonical_cache: Default::default(),
                            token_cache: std::cell::RefCell::new(
                                crate::editor::open_doc::TokenCache::default(),
                            ),
                            preview: crate::editor::markdown_preview::MarkdownPreviewState::default(
                            ),
                        });
                        active_tab = docs.len() - 1;
                        autoreload.watch(&j.path);
                        remember_recent_file(&mut recent_files, &j.path, userdir_path);
                    }
                    Ok(Err(e)) => {
                        info_message = Some((format!("Load failed: {e}"), Instant::now()));
                    }
                    Err(_) => {
                        info_message = Some(("Load thread panicked".to_string(), Instant::now()));
                    }
                }
            }
        }

        // LSP: poll transport and handle responses.
        if subsystems.has_lsp() {
            if let Some(tid) = lsp_state.transport_id {
                // Request fresh inlay hints whenever the active file
                // changes identity from what `inlay_hints_uri` records.
                if lsp_state.initialized {
                    if let Some(doc) = docs.get(active_tab) {
                        if !doc.path.is_empty() {
                            let ext = doc.path.rsplit('.').next().unwrap_or("");
                            let is_lsp_file = ext_to_lsp_filetype(ext)
                                .map(|ft| ft == lsp_state.filetype)
                                .unwrap_or(false);
                            if is_lsp_file {
                                let uri = path_to_uri(&doc.path);
                                if !lsp_state.opened_documents.contains(&uri)
                                    && let Some(buffer_id) = doc.view.buffer_id
                                {
                                    let text = buffer::with_buffer(buffer_id, |buffer| {
                                        Ok(buffer.lines.join(""))
                                    })
                                    .unwrap_or_default();
                                    let _ = lsp::send_message(
                                        tid,
                                        &lsp_did_open(
                                            &uri,
                                            lsp_language_id(&lsp_state.filetype),
                                            &text,
                                        ),
                                    );
                                    lsp_state.opened_documents.insert(uri.clone());
                                    lsp_state.document_texts.insert(uri.clone(), text);
                                    request_lsp_diagnostics(tid, &mut lsp_state, &uri);
                                }
                                let already_pending =
                                    lsp_state.pending_request_uris.values().any(|u| u == &uri);
                                if lsp_state.inlay_hints_uri != uri && !already_pending {
                                    let line_count = doc
                                        .view
                                        .buffer_id
                                        .and_then(|id| {
                                            buffer::with_buffer(id, |b| Ok(b.lines.len())).ok()
                                        })
                                        .unwrap_or(100);
                                    let req_id = lsp_state.next_id();
                                    lsp_state
                                        .pending_requests
                                        .insert(req_id, "textDocument/inlayHint".to_string());
                                    lsp_state.pending_request_uris.insert(req_id, uri.clone());
                                    let _ = lsp::send_message(
                                        tid,
                                        &lsp_inlay_hint_request(req_id, &uri, 0, line_count),
                                    );
                                    lsp_state.inlay_hints.clear();
                                    lsp_state.inlay_hints_uri = uri;
                                    for d in &mut docs {
                                        d.cached_change_id = -1;
                                    }
                                }
                            }
                        }
                    }
                }
                // Retry timer for inlay hints while the server is still indexing.
                if let Some(retry_at) = lsp_state.inlay_retry_at {
                    if Instant::now() >= retry_at {
                        lsp_state.inlay_retry_at = None;
                        if let Some(doc) = docs.get(active_tab) {
                            if !doc.path.is_empty() {
                                let ext = doc.path.rsplit('.').next().unwrap_or("");
                                let is_lsp_file = ext_to_lsp_filetype(ext)
                                    .map(|ft| ft == lsp_state.filetype)
                                    .unwrap_or(false);
                                if is_lsp_file {
                                    let uri = path_to_uri(&doc.path);
                                    let line_count = doc
                                        .view
                                        .buffer_id
                                        .and_then(|id| {
                                            buffer::with_buffer(id, |b| Ok(b.lines.len())).ok()
                                        })
                                        .unwrap_or(100);
                                    let req_id = lsp_state.next_request_id;
                                    lsp_state.next_request_id += 1;
                                    lsp_state
                                        .pending_requests
                                        .insert(req_id, "textDocument/inlayHint".to_string());
                                    lsp_state.pending_request_uris.insert(req_id, uri.clone());
                                    let _ = lsp::send_message(
                                        tid,
                                        &lsp_inlay_hint_request(req_id, &uri, 0, line_count),
                                    );
                                }
                            }
                        }
                    }
                }
                if lsp_state
                    .diagnostic_retry_at
                    .is_some_and(|retry_at| Instant::now() >= retry_at)
                {
                    lsp_state.diagnostic_retry_at = None;
                    if let Some(doc) = docs.get(active_tab)
                        && !doc.path.is_empty()
                    {
                        request_lsp_diagnostics(tid, &mut lsp_state, &path_to_uri(&doc.path));
                    }
                }
                if let Ok(poll) = lsp::poll_transport(tid) {
                    for msg in &poll.messages {
                        // Server-to-client `workspace/applyEdit` request: apply the
                        // edit and acknowledge so the server does not block waiting.
                        if msg.get("method").and_then(|m| m.as_str()) == Some("workspace/applyEdit")
                        {
                            let atomic = config.files.atomic_save;
                            let applied = msg
                                .get("params")
                                .and_then(|p| p.get("edit"))
                                .map(|e| apply_lsp_workspace_edit(e, &mut docs, use_git(), atomic))
                                .unwrap_or(0);
                            if applied > 0 {
                                for d in &mut docs {
                                    d.cached_change_id = -1;
                                }
                                crate::window::force_invalidate();
                                redraw = true;
                            }
                            if let (Some(rid), Some(tid)) = (msg.get("id"), lsp_state.transport_id)
                            {
                                let _ = lsp::send_message(
                                    tid,
                                    &serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": rid.clone(),
                                        "result": { "applied": applied > 0 }
                                    }),
                                );
                            }
                            continue;
                        }
                        // Answer server-to-client requests. Roslyn uses these
                        // during startup and can wait indefinitely if the client
                        // advertises support but never sends a response.
                        if let (Some(method), Some(request_id)) = (
                            msg.get("method").and_then(|value| value.as_str()),
                            msg.get("id"),
                        ) {
                            let mut request_diagnostics = false;
                            let response = match method {
                                "workspace/configuration" => {
                                    let settings = find_lsp_spec(&lsp_state.filetype, &lsp_specs)
                                        .and_then(|spec| spec.settings.as_ref());
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": request_id.clone(),
                                        "result": lsp_configuration_values(
                                            msg.pointer("/params/items"),
                                            settings,
                                        )
                                    })
                                }
                                "workspace/workspaceFolders" => serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": request_id.clone(),
                                    "result": [{
                                        "uri": lsp_state.root_uri,
                                        "name": uri_to_path(&lsp_state.root_uri)
                                            .rsplit(std::path::MAIN_SEPARATOR)
                                            .next()
                                            .unwrap_or("workspace")
                                    }]
                                }),
                                "client/registerCapability" => {
                                    if let Some(sync_kind) = msg
                                        .pointer("/params/registrations")
                                        .and_then(|value| value.as_array())
                                        .and_then(|registrations| {
                                            registrations.iter().find_map(|registration| {
                                                (registration
                                                    .get("method")
                                                    .and_then(|value| value.as_str())
                                                    == Some("textDocument/didChange"))
                                                .then(|| {
                                                    registration
                                                        .pointer("/registerOptions/syncKind")
                                                        .and_then(serde_json::Value::as_i64)
                                                })
                                                .flatten()
                                            })
                                        })
                                    {
                                        lsp_state.incremental_sync = sync_kind == 2;
                                    }
                                    if msg
                                        .pointer("/params/registrations")
                                        .and_then(|value| value.as_array())
                                        .is_some_and(|registrations| {
                                            registrations.iter().any(|registration| {
                                                registration
                                                    .get("method")
                                                    .and_then(|value| value.as_str())
                                                    == Some("textDocument/diagnostic")
                                            })
                                        })
                                    {
                                        lsp_state.pull_diagnostics = true;
                                        request_diagnostics = true;
                                    }
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": request_id.clone(),
                                        "result": null
                                    })
                                }
                                "client/unregisterCapability"
                                | "window/workDoneProgress/create"
                                | "window/showMessageRequest"
                                | "workspace/inlayHint/refresh"
                                | "workspace/semanticTokens/refresh" => serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": request_id.clone(),
                                    "result": null
                                }),
                                "workspace/diagnostic/refresh" => {
                                    request_diagnostics = true;
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": request_id.clone(),
                                        "result": null
                                    })
                                }
                                _ => serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": request_id.clone(),
                                    "error": {
                                        "code": -32601,
                                        "message": format!("Unsupported client method: {method}")
                                    }
                                }),
                            };
                            let _ = lsp::send_message(tid, &response);
                            if request_diagnostics
                                && let Some(doc) = docs.get(active_tab)
                                && !doc.path.is_empty()
                            {
                                request_lsp_diagnostics(
                                    tid,
                                    &mut lsp_state,
                                    &path_to_uri(&doc.path),
                                );
                            }
                            continue;
                        }
                        // Handle initialize response.
                        if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("initialize")
                            {
                                lsp_state.pending_requests.remove(&id);
                                lsp_state.code_action_resolve_provider = msg
                                    .pointer(
                                        "/result/capabilities/codeActionProvider/resolveProvider",
                                    )
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                // LSP capability advertisements are not reliable here:
                                // current Roslyn accepts `textDocument/diagnostic` but
                                // omits `diagnosticProvider`. Probe every server once and
                                // disable only after an explicit method-not-found response.
                                lsp_state.pull_diagnostics = should_probe_pull_diagnostics();
                                lsp_state.incremental_sync = msg
                                    .pointer("/result/capabilities/textDocumentSync/change")
                                    .and_then(serde_json::Value::as_i64)
                                    .or_else(|| {
                                        msg.pointer("/result/capabilities/textDocumentSync")
                                            .and_then(serde_json::Value::as_i64)
                                    })
                                    == Some(2);
                                lsp_state.initialized = true;
                                lsp_state.note_spawn_success();
                                // Send initialized notification.
                                let _ = lsp::send_message(
                                    tid,
                                    &serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "method": "initialized",
                                        "params": {}
                                    }),
                                );
                                if let Some(settings) =
                                    find_lsp_spec(&lsp_state.filetype, &lsp_specs)
                                        .and_then(|spec| spec.settings.as_ref())
                                {
                                    let _ = lsp::send_message(
                                        tid,
                                        &serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "method": "workspace/didChangeConfiguration",
                                            "params": { "settings": settings }
                                        }),
                                    );
                                }
                                if lsp_state.filetype == "c#"
                                    && let Some(notification) =
                                        csharp_project_open_notification(&lsp_state.root_uri)
                                {
                                    let _ = lsp::send_message(tid, &notification);
                                }
                                // Announce files matching the LSP filetype.
                                // The active tab goes now, so the file the
                                // user is looking at is served immediately;
                                // the rest queue and drain a few per frame.
                                let mut to_open: Vec<String> = docs
                                    .iter()
                                    .filter(|doc| {
                                        !doc.path.is_empty()
                                            && ext_to_lsp_filetype(
                                                doc.path.rsplit('.').next().unwrap_or(""),
                                            ) == Some(lsp_state.filetype.as_str())
                                    })
                                    .map(|doc| doc.path.clone())
                                    .collect();
                                if let Some(active_path) =
                                    docs.get(active_tab).map(|doc| doc.path.clone())
                                    && let Some(pos) =
                                        to_open.iter().position(|p| *p == active_path)
                                {
                                    let active_path = to_open.remove(pos);
                                    announce_document_to_lsp(
                                        tid,
                                        &mut lsp_state,
                                        &docs,
                                        &active_path,
                                    );
                                }
                                lsp_state.pending_did_open.extend(to_open);
                                // Request inlay hints only for the active file if it matches LSP.
                                if let Some(doc) = docs.get(active_tab) {
                                    let ext = doc.path.rsplit('.').next().unwrap_or("");
                                    if ext_to_lsp_filetype(ext)
                                        .map(|ft| ft == lsp_state.filetype)
                                        .unwrap_or(false)
                                    {
                                        let uri = path_to_uri(&doc.path);
                                        let line_count = doc
                                            .view
                                            .buffer_id
                                            .and_then(|id| {
                                                buffer::with_buffer(id, |b| Ok(b.lines.len())).ok()
                                            })
                                            .unwrap_or(100);
                                        let req_id = lsp_state.next_id();
                                        lsp_state
                                            .pending_requests
                                            .insert(req_id, "textDocument/inlayHint".to_string());
                                        lsp_state.pending_request_uris.insert(req_id, uri.clone());
                                        let _ = lsp::send_message(
                                            tid,
                                            &lsp_inlay_hint_request(req_id, &uri, 0, line_count),
                                        );
                                    }
                                }
                            }

                            if lsp_state.pending_requests.get(&id).map(String::as_str)
                                == Some("textDocument/diagnostic")
                            {
                                lsp_state.pending_requests.remove(&id);
                                let uri = lsp_state
                                    .pending_request_uris
                                    .remove(&id)
                                    .unwrap_or_default();
                                if pull_diagnostics_are_unsupported(msg) {
                                    lsp_state.pull_diagnostics = false;
                                    lsp_state.diagnostic_retry_at = None;
                                    lsp_state.diagnostic_retry_count = 0;
                                    continue;
                                }
                                if let Some(result_id) = msg
                                    .pointer("/result/resultId")
                                    .and_then(|value| value.as_str())
                                {
                                    lsp_state
                                        .diagnostic_result_ids
                                        .insert(uri.clone(), result_id.to_string());
                                }
                                let is_unchanged =
                                    msg.pointer("/result/kind").and_then(|value| value.as_str())
                                        == Some("unchanged");
                                if !uri.is_empty() && !is_unchanged {
                                    let diagnostics = msg
                                        .pointer("/result/items")
                                        .and_then(|value| value.as_array())
                                        .map(|items| {
                                            items
                                                .iter()
                                                .map(Diagnostic::from_lsp)
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default();
                                    let path = uri_to_path(&uri);
                                    let pull_is_empty = diagnostics.is_empty();
                                    lsp_state.update_pull_diagnostics(path.clone(), diagnostics);
                                    let has_pushed_diagnostics = lsp_state
                                        .push_diagnostics
                                        .get(&path)
                                        .is_some_and(|diagnostics| !diagnostics.is_empty());
                                    if pull_is_empty && !has_pushed_diagnostics {
                                        if lsp_state.diagnostic_retry_count < 20 {
                                            lsp_state.diagnostic_retry_count += 1;
                                            lsp_state.diagnostic_retry_at = Some(
                                                Instant::now() + std::time::Duration::from_secs(1),
                                            );
                                        }
                                    } else {
                                        lsp_state.diagnostic_retry_count = 0;
                                        lsp_state.diagnostic_retry_at = None;
                                    }
                                    redraw = true;
                                }
                            }

                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("textDocument/inlayHint")
                            {
                                lsp_state.pending_requests.remove(&id);
                                let req_uri = lsp_state
                                    .pending_request_uris
                                    .remove(&id)
                                    .unwrap_or_default();
                                let active_uri = docs
                                    .get(active_tab)
                                    .filter(|d| !d.path.is_empty())
                                    .map(|d| path_to_uri(&d.path))
                                    .unwrap_or_default();
                                if !req_uri.is_empty() && req_uri != active_uri {
                                    continue;
                                }
                                if let Some(result) = msg.get("result").and_then(|r| r.as_array()) {
                                    let mut new_hints: Vec<InlayHint> =
                                        Vec::with_capacity(result.len());
                                    for hint in result {
                                        let line = hint
                                            .get("position")
                                            .and_then(|p| p.get("line"))
                                            .and_then(|l| l.as_i64())
                                            .unwrap_or(0)
                                            as usize;
                                        let col = hint
                                            .get("position")
                                            .and_then(|p| p.get("character"))
                                            .and_then(|c| c.as_i64())
                                            .unwrap_or(0)
                                            as usize;
                                        let label = if let Some(s) =
                                            hint.get("label").and_then(|l| l.as_str())
                                        {
                                            s.to_string()
                                        } else if let Some(parts) =
                                            hint.get("label").and_then(|l| l.as_array())
                                        {
                                            parts
                                                .iter()
                                                .filter_map(|p| {
                                                    p.get("value").and_then(|v| v.as_str())
                                                })
                                                .collect::<Vec<_>>()
                                                .join("")
                                        } else {
                                            continue;
                                        };
                                        let padding_left = hint
                                            .get("paddingLeft")
                                            .and_then(|p| p.as_bool())
                                            .unwrap_or(true);
                                        let padding_right = hint
                                            .get("paddingRight")
                                            .and_then(|p| p.as_bool())
                                            .unwrap_or(false);
                                        let mut display = label;
                                        if padding_left {
                                            display = format!(" {display}");
                                        }
                                        if padding_right {
                                            display = format!("{display} ");
                                        }
                                        new_hints.push(InlayHint {
                                            line,
                                            col,
                                            label: display,
                                        });
                                    }
                                    let uri_changed = lsp_state.inlay_hints_uri != req_uri;
                                    let content_changed = uri_changed
                                        || lsp_state.inlay_hints.len() != new_hints.len()
                                        || lsp_state.inlay_hints.iter().zip(new_hints.iter()).any(
                                            |(a, b)| {
                                                a.line != b.line
                                                    || a.col != b.col
                                                    || a.label != b.label
                                            },
                                        );
                                    let empty = new_hints.is_empty();
                                    lsp_state.inlay_hints = new_hints;
                                    lsp_state.inlay_hints_uri = req_uri.clone();
                                    if empty {
                                        if lsp_state.inlay_retry_count < 20 {
                                            lsp_state.inlay_retry_at = Some(
                                                Instant::now() + std::time::Duration::from_secs(2),
                                            );
                                            lsp_state.inlay_retry_count += 1;
                                        }
                                    } else {
                                        lsp_state.inlay_retry_count = 0;
                                        lsp_state.inlay_retry_at = None;
                                    }
                                    if content_changed {
                                        pending_render_cache = None;
                                        for d in &mut docs {
                                            d.cached_change_id = -1;
                                        }
                                        crate::window::force_invalidate();
                                    }
                                    redraw = true;
                                }
                            }

                            // Handle completion response.
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("textDocument/completion")
                            {
                                lsp_state.pending_requests.remove(&id);
                                // Drop responses for any request older than the
                                // latest one we sent — LSP servers may answer
                                // out of order, and a slow stale reply (with a
                                // shorter prefix) would otherwise clobber a
                                // fresher list. Mirrors the inlay-hint
                                // late-response gate.
                                if id != completion.latest_request_id {
                                    continue;
                                }
                                // Re-derive the word-prefix at the cursor RIGHT
                                // NOW (the user may have typed more characters
                                // between the request being sent and this
                                // reply). The LSP server already filters by
                                // its own prefix-snapshot; we re-filter
                                // client-side so the popup never shows an
                                // item that doesn't match the current word.
                                let prefix_now: String = docs
                                    .get(active_tab)
                                    .and_then(|d| d.view.buffer_id)
                                    .and_then(|bid| {
                                        buffer::with_buffer(bid, |b| {
                                            let l = *b.selections.get(2).unwrap_or(&1);
                                            let c = *b.selections.get(3).unwrap_or(&1);
                                            let line = b
                                                .lines
                                                .get(l - 1)
                                                .map(String::as_str)
                                                .unwrap_or("");
                                            let chars: Vec<char> = line.chars().collect();
                                            let col = (c - 1).min(chars.len());
                                            let mut start = col;
                                            while start > 0 {
                                                let ch = chars[start - 1];
                                                if ch.is_alphanumeric() || ch == '_' {
                                                    start -= 1;
                                                } else {
                                                    break;
                                                }
                                            }
                                            Ok(chars[start..col].iter().collect::<String>())
                                        })
                                        .ok()
                                    })
                                    .unwrap_or_default();
                                let mut items: Vec<(String, String, String)> = Vec::new();
                                let result = msg.get("result");
                                // result can be an array or {items: [...]}.
                                let item_arr = result
                                    .and_then(|r| {
                                        r.as_array().cloned().or_else(|| {
                                            r.get("items").and_then(|v| v.as_array()).cloned()
                                        })
                                    })
                                    .unwrap_or_default();
                                for item in item_arr.iter() {
                                    let label = item
                                        .get("label")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    if !prefix_now.is_empty() && !label.starts_with(&prefix_now) {
                                        continue;
                                    }
                                    let detail = item
                                        .get("detail")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let insert_text = item
                                        .get("insertText")
                                        .and_then(|v| v.as_str())
                                        .or_else(|| {
                                            item.get("textEdit")
                                                .and_then(|te| te.get("newText"))
                                                .and_then(|v| v.as_str())
                                        })
                                        .unwrap_or(&label)
                                        .to_string();
                                    items.push((label, detail, insert_text));
                                    if items.len() >= 20 {
                                        break;
                                    }
                                }
                                if !items.is_empty() && !cmdview_active && !palette_active {
                                    completion.items = items;
                                    completion.selected = 0;
                                    completion.visible = true;
                                } else {
                                    completion.hide();
                                }
                                redraw = true;
                            }

                            // Handle hover response.
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("textDocument/hover")
                            {
                                lsp_state.pending_requests.remove(&id);
                                if pending_lsp_hover_request == Some(id)
                                    && hovered_diagnostic.is_none()
                                {
                                    pending_lsp_hover_request = None;
                                    let contents =
                                        msg.get("result").and_then(|r| r.get("contents"));
                                    let text = contents
                                        .and_then(|c| {
                                            // MarkupContent: {kind, value}
                                            c.get("value")
                                                .and_then(|v| v.as_str())
                                                .map(String::from)
                                                .or_else(|| {
                                                    // Plain string.
                                                    c.as_str().map(String::from)
                                                })
                                        })
                                        .unwrap_or_default();
                                    if !text.is_empty() {
                                        hover.text = text;
                                        hover.visible = true;
                                    } else {
                                        hover.hide();
                                    }
                                    redraw = true;
                                }
                            }

                            // Handle go-to-definition response.
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("textDocument/definition")
                            {
                                lsp_state.pending_requests.remove(&id);
                                let result = msg.get("result");
                                // result can be Location, Location[], or null.
                                let loc = result.and_then(|r| {
                                    if r.is_array() {
                                        r.as_array().and_then(|a| a.first())
                                    } else if r.is_object() {
                                        Some(r)
                                    } else {
                                        None
                                    }
                                });
                                if let Some(location) = loc {
                                    let target_uri =
                                        location.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                                    let target_line = location
                                        .get("range")
                                        .and_then(|r| r.get("start"))
                                        .and_then(|s| s.get("line"))
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0)
                                        as usize
                                        + 1;
                                    let target_col = location
                                        .get("range")
                                        .and_then(|r| r.get("start"))
                                        .and_then(|s| s.get("character"))
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0)
                                        as usize
                                        + 1;
                                    let target_path = uri_to_path(target_uri);
                                    if !target_path.is_empty() {
                                        // Open or switch to file.
                                        let existing =
                                            docs.iter().position(|d| d.path == target_path);
                                        let tab_idx = if let Some(idx) = existing {
                                            idx
                                        } else {
                                            open_file_into(&target_path, &mut docs, use_git());
                                            autoreload.watch(&target_path);
                                            remember_recent_file(
                                                &mut recent_files,
                                                &target_path,
                                                userdir_path,
                                            );
                                            docs.len() - 1
                                        };
                                        active_tab = tab_idx;
                                        // Set cursor to target position.
                                        if let Some(doc) = docs.get(active_tab) {
                                            if let Some(buf_id) = doc.view.buffer_id {
                                                let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                    let line =
                                                        target_line.min(b.lines.len()).max(1);
                                                    let max_col = char_count(
                                                        b.lines[line - 1].trim_end_matches('\n'),
                                                    ) + 1;
                                                    let col = target_col.min(max_col);
                                                    b.selections = vec![line, col, line, col];
                                                    Ok(())
                                                });
                                            }
                                        }
                                    }
                                }
                                redraw = true;
                            }

                            // Handle document-formatting response (manual or on-save).
                            {
                                let fmt_method = lsp_state.pending_requests.get(&id).cloned();
                                if matches!(
                                    fmt_method.as_deref(),
                                    Some(
                                        "textDocument/formatting" | "textDocument/formatting@save"
                                    )
                                ) {
                                    lsp_state.pending_requests.remove(&id);
                                    let save_after = fmt_method.as_deref()
                                        == Some("textDocument/formatting@save");
                                    let req_uri = lsp_state
                                        .pending_request_uris
                                        .remove(&id)
                                        .unwrap_or_default();
                                    let req_change_id =
                                        lsp_state.pending_request_change_ids.remove(&id);
                                    if let Some(edits) =
                                        msg.get("result").and_then(|r| r.as_array())
                                        && let Some(idx) = docs.iter().position(|d| {
                                            !d.path.is_empty() && path_to_uri(&d.path) == req_uri
                                        })
                                        && let Some(buf_id) = docs[idx].view.buffer_id
                                        // The edit positions address the revision the request
                                        // was sent for. Typing while the server works moves
                                        // every offset after the caret, so a response that
                                        // outlived its revision is dropped rather than spliced
                                        // into text it does not describe.
                                        && buffer::with_buffer(buf_id, |b| Ok(b.change_id))
                                            .is_ok_and(|cid| {
                                                req_change_id.is_none_or(|want| want == cid)
                                            })
                                    {
                                        let changed = buffer::with_buffer_mut(buf_id, |b| {
                                            Ok(apply_lsp_text_edits(b, edits))
                                        })
                                        .unwrap_or(false);
                                        if changed {
                                            docs[idx].cached_change_id = -1;
                                            docs[idx].cached_render =
                                                std::sync::Arc::new(Vec::new());
                                            if save_after {
                                                let path = docs[idx].path.clone();
                                                let atomic = config.files.atomic_save;
                                                if let Ok(Ok(cid)) =
                                                    buffer::with_buffer(buf_id, |b| {
                                                        Ok(buffer::save_file(
                                                            b, &path, b.crlf, atomic,
                                                        )
                                                        .map(|()| b.change_id))
                                                    })
                                                {
                                                    docs[idx].saved_change_id = cid;
                                                    docs[idx].saved_signature =
                                                        buffer::with_buffer(buf_id, |b| {
                                                            Ok(buffer::content_signature(&b.lines))
                                                        })
                                                        .unwrap_or(0);
                                                }
                                            }
                                            redraw = true;
                                        }
                                    }
                                }
                            }

                            // Handle rename response (WorkspaceEdit).
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("textDocument/rename")
                            {
                                lsp_state.pending_requests.remove(&id);
                                if let Some(result) = msg.get("result").filter(|r| !r.is_null()) {
                                    let atomic = config.files.atomic_save;
                                    let n = apply_lsp_workspace_edit(
                                        result,
                                        &mut docs,
                                        use_git(),
                                        atomic,
                                    );
                                    info_message = Some((
                                        format!("Renamed across {n} file(s)"),
                                        Instant::now(),
                                    ));
                                    for d in &mut docs {
                                        d.cached_change_id = -1;
                                    }
                                    crate::window::force_invalidate();
                                } else {
                                    info_message = Some((
                                        "Rename produced no changes".to_string(),
                                        Instant::now(),
                                    ));
                                }
                                redraw = true;
                            }

                            // Resolve lazy code actions before applying their edit
                            // or command. Rust Analyzer and OmniSharp both use this
                            // path for actions whose initial response only has data.
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("codeAction/resolve")
                            {
                                lsp_state.pending_requests.remove(&id);
                                if let Some(action) =
                                    msg.get("result").filter(|result| result.is_object())
                                {
                                    execute_lsp_code_action(
                                        action,
                                        &mut docs,
                                        &mut lsp_state,
                                        use_git(),
                                        config.files.atomic_save,
                                    );
                                } else {
                                    info_message = Some((
                                        "Language server could not resolve the code action"
                                            .to_string(),
                                        Instant::now(),
                                    ));
                                }
                                redraw = true;
                            }

                            // Handle code-action response: collect into the picker.
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("textDocument/codeAction/hover")
                            {
                                lsp_state.pending_requests.remove(&id);
                                if diagnostic_hover_action_request == Some(id) {
                                    diagnostic_hover_action_request = None;
                                    diagnostic_hover_actions =
                                        collect_lsp_code_actions(msg.get("result"));
                                    diagnostic_quick_fix_rect = None;
                                    redraw = true;
                                }
                            }

                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("textDocument/codeAction")
                            {
                                lsp_state.pending_requests.remove(&id);
                                code_actions = collect_lsp_code_actions(msg.get("result"));
                                if code_actions.is_empty() {
                                    info_message = Some((
                                        "No code actions available".to_string(),
                                        Instant::now(),
                                    ));
                                    code_action_active = false;
                                } else {
                                    code_action_selected = 0;
                                    code_action_active = true;
                                }
                                redraw = true;
                            }

                            // Handle signature-help response.
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("textDocument/signatureHelp")
                            {
                                lsp_state.pending_requests.remove(&id);
                                let result = msg.get("result");
                                let sig = result
                                    .and_then(|r| r.get("signatures"))
                                    .and_then(|s| s.as_array())
                                    .and_then(|arr| {
                                        let active = result
                                            .and_then(|r| r.get("activeSignature"))
                                            .and_then(|v| v.as_i64())
                                            .unwrap_or(0)
                                            as usize;
                                        arr.get(active).or_else(|| arr.first())
                                    })
                                    .and_then(|s| s.get("label"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if sig.is_empty() {
                                    signature_help.hide();
                                } else {
                                    signature_help.text = sig;
                                    signature_help.visible = true;
                                }
                                redraw = true;
                            }

                            // Handle implementation/typeDefinition/references responses.
                            // These return the same Location/Location[] format as definition.
                            let method_str = lsp_state.pending_requests.get(&id).cloned();
                            if matches!(
                                method_str.as_deref(),
                                Some(
                                    "textDocument/implementation"
                                        | "textDocument/typeDefinition"
                                        | "textDocument/references"
                                )
                            ) {
                                lsp_state.pending_requests.remove(&id);
                                let result = msg.get("result");
                                let loc = result.and_then(|r| {
                                    if r.is_array() {
                                        r.as_array().and_then(|a| a.first())
                                    } else if r.is_object() {
                                        Some(r)
                                    } else {
                                        None
                                    }
                                });
                                if let Some(location) = loc {
                                    let target_uri =
                                        location.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                                    let target_line = location
                                        .get("range")
                                        .and_then(|r| r.get("start"))
                                        .and_then(|s| s.get("line"))
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0)
                                        as usize
                                        + 1;
                                    let target_col = location
                                        .get("range")
                                        .and_then(|r| r.get("start"))
                                        .and_then(|s| s.get("character"))
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0)
                                        as usize
                                        + 1;
                                    let target_path = uri_to_path(target_uri);
                                    if !target_path.is_empty() {
                                        let existing =
                                            docs.iter().position(|d| d.path == target_path);
                                        let tab_idx = if let Some(idx) = existing {
                                            idx
                                        } else {
                                            open_file_into(&target_path, &mut docs, use_git());
                                            autoreload.watch(&target_path);
                                            remember_recent_file(
                                                &mut recent_files,
                                                &target_path,
                                                userdir_path,
                                            );
                                            docs.len() - 1
                                        };
                                        active_tab = tab_idx;
                                        if let Some(doc) = docs.get(active_tab) {
                                            if let Some(buf_id) = doc.view.buffer_id {
                                                let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                    let line =
                                                        target_line.min(b.lines.len()).max(1);
                                                    let max_col = char_count(
                                                        b.lines[line - 1].trim_end_matches('\n'),
                                                    ) + 1;
                                                    let col = target_col.min(max_col);
                                                    b.selections = vec![line, col, line, col];
                                                    Ok(())
                                                });
                                            }
                                        }
                                    }
                                }
                                redraw = true;
                            }
                        }
                        // Handle publishDiagnostics.
                        if msg.get("method").and_then(|v| v.as_str())
                            == Some("textDocument/publishDiagnostics")
                        {
                            if let Some(params) = msg.get("params") {
                                if let Some(uri) = params.get("uri").and_then(|v| v.as_str()) {
                                    let path = uri_to_path(uri);
                                    let diags: Vec<Diagnostic> = params
                                        .get("diagnostics")
                                        .and_then(|v| v.as_array())
                                        .map(|arr| arr.iter().map(Diagnostic::from_lsp).collect())
                                        .unwrap_or_default();
                                    lsp_state.update_push_diagnostics(path, diags);
                                    redraw = true;
                                }
                            }
                        }
                        if msg.get("method").and_then(|value| value.as_str())
                            == Some("workspace/projectInitializationComplete")
                            && let Some(doc) = docs.get(active_tab)
                            && !doc.path.is_empty()
                        {
                            request_lsp_diagnostics(tid, &mut lsp_state, &path_to_uri(&doc.path));
                        }
                    }
                    if !poll.running {
                        if !lsp_state.initialized && !lsp_state.launch_command.is_empty() {
                            lsp_state
                                .rejected_launch_commands
                                .insert(lsp_state.launch_command.clone());
                        }
                        lsp::remove_transport(tid);
                        lsp_state.note_spawn_failure();
                        lsp_state.transport_id = None;
                        lsp_state.initialized = false;
                        lsp_state.forget_server_state();
                    }
                }
            }
        }

        // LSP: detect any buffer mutation on the active doc by watching
        // `change_id`. The typing path used to flip `last_change` itself,
        // but every other edit route (paste, undo, redo, format-document,
        // multi-cursor delete, snippet apply, find-and-replace) bypassed
        // that flag, so inlay hints went stale until the next keystroke.
        // Polling the change counter per frame catches all of them in one
        // place.
        if subsystems.has_lsp() && lsp_state.transport_id.is_some() && lsp_state.initialized {
            if let Some(doc) = docs.get(active_tab) {
                let lsp_allowed = !(crate::editor::open_doc::doc_policy(doc).disable_lsp);
                if !doc.path.is_empty() && lsp_allowed {
                    let ext = doc.path.rsplit('.').next().unwrap_or("");
                    let is_lsp_file = ext_to_lsp_filetype(ext)
                        .map(|ft| ft == lsp_state.filetype)
                        .unwrap_or(false);
                    if is_lsp_file {
                        if let Some(buf_id) = doc.view.buffer_id {
                            let cur = buffer::with_buffer(buf_id, |b| Ok(b.change_id)).unwrap_or(0);
                            let uri = path_to_uri(&doc.path);
                            let prev = lsp_state.last_seen_change_id.get(&uri).copied();
                            match prev {
                                None => {
                                    lsp_state.last_seen_change_id.insert(uri, cur);
                                }
                                Some(p) if p != cur => {
                                    lsp_state.last_seen_change_id.insert(uri.clone(), cur);
                                    lsp_state.last_change = Some(Instant::now());
                                    lsp_state.pending_change_uri = Some(uri);
                                    lsp_state.pending_change_version += 1;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        // LSP: announce a bounded number of queued documents per frame, so a
        // session's worth of restored tabs never lands in one frame.
        if subsystems.has_lsp() && lsp_state.initialized {
            if let Some(tid) = lsp_state.transport_id {
                /// Documents announced per frame. Each one joins and
                /// serializes a whole buffer.
                const DID_OPEN_PER_FRAME: usize = 2;
                for _ in 0..DID_OPEN_PER_FRAME {
                    let Some(path) = lsp_state.pending_did_open.pop_front() else {
                        break;
                    };
                    announce_document_to_lsp(tid, &mut lsp_state, &docs, &path);
                }
            }
        }

        // LSP: flush debounced didChange after 300ms of no changes.
        if subsystems.has_lsp() {
            if let Some(last) = lsp_state.last_change {
                if last.elapsed().as_millis() >= 300 {
                    if let (Some(tid), Some(uri)) =
                        (lsp_state.transport_id, lsp_state.pending_change_uri.take())
                    {
                        if lsp_state.initialized {
                            // Read current buffer text for the file.
                            let file_path = uri_to_path(&uri);
                            if let Some(doc) = docs.iter().find(|d| d.path == file_path) {
                                let ext = doc.path.rsplit('.').next().unwrap_or("");
                                let is_lsp_file = !(crate::editor::open_doc::doc_policy(doc)
                                    .disable_lsp)
                                    && ext_to_lsp_filetype(ext)
                                        .map(|ft| ft == lsp_state.filetype)
                                        .unwrap_or(false);
                                if is_lsp_file {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let text =
                                            buffer::with_buffer(buf_id, |b| Ok(b.lines.join("")))
                                                .unwrap_or_default();
                                        let previous_text = lsp_state
                                            .document_texts
                                            .get(&uri)
                                            .cloned()
                                            .unwrap_or_default();
                                        let change = if lsp_state.incremental_sync {
                                            lsp_incremental_did_change(
                                                &uri,
                                                lsp_state.pending_change_version,
                                                &previous_text,
                                                &text,
                                            )
                                        } else {
                                            lsp_did_change(
                                                &uri,
                                                lsp_state.pending_change_version,
                                                &text,
                                            )
                                        };
                                        let _ = lsp::send_message(tid, &change);
                                        lsp_state.document_texts.insert(uri.clone(), text);
                                        request_lsp_diagnostics(tid, &mut lsp_state, &uri);
                                        // Re-request inlay hints after change is flushed.
                                        let line_count =
                                            buffer::with_buffer(buf_id, |b| Ok(b.lines.len()))
                                                .unwrap_or(100);
                                        let req_id = lsp_state.next_id();
                                        lsp_state
                                            .pending_requests
                                            .insert(req_id, "textDocument/inlayHint".to_string());
                                        lsp_state.pending_request_uris.insert(req_id, uri.clone());
                                        let _ = lsp::send_message(
                                            tid,
                                            &lsp_inlay_hint_request(req_id, &uri, 0, line_count),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    lsp_state.last_change = None;
                }
            }
        }

        if diagnostic_hover_leave_at.is_some_and(|deadline| Instant::now() >= deadline) {
            hover.hide();
            hovered_diagnostic = None;
            diagnostic_hover_region_rect = None;
            diagnostic_hover_leave_at = None;
            diagnostic_quick_fix_rect = None;
            diagnostic_hover_action_request = None;
            diagnostic_hover_actions.clear();
            redraw = true;
        }

        // LSP: fire a deferred `textDocument/hover` after the mouse
        // has been still for ~600ms over a code position with no
        // diagnostic under it. Keeps the LSP unspammed while the
        // cursor moves; surfaces type / doc info as a tooltip once
        // the user pauses.
        if subsystems.has_lsp()
            && !hover.visible
            && let Some(idle_since) = mouse_idle_since
            && let Some((line, col)) = mouse_doc_pos
            && idle_since.elapsed() >= std::time::Duration::from_millis(600)
            && last_lsp_hover_pos != Some((line, col))
        {
            mouse_idle_since = None;
            last_lsp_hover_pos = Some((line, col));
            if let Some(doc) = docs.get(active_tab)
                && let Some(tid) = lsp_state.transport_id
                && lsp_state.initialized
                && !doc.path.is_empty()
                && doc.view.buffer_id.is_some()
            {
                let uri = path_to_uri(&doc.path);
                let req_id = lsp_state.next_id();
                lsp_state
                    .pending_requests
                    .insert(req_id, "textDocument/hover".to_string());
                let _ = lsp::send_message(tid, &lsp_hover_request(req_id, &uri, line - 1, col - 1));
                pending_lsp_hover_request = Some(req_id);
                hover.line = line;
                hover.col = col;
            }
        }

        // Terminal: poll/drain/reap every pty each frame regardless of panel
        // visibility, so a shell that exits or floods output while the panel
        // is hidden is still reaped and its pty kept drained. Only repaints
        // are gated on visibility.
        if subsystems.has_terminal() {
            let mut dead_indices = Vec::new();
            for (i, inst) in terminal.terminals.iter_mut().enumerate() {
                inst.inner.poll();
                if !inst.inner.running {
                    dead_indices.push(i);
                } else {
                    // Drain the pty up to a per-frame byte budget so a burst
                    // (a build, `cat bigfile`) shows up in one frame instead of
                    // trickling at 4 KiB/frame, while still yielding to the UI
                    // within the budget. The pty back-pressures the child once
                    // its kernel buffer fills, so nothing is lost.
                    let mut remaining: usize = 256 * 1024;
                    while remaining > 0 {
                        match inst.inner.read(remaining.min(64 * 1024)) {
                            Some(data) if !data.is_empty() => {
                                remaining = remaining.saturating_sub(data.len());
                                inst.tbuf.process_output(&data);
                                if terminal.visible {
                                    redraw = true;
                                }
                            }
                            _ => break,
                        }
                    }
                }
            }
            // Remove dead terminals in reverse order.
            for i in dead_indices.into_iter().rev() {
                terminal.terminals[i].inner.cleanup();
                terminal.terminals.remove(i);
                if terminal.visible {
                    redraw = true;
                }
            }
            if terminal.terminals.is_empty() {
                let was_visible = terminal.visible;
                terminal.visible = false;
                terminal.focused = false;
                terminal.active = 0;
                if was_visible {
                    // Panel just went away -- force a native repaint so the
                    // editor content reclaims the vacated strip in the
                    // same frame instead of waiting for the next event.
                    crate::window::force_invalidate();
                }
            } else if terminal.active >= terminal.terminals.len() {
                terminal.active = terminal.terminals.len() - 1;
            }
        }

        // Git: surface results of async mutations (push/pull/commit/stash) and
        // apply async diff results to their docs. These run on worker threads,
        // so the render loop never blocks on git network or fork/exec I/O.
        for m in crate::editor::git::drain_finished_mutations() {
            let detail = m.result.stderr.trim();
            let msg = if m.result.ok {
                format!("{}: done", m.label)
            } else if !detail.is_empty() {
                format!("{}: {detail}", m.label)
            } else {
                format!("{} failed", m.label)
            };
            info_message = Some((msg, Instant::now()));
            // A mutation can change the working-tree baseline, so refresh the
            // diff gutters of the open docs against the new git state.
            for doc in &docs {
                if !doc.path.is_empty() {
                    crate::editor::git::start_diff(&doc.path);
                }
            }
            redraw = true;
        }
        for d in crate::editor::git::drain_diffs() {
            if let Some(doc) = docs.iter_mut().find(|doc| doc.path == d.path) {
                doc.git_changes = d.changes;
                redraw = true;
            }
        }

        // Project search/replace: pick up async grep results for the active
        // panel. run_project_search is non-blocking and refreshes in the
        // background; apply the latest results when they differ.
        if subsystems.has_find_in_files() {
            if project_search_active {
                let r = run_project_search(
                    &project_search_query,
                    &project_root,
                    project_use_regex,
                    project_whole_word,
                    project_case_insensitive,
                );
                if r != project_search_results {
                    project_search_results = r;
                    if project_search_selected >= project_search_results.len() {
                        project_search_selected = project_search_results.len().saturating_sub(1);
                    }
                    redraw = true;
                }
            }
            if project_replace_active {
                let r = run_project_search(
                    &project_replace_search,
                    &project_root,
                    project_use_regex,
                    project_whole_word,
                    project_case_insensitive,
                );
                if r != project_replace_results {
                    project_replace_results = r;
                    if project_replace_selected >= project_replace_results.len() {
                        project_replace_selected = project_replace_results.len().saturating_sub(1);
                    }
                    redraw = true;
                }
            }
        }

        // Project replace-all: apply the result of the background sed job.
        if let Some(job) = replace_job.take() {
            if job.is_finished() {
                let count = job.join().unwrap_or(0);
                info_message = Some((
                    format!("Replaced {count} occurrences across project"),
                    Instant::now(),
                ));
                // Reload any open files the replace may have changed.
                for doc in &mut docs {
                    if let Some(buf_id) = doc.view.buffer_id {
                        if !doc.path.is_empty() {
                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                let mut fresh = buffer::default_buffer_state();
                                if buffer::load_file(&mut fresh, &doc.path).is_ok() {
                                    b.lines = fresh.lines;
                                    b.change_id += 1;
                                }
                                Ok(())
                            });
                        }
                    }
                }
                redraw = true;
            } else {
                replace_job = Some(job);
            }
        }

        // Git status panel: apply the result of the background refresh.
        if let Some(job) = git_status_job.take() {
            if job.is_finished() {
                git_status_entries = job.join().unwrap_or_default();
                if git_status_selected >= git_status_entries.len() {
                    git_status_selected = git_status_entries.len().saturating_sub(1);
                }
                redraw = true;
            } else {
                git_status_job = Some(job);
            }
        }

        // Git blame overlay: apply the background result if still wanted.
        if let Some(job) = git_blame_job.take() {
            if job.is_finished() {
                let lines = job.join().unwrap_or_default();
                if git_blame_active {
                    git_blame_lines = lines;
                }
                redraw = true;
            } else {
                git_blame_job = Some(job);
            }
        }

        // Git log panel: apply the background result.
        if let Some(job) = git_log_job.take() {
            if job.is_finished() {
                git_log_entries = job.join().unwrap_or_default();
                if git_log_selected >= git_log_entries.len() {
                    git_log_selected = git_log_entries.len().saturating_sub(1);
                }
                redraw = true;
            } else {
                git_log_job = Some(job);
            }
        }

        // Update check: surface the version comparison when curl returns.
        if let Some(job) = update_check_job.take() {
            if job.is_finished() {
                if let Ok(msg) = job.join() {
                    info_message = Some((msg, Instant::now()));
                }
                redraw = true;
            } else {
                update_check_job = Some(job);
            }
        }

        {
            // Layout + render.
            let (w, h, _, _) = crate::window::get_window_size();
            let width = w as f64;
            let height = h as f64;
            let status_h = style.font_height + style.padding_y * 2.0;
            let sidebar_w = if subsystems.has_sidebar() && sidebar_visible {
                sidebar_width
            } else {
                0.0
            };

            let tab_h = if !single_file_mode && !docs.is_empty() {
                style.font_height + style.padding_y * 3.0
            } else {
                0.0
            };
            let terminal_h = if subsystems.has_terminal() && terminal.visible {
                (height * 0.3)
                    .min(height - tab_h - status_h - 50.0)
                    .max(80.0)
            } else {
                0.0
            };
            let minimap_w = if minimap_visible { 120.0 } else { 0.0 };
            let breadcrumb_h = if docs.get(active_tab).is_some() {
                style.font_height + style.padding_y * 0.5
            } else {
                0.0
            };
            let content_rect = crate::editor::types::Rect {
                x: sidebar_w,
                y: tab_h + breadcrumb_h,
                w: width - sidebar_w - minimap_w,
                h: height - tab_h - breadcrumb_h - terminal_h - status_h,
            };
            empty_view.set_rect(content_rect);
            // Note-Anvil keeps the markdown preview pinned on for every
            // doc — it's not toggleable in notes mode.
            if subsystems.has_notes_mode() {
                for d in docs.iter_mut() {
                    d.preview.enabled = true;
                }
            }
            if let Some(doc) = docs.get_mut(active_tab) {
                if doc.preview.enabled {
                    // Split the content area into editor | preview panes at the
                    // user-adjustable `preview_split` fraction (drag the divider
                    // to resize; persisted per app). The editor keeps float
                    // rects (its existing wrap/click math has always tolerated
                    // them); the preview rect is snapped to integer pixels so
                    // the background fill and clip rect enclose every logical
                    // pixel. Without snapping, `draw_rect`'s i32 cast truncates
                    // the bottom of the fill, leaving a stale pixel row that
                    // reads as a thin blue line from a previously drawn heading
                    // rule.
                    let half_w = (content_rect.w * preview_split).floor();
                    let left = crate::editor::types::Rect {
                        x: content_rect.x,
                        y: content_rect.y,
                        w: half_w,
                        h: content_rect.h,
                    };
                    let right_x = (content_rect.x + half_w).round();
                    let right_y = content_rect.y.floor();
                    let right_bottom = (content_rect.y + content_rect.h).ceil();
                    let right_right = (content_rect.x + content_rect.w).ceil();
                    let right = crate::editor::types::Rect {
                        x: right_x,
                        y: right_y,
                        w: right_right - right_x,
                        h: right_bottom - right_y,
                    };
                    doc.view.set_rect(left);
                    doc.preview.rect = right;
                } else {
                    doc.view.set_rect(content_rect);
                    doc.preview.rect = crate::editor::types::Rect::default();
                    doc.preview.content_rect = crate::editor::types::Rect::default();
                    doc.preview.focused = false;
                }
            }
            status_view.set_rect(crate::editor::types::Rect {
                x: 0.0,
                y: height - status_h,
                w: width,
                h: status_h,
            });

            let uctx = UpdateContext {
                dt: 1.0 / fps,
                window_width: width,
                window_height: height,
            };
            empty_view.update(&uctx);
            if let Some(doc) = docs.get_mut(active_tab) {
                let dv = &mut doc.view;
                if let Some(buf_id) = dv.buffer_id {
                    use crate::editor::view::DrawContext as _;
                    let line_count =
                        buffer::with_buffer(buf_id, |b| Ok(b.lines.len())).unwrap_or(1);
                    let digits = format!("{}", line_count).len().max(2);
                    let char_w = draw_ctx.font_width(style.code_font, "9");
                    dv.gutter_width = char_w * digits as f64 + style.padding_x * 2.0;
                    dv.code_char_w = char_w;
                }
                dv.update(&uctx);
            }
            status_view.update(&uctx);

            // Autoreload: check for external file changes. A watcher event is
            // only a hint; reading the file to answer it happens on a worker,
            // and the result is applied below against the document as it
            // stands when the read lands.
            for changed in autoreload.poll_changed() {
                let canonical = crate::editor::open_doc::canonicalize_path(&changed);
                let Some(doc) = docs
                    .iter()
                    .find(|doc| crate::editor::open_doc::doc_canonical_path(doc) == canonical)
                else {
                    continue;
                };
                if crate::editor::open_doc::doc_policy(doc).large {
                    // The reload prompt leaves the choice with the user and
                    // does the work only when it is explicitly accepted.
                    nag = Nag::ReloadFromDisk {
                        path: doc.path.clone(),
                    };
                    redraw = true;
                    continue;
                }
                crate::editor::reload::probe(&doc.path);
            }

            for disk in crate::editor::reload::drain() {
                let canonical = crate::editor::open_doc::canonicalize_path(&disk.path);
                let Some(doc) = docs
                    .iter_mut()
                    .find(|doc| crate::editor::open_doc::doc_canonical_path(doc) == canonical)
                else {
                    continue;
                };
                let Some(buf_id) = doc.view.buffer_id else {
                    continue;
                };
                use crate::editor::open_doc::ExternalChange;
                match crate::editor::open_doc::classify_external_change(doc, disk.signature) {
                    ExternalChange::Echo => continue,
                    ExternalChange::AskUser => {
                        // Local edits would be lost by an automatic reload.
                        nag = Nag::ReloadFromDisk {
                            path: doc.path.clone(),
                        };
                    }
                    ExternalChange::Adopt => {
                        let _perf = crate::editor::perf::span("external_reload_apply");
                        // No local edits: adopt the external content, which the
                        // worker has already read.
                        let _ = buffer::with_buffer_mut(buf_id, |b| {
                            b.lines = disk.lines;
                            // `default_buffer_state()` resets change_id to 1; a
                            // just-opened buffer also sits at 1, so the doc-view
                            // render cache would hit on stale lines. Bump past the
                            // current value to invalidate every downstream cache.
                            b.change_id = b.change_id.wrapping_add(1).max(1);
                            b.total_bytes = b.lines.iter().map(|l| l.len() as u64).sum();
                            buffer::reset_history(b);
                            Ok(())
                        });
                        // Force the render cache to rebuild next frame rather than
                        // relying on the change_id comparison to catch the bump.
                        doc.cached_change_id = -1;
                        doc.cached_render = std::sync::Arc::new(Vec::new());
                        crate::window::force_invalidate();
                        // Realign the "saved" markers with what is now on disk so
                        // the next external change is judged against the correct
                        // baseline.
                        if let Ok(cid) = buffer::with_buffer(buf_id, |b| Ok(b.change_id)) {
                            doc.saved_change_id = cid;
                            doc.saved_signature = disk.signature;
                        }
                    }
                }
                redraw = true;
            }

            // Sidebar watcher: refresh when files are created/deleted/renamed.
            if subsystems.has_sidebar()
                && !project_root.is_empty()
                && sidebar_watcher.poll_changed()
            {
                let expanded: HashSet<String> = sidebar_entries
                    .iter()
                    .filter(|e| e.is_dir && e.expanded)
                    .map(|e| e.path.clone())
                    .collect();
                sidebar_entries = scan_for_sidebar(
                    subsystems.has_notes_mode(),
                    &project_root,
                    sidebar_show_hidden,
                );
                expand_sidebar_from_set(&mut sidebar_entries, &expanded, sidebar_show_hidden);
                sidebar_watcher.unwatch_all();
                sidebar_watcher.watch_dir(&project_root);
                for entry in &sidebar_entries {
                    if entry.is_dir && entry.expanded {
                        sidebar_watcher.watch_dir(&entry.path);
                    }
                }
                redraw = true;
            }

            // Notes-mode autosave: any dirty doc that has been idle (no
            // edit) for at least the debounce window gets persisted.
            // Keeps writes off the per-keystroke path while still
            // flushing within ~250 ms of typing pause.
            if subsystems.has_notes_mode() {
                let idle_threshold_secs = 0.25;
                let now = buffer::now_secs();
                for doc in docs.iter_mut() {
                    if doc.path.is_empty() {
                        continue;
                    }
                    let Some(buf_id) = doc.view.buffer_id else {
                        continue;
                    };
                    let needs_save = buffer::with_buffer(buf_id, |b| {
                        let dirty = b.change_id != doc.saved_change_id;
                        let idle = b
                            .last_edit
                            .map(|le| now - le.0 >= idle_threshold_secs)
                            .unwrap_or(true);
                        Ok(dirty && idle)
                    })
                    .unwrap_or(false);
                    if !needs_save {
                        continue;
                    }
                    let path = doc.path.clone();
                    let saved = buffer::with_buffer_mut(buf_id, |b| {
                        let crlf = b.crlf;
                        buffer::save_file(b, &path, crlf, false)
                            .map_err(|_| buffer::BufferError::UnknownBuffer)?;
                        Ok((b.change_id, buffer::content_signature(&b.lines)))
                    });
                    if let Ok((cid, sig)) = saved {
                        doc.saved_change_id = cid;
                        doc.saved_signature = sig;
                    }
                }
            }

            // Apply deferred render cache unconditionally so it never goes
            // stale. This MUST be outside the `if redraw` block -- otherwise
            // the cache sits unconsumed until the next event and forces an
            // infinite render loop if we try to force redraw when pending.
            if let Some((tab_idx, rendered_buf_id, lines, cid, sy, hint_count, rw, rh)) =
                pending_render_cache.take()
            {
                if let Some(doc_mut) = docs.get_mut(tab_idx) {
                    // Only apply the cache if the doc at this tab still wraps the
                    // same buffer that produced the render. A project switch
                    // (Open Recent) swaps the entire docs list, so tab_idx can
                    // alias a completely different file — in that case, a stale
                    // render would overwrite the fresh doc's empty cache and
                    // cause the previous project's text to flash on-screen.
                    if doc_mut.view.buffer_id == Some(rendered_buf_id) {
                        doc_mut.cached_render = lines;
                        doc_mut.cached_change_id = cid;
                        doc_mut.cached_scroll_y = sy;
                        doc_mut.cached_hint_count = hint_count;
                        doc_mut.cached_rect_w = rw;
                        doc_mut.cached_rect_h = rh;
                    }
                }
            }

            // A cold/deep syntax walk can yield after its per-frame budget.
            // The render pass sets this when another frame is needed to
            // continue from the saved lexical frontier.
            let mut syntax_pending_after_frame = false;
            if redraw && window_hidden {
                // Consume the redraw flag but skip the actual render pass.
                // The compositor would throw away our frames anyway while
                // the window is occluded/minimised, and we've dropped the
                // glyph cache / render-cache buffers in the event handler.
                redraw = false;
            }
            if redraw {
                // Update window title and status bar from active tab.
                let app_name = if subsystems.has_notes_mode() {
                    "Note Anvil"
                } else if subsystems.has_sidebar() {
                    "Lite Anvil"
                } else {
                    "Nano Anvil"
                };
                let active_doc_for_title = docs.get(active_tab);
                let title = active_doc_for_title
                    .map(|d| d.name.as_str())
                    .unwrap_or(app_name);
                let title_dirty = active_doc_for_title.is_some_and(doc_is_modified);
                let title_key = if title_dirty {
                    format!("*{title}")
                } else {
                    title.to_string()
                };
                if window_title != title_key {
                    let display =
                        crate::editor::doc_view::format_window_title(title, app_name, title_dirty);
                    crate::window::set_window_title(&display);
                    window_title = title_key;
                }
                status_view.left_items.clear();
                status_view.right_items.clear();
                if let Some(doc) = docs.get(active_tab) {
                    // Left: filename (with modified indicator). Cap at a
                    // third of the window so a runaway filename can't
                    // collide with the cursor-position segment or the
                    // right-side status items.
                    let modified_label = if doc_is_modified(doc) {
                        format!("*{}", doc.name)
                    } else {
                        doc.name.clone()
                    };
                    let filename_max_w = (width / 3.0).max(80.0);
                    let filename_display = {
                        use crate::editor::view::DrawContext as _;
                        if draw_ctx.font_width(style.font, &modified_label) <= filename_max_w {
                            modified_label
                        } else {
                            truncate_left_to_width(
                                &modified_label,
                                filename_max_w,
                                style.font,
                                &mut draw_ctx,
                            )
                        }
                    };
                    status_view.left_items.push(StatusItem {
                        text: filename_display,
                        color: None,
                        command: None,
                    });
                    // Left: cursor position + document %.
                    if let Some(buf_id) = doc.view.buffer_id {
                        let (line, col, total) = buffer::with_buffer(buf_id, |b| {
                            Ok((
                                *b.selections.get(2).unwrap_or(&1),
                                *b.selections.get(3).unwrap_or(&1),
                                b.lines.len(),
                            ))
                        })
                        .unwrap_or((1, 1, 1));
                        let pct = (line * 100).checked_div(total).unwrap_or(100);
                        status_view.left_items.push(StatusItem {
                            text: format!("  Ln {line}/{total}, Col {col}  ({pct}%)"),
                            color: Some(style.dim.to_array()),
                            command: None,
                        });
                    }
                    // Right side with separators: Lang | UTF-8 | Spaces: N | LF | INS
                    let ext = doc.path.rsplit('.').next().unwrap_or("");
                    let filename_for_lang =
                        doc.path.rsplit('/').next().unwrap_or(doc.path.as_str());
                    if status_lang_cache.0 != doc.path {
                        let name = crate::editor::syntax::match_syntax_entry(
                            filename_for_lang,
                            &syntax_index,
                        )
                        .map(|e| e.name.clone())
                        .unwrap_or_else(|| {
                            if ext.is_empty() {
                                "Plain Text".to_string()
                            } else {
                                ext.to_string()
                            }
                        });
                        status_lang_cache = (doc.path.clone(), name);
                    }
                    let lang: &str = &status_lang_cache.1;
                    let indent_label = if doc.indent_type == "hard" {
                        "Tabs".to_string()
                    } else {
                        format!("Spaces: {}", doc.indent_size)
                    };
                    let (crlf, huge) = doc
                        .view
                        .buffer_id
                        .and_then(|id| {
                            buffer::with_buffer(id, |b| Ok((b.crlf, b.undo_snapshots_disabled())))
                                .ok()
                        })
                        .unwrap_or((false, false));
                    let le = if crlf { "CRLF" } else { "LF" };
                    let mode = if overwrite_mode { "OVR" } else { "INS" };
                    let sep = " | ";
                    let mut right_parts = vec![
                        lang.to_string(),
                        "UTF-8".to_string(),
                        indent_label,
                        le.to_string(),
                    ];
                    if huge {
                        right_parts.push("Limited Undo".to_string());
                    }
                    if doc.token_cache.borrow().syntax_limited {
                        right_parts.push("Syntax limited".to_string());
                    }
                    if doc_is_modified(doc) {
                        right_parts.push("modified".to_string());
                    }
                    right_parts.push(mode.to_string());
                    let right_text = right_parts.join(sep);
                    status_view.right_items.push(StatusItem {
                        text: right_text,
                        color: Some(style.dim.to_array()),
                        command: None,
                    });
                } else {
                    status_view.left_items.push(StatusItem {
                        text: if subsystems.has_notes_mode() {
                            "Note Anvil"
                        } else if subsystems.has_sidebar() {
                            "Lite Anvil"
                        } else {
                            "Nano Anvil"
                        }
                        .to_string(),
                        color: None,
                        command: None,
                    });
                    status_view.right_items.push(StatusItem {
                        text: format!("v{}", env!("CARGO_PKG_VERSION")),
                        color: None,
                        command: None,
                    });
                }

                // Append LSP diagnostic count to status bar.
                if let Some(doc) = docs.get(active_tab) {
                    if let Some(diags) = lsp_state.diagnostics.get(&doc.path) {
                        if !diags.is_empty() {
                            let errors = diags.iter().filter(|d| d.severity == 1).count();
                            let warnings = diags.iter().filter(|d| d.severity == 2).count();
                            let label = if errors > 0 && warnings > 0 {
                                format!("{errors}E {warnings}W")
                            } else if errors > 0 {
                                format!("{errors}E")
                            } else {
                                format!("{warnings}W")
                            };
                            let color = if errors > 0 {
                                Some(style.error.to_array())
                            } else {
                                Some(style.warn.to_array())
                            };
                            status_view.right_items.insert(
                                0,
                                StatusItem {
                                    text: label,
                                    color,
                                    command: None,
                                },
                            );
                        }
                    }
                }

                // Track the target instantly. A previous smooth-scroll lerp
                // here was event-driven, not time-driven: it advanced one
                // step per redraw, so any mouse motion (which redraws for
                // hover/cursor updates) would tick the animation forward,
                // making the view appear to scroll on bare mouse movement.
                // Snapping to the target eliminates that class of bug — all
                // legitimate scroll triggers (cursor, find, scrollbar,
                // wheel) write to `target_scroll_y` and now show up the same
                // frame.
                if let Some(doc) = docs.get_mut(active_tab) {
                    let dv = &mut doc.view;
                    let target = dv.target_scroll_y.round();
                    dv.target_scroll_y = target;
                    if dv.scroll_y != target {
                        dv.scroll_y = target;
                    }
                }

                crate::renderer::native_begin_frame();
                crate::editor::app_state::clip_init(width, height);

                // Tab-bar overlay state captured during the tab draw pass and
                // consumed later (just before native_end_frame) to render the
                // hover tooltip and overflow dropdown list. Drawing those at
                // the end keeps them on top of the sidebar / breadcrumb / doc
                // view — otherwise the breadcrumb would paint over them.
                let mut tab_hover: Option<usize> = None;
                let mut tab_overlay_tbh: f64 = 0.0;
                let mut tab_overlay_overflow: bool = false;
                let mut tab_overlay_rects: std::sync::Arc<Vec<(f64, f64, String, String)>> =
                    std::sync::Arc::new(Vec::new());
                let mut tab_overlay_btn_right: f64 = width;
                let mut tab_overlay_btn_w: f64 = 0.0;

                // Draw tab bar (hidden in single-file mode).
                let _tab_bar_h = if !single_file_mode && !docs.is_empty() {
                    let tbh = style.font_height + style.padding_y * 3.0;
                    let accent_h = 3.0;
                    use crate::editor::view::DrawContext as _;
                    draw_ctx.draw_rect(
                        sidebar_w,
                        0.0,
                        width - sidebar_w,
                        tbh,
                        style.background2.to_array(),
                    );

                    let close_w = draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                    let dropdown_btn_w = (style.font_height + style.padding_x * 2.0).ceil();

                    // Measure the full-width tab bar (no truncation) to decide
                    // whether to enter overflow mode, then lay the tabs out.
                    // Reserving the dropdown button space keeps the decision
                    // stable once overflow is on. Both passes are measurement,
                    // so they are done once per change rather than per frame.
                    let avail_full = (width - sidebar_w).max(0.0);
                    let signature = tab_layout_signature(&docs, width, sidebar_w, close_w, &style);
                    if tab_layout.signature != signature {
                        let mut full_total = 0.0_f64;
                        for doc in docs.iter() {
                            let label = if doc_is_modified(doc) {
                                format!("*{}", doc.name)
                            } else {
                                doc.name.clone()
                            };
                            full_total += draw_ctx.font_width(style.font, &label)
                                + style.padding_x * 2.0
                                + close_w
                                + style.divider_size;
                        }
                        let overflow = full_total > avail_full;
                        let mut rects: Vec<(f64, f64, String, String)> =
                            Vec::with_capacity(docs.len());
                        let mut tx = sidebar_w;
                        for doc in docs.iter() {
                            let full_label = if doc_is_modified(doc) {
                                format!("*{}", doc.name)
                            } else {
                                doc.name.clone()
                            };
                            let display_label = if overflow {
                                let base = truncate_tab_name(&doc.name, 10);
                                if doc_is_modified(doc) {
                                    format!("*{base}")
                                } else {
                                    base
                                }
                            } else {
                                full_label.clone()
                            };
                            let tw = draw_ctx.font_width(style.font, &display_label)
                                + style.padding_x * 2.0
                                + close_w;
                            rects.push((tx, tw, display_label, full_label));
                            tx += tw + style.divider_size;
                        }
                        tab_layout = TabLayout {
                            signature,
                            full_total,
                            rects: std::sync::Arc::new(rects),
                        };
                    }
                    let tabs_overflow = tab_layout.full_total > avail_full;
                    if !tabs_overflow {
                        tab_dropdown_open = false;
                    }
                    let tabs_right_limit = if tabs_overflow {
                        (width - dropdown_btn_w).max(sidebar_w)
                    } else {
                        width
                    };
                    tab_overlay_tbh = tbh;
                    tab_overlay_overflow = tabs_overflow;
                    tab_overlay_btn_right = width;
                    tab_overlay_btn_w = dropdown_btn_w;

                    let tab_rects = std::sync::Arc::clone(&tab_layout.rects);
                    for (i, (tx, tw, display_label, _)) in tab_rects.iter().enumerate() {
                        let (tx, tw) = (*tx, *tw);
                        // Don't draw tabs that fall entirely past the dropdown limit;
                        // they're still reachable via the dropdown menu.
                        if tx >= tabs_right_limit {
                            continue;
                        }
                        let bg = if i == active_tab {
                            style.background.to_array()
                        } else {
                            style.background2.to_array()
                        };
                        let fg = if i == active_tab {
                            style.text.to_array()
                        } else {
                            style.dim.to_array()
                        };
                        // Clip this tab to the area left of the dropdown button.
                        let tab_visible_w = (tabs_right_limit - tx).max(0.0).min(tw);
                        draw_ctx.set_clip_rect(tx, 0.0, tab_visible_w, tbh);
                        draw_ctx.draw_rect(tx, accent_h, tw, tbh - accent_h, bg);
                        if i == active_tab {
                            draw_ctx.draw_rect(tx, 0.0, tw, accent_h, style.accent.to_array());
                        }
                        let text_y_tab = accent_h + (tbh - accent_h - style.font_height) / 2.0;
                        draw_ctx.draw_text(
                            style.font,
                            display_label,
                            tx + style.padding_x,
                            text_y_tab,
                            fg,
                        );
                        // Close button with hover highlight.
                        let close_x = tx + tw - close_w;
                        let close_hovered =
                            mouse_y < tbh && mouse_x >= close_x && mouse_x < close_x + close_w;
                        if close_hovered {
                            draw_ctx.draw_rect(
                                close_x,
                                accent_h,
                                close_w,
                                tbh - accent_h,
                                style.line_highlight.to_array(),
                            );
                        }
                        let close_color = if close_hovered {
                            style.text.to_array()
                        } else {
                            style.dim.to_array()
                        };
                        draw_ctx.draw_text(
                            style.icon_font,
                            "C",
                            close_x + style.padding_x * 0.5,
                            accent_h
                                + (tbh - accent_h - draw_ctx.font_height(style.icon_font)) / 2.0,
                            close_color,
                        );
                        draw_ctx.draw_rect(
                            tx + tw,
                            style.padding_y * 0.5,
                            style.divider_size,
                            tbh - style.padding_y,
                            style.dim.to_array(),
                        );
                        // Restore clip for the rest of the tab bar / dropdown draw.
                        crate::editor::app_state::clip_init(width, height);

                        // Track hover for tooltip: only when not over the close icon,
                        // so the close-button interaction is unambiguous.
                        if mouse_y < tbh
                            && mouse_x >= tx
                            && mouse_x < (tx + tw).min(tabs_right_limit)
                            && !close_hovered
                        {
                            tab_hover = Some(i);
                        }
                    }
                    if mouse_y >= tbh {
                        tab_tooltip_suppressed = false;
                    }

                    // Overflow dropdown button. The arrow is drawn as a filled
                    // triangle built from horizontal one-pixel bars rather than a
                    // font glyph — the icons.ttf bundle doesn't include a
                    // chevron-down, and the regular font's "v" looked like a
                    // letter, not an icon.
                    if tabs_overflow {
                        let btn_x = width - dropdown_btn_w;
                        let btn_hovered = mouse_y < tbh && mouse_x >= btn_x;
                        let btn_bg = if btn_hovered || tab_dropdown_open {
                            style.line_highlight.to_array()
                        } else {
                            style.background2.to_array()
                        };
                        draw_ctx.draw_rect(btn_x, accent_h, dropdown_btn_w, tbh - accent_h, btn_bg);
                        draw_ctx.draw_rect(
                            btn_x,
                            accent_h,
                            style.divider_size,
                            tbh - accent_h,
                            style.divider.to_array(),
                        );
                        let arrow_color = style.text.to_array();
                        let arrow_h = (style.font_height * 0.45).round().max(4.0);
                        let arrow_w_px = arrow_h * 2.0;
                        let arrow_cx = btn_x + dropdown_btn_w / 2.0;
                        let arrow_top = accent_h + (tbh - accent_h - arrow_h) / 2.0;
                        let rows = arrow_h as i32;
                        for i in 0..rows {
                            let progress = i as f64 / rows as f64;
                            let row_w = (arrow_w_px * (1.0 - progress)).max(1.0);
                            let row_x = arrow_cx - row_w / 2.0;
                            let row_y = arrow_top + i as f64;
                            draw_ctx.draw_rect(row_x, row_y, row_w, 1.0, arrow_color);
                        }
                    } else {
                        tab_dropdown_open = false;
                    }

                    draw_ctx.draw_rect(
                        sidebar_w,
                        tbh - style.divider_size,
                        width - sidebar_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    crate::editor::app_state::clip_init(width, height);

                    // Hand off per-tab rects to the deferred overlay pass. That
                    // pass runs after every other panel has drawn, so the tooltip
                    // and overflow dropdown aren't painted over by the breadcrumb
                    // / sidebar / doc view that follow this block.
                    tab_overlay_rects = tab_rects;

                    tbh
                } else {
                    tab_dropdown_open = false;
                    0.0
                };

                // Draw breadcrumb strip above the document area.
                if let Some(doc) = docs.get(active_tab) {
                    crate::editor::doc_view::draw_breadcrumb(
                        &mut draw_ctx,
                        &doc.path,
                        sidebar_w,
                        tab_h,
                        width - sidebar_w - minimap_w,
                        breadcrumb_h,
                        &style,
                    );
                }

                // Draw sidebar.
                if subsystems.has_sidebar() && sidebar_visible {
                    use crate::editor::view::DrawContext as _;
                    draw_ctx.draw_rect(0.0, 0.0, sidebar_w, height, style.background2.to_array());

                    // Mini toolbar at the top of the sidebar (big icon font).
                    // When the toolbar subsystem is off (Note-Anvil), collapse
                    // the reserved height so the directory header sits flush
                    // with the top instead of leaving an empty strip.
                    let ibf = style.icon_big_font;
                    let icon_h = draw_ctx.font_height(ibf);
                    let toolbar_h = if subsystems.has_toolbar() {
                        icon_h + style.padding_y * 2.0
                    } else {
                        0.0
                    };
                    if subsystems.has_toolbar() {
                        draw_ctx.draw_rect(
                            0.0,
                            0.0,
                            sidebar_w,
                            toolbar_h,
                            style.background3.to_array(),
                        );
                        let toolbar_buttons: &[(&str, &str)] = &[
                            ("f", "core:new-doc"),
                            ("D", "core:open-file"),
                            ("S", "doc:save"),
                            ("L", "find-replace:find"),
                            ("B", "core:find-command"),
                            ("P", "core:open-user-settings"),
                        ];
                        let mut bx = style.padding_x;
                        let btn_y = (toolbar_h - icon_h) / 2.0;
                        let icon_spacing = style.padding_x;
                        for (icon, _cmd) in toolbar_buttons {
                            let iw = draw_ctx.font_width(ibf, icon);
                            if bx + iw + icon_spacing > sidebar_w {
                                break;
                            }
                            draw_ctx.draw_text(ibf, icon, bx, btn_y, style.dim.to_array());
                            bx += iw + icon_spacing;
                        }
                        draw_ctx.draw_rect(
                            0.0,
                            toolbar_h - style.divider_size,
                            sidebar_w,
                            style.divider_size,
                            style.divider.to_array(),
                        );
                    }

                    // Project directory name header.
                    let dir_header_h = style.font_height + style.padding_y;
                    let resolved_root = std::fs::canonicalize(&project_root)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| project_root.clone());
                    let dir_name = resolved_root
                        .rsplit('/')
                        .find(|s| !s.is_empty())
                        .unwrap_or(&resolved_root);
                    // Ellipsize if the folder name overflows the sidebar width.
                    let header_avail =
                        (sidebar_w - style.padding_x * 2.0 - style.divider_size).max(0.0);
                    let dir_label: String = if draw_ctx.font_width(style.font, dir_name)
                        <= header_avail
                    {
                        dir_name.to_string()
                    } else {
                        let ell = "...";
                        let ell_w = draw_ctx.font_width(style.font, ell);
                        let chars: Vec<char> = dir_name.chars().collect();
                        let mut fit = String::new();
                        for take in (0..chars.len()).rev() {
                            let candidate: String = chars[..take].iter().collect();
                            if draw_ctx.font_width(style.font, &candidate) + ell_w <= header_avail {
                                fit = format!("{candidate}{ell}");
                                break;
                            }
                        }
                        if fit.is_empty() { ell.to_string() } else { fit }
                    };
                    draw_ctx.draw_rect(
                        0.0,
                        toolbar_h,
                        sidebar_w,
                        dir_header_h,
                        style.background2.to_array(),
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &dir_label,
                        style.padding_x,
                        toolbar_h + (dir_header_h - style.font_height) / 2.0,
                        style.accent.to_array(),
                    );
                    draw_ctx.draw_rect(
                        0.0,
                        toolbar_h + dir_header_h - style.divider_size,
                        sidebar_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );

                    // Notes-mode: sort toggle + search box between the
                    // directory header and the file list.
                    let notes_row_h = if subsystems.has_notes_mode() {
                        let row_h = style.font_height + style.padding_y * 2.0;
                        let search_h = style.font_height + style.padding_y * 2.0;
                        let bar_y = toolbar_h + dir_header_h;
                        // Sort-toggle row background.
                        draw_ctx.draw_rect(
                            0.0,
                            bar_y,
                            sidebar_w,
                            row_h,
                            style.background2.to_array(),
                        );
                        let half = (sidebar_w / 2.0).floor();
                        let is_alpha = notes_sort_mode <= 1;
                        let is_recent = notes_sort_mode >= 2;
                        let arrow = |asc: bool| if asc { "\u{2191}" } else { "\u{2193}" };
                        let alpha_arrow = arrow(notes_sort_mode == 0);
                        let recent_arrow = arrow(notes_sort_mode == 3);
                        let alpha_label = format!("A-Z {alpha_arrow}");
                        let recent_label = format!("Recent {recent_arrow}");
                        if is_alpha {
                            draw_ctx.draw_rect(
                                0.0,
                                bar_y,
                                half,
                                row_h,
                                style.line_highlight.to_array(),
                            );
                        }
                        if is_recent {
                            draw_ctx.draw_rect(
                                half,
                                bar_y,
                                sidebar_w - half,
                                row_h,
                                style.line_highlight.to_array(),
                            );
                        }
                        let alpha_w = draw_ctx.font_width(style.font, &alpha_label);
                        let recent_w = draw_ctx.font_width(style.font, &recent_label);
                        let text_y = bar_y + (row_h - style.font_height) / 2.0;
                        draw_ctx.draw_text(
                            style.font,
                            &alpha_label,
                            (half - alpha_w) / 2.0,
                            text_y,
                            if is_alpha {
                                style.accent.to_array()
                            } else {
                                style.dim.to_array()
                            },
                        );
                        draw_ctx.draw_text(
                            style.font,
                            &recent_label,
                            half + (sidebar_w - half - recent_w) / 2.0,
                            text_y,
                            if is_recent {
                                style.accent.to_array()
                            } else {
                                style.dim.to_array()
                            },
                        );
                        draw_ctx.draw_rect(
                            half,
                            bar_y + style.padding_y * 0.3,
                            style.divider_size,
                            row_h - style.padding_y * 0.6,
                            style.divider.to_array(),
                        );
                        // Search input row.
                        let search_y = bar_y + row_h;
                        let search_bg = if notes_search_focused {
                            style.background.to_array()
                        } else {
                            style.background3.to_array()
                        };
                        draw_ctx.draw_rect(
                            style.padding_x,
                            search_y + style.padding_y * 0.4,
                            sidebar_w - style.padding_x * 2.0,
                            search_h - style.padding_y * 0.8,
                            search_bg,
                        );
                        let label = if notes_search.is_empty() && !notes_search_focused {
                            "Search notes..."
                        } else {
                            notes_search.as_str()
                        };
                        let label_color = if notes_search.is_empty() && !notes_search_focused {
                            style.dim.to_array()
                        } else {
                            style.text.to_array()
                        };
                        draw_ctx.draw_text(
                            style.font,
                            label,
                            style.padding_x * 2.0,
                            search_y + (search_h - style.font_height) / 2.0,
                            label_color,
                        );
                        // Caret when focused.
                        if notes_search_focused {
                            let caret_x = style.padding_x * 2.0
                                + draw_ctx.font_width(style.font, &notes_search);
                            draw_ctx.draw_rect(
                                caret_x,
                                search_y + style.padding_y * 0.5,
                                1.0,
                                style.font_height,
                                style.caret.to_array(),
                            );
                        }
                        draw_ctx.draw_rect(
                            0.0,
                            bar_y + row_h + search_h - style.divider_size,
                            sidebar_w,
                            style.divider_size,
                            style.divider.to_array(),
                        );
                        row_h + search_h
                    } else {
                        0.0
                    };

                    // File tree entries — clip to the area below the header so
                    // scrolled entries don't overdraw the toolbar or folder name.
                    let entry_h = style.font_height + style.padding_y;
                    let icon_font_h = draw_ctx.font_height(style.icon_font);
                    let icon_w = draw_ctx.font_width(style.icon_font, "D") + style.padding_x * 0.5;
                    let active_path = docs.get(active_tab).map(|d| d.path.as_str()).unwrap_or("");
                    let sidebar_content_top = toolbar_h + dir_header_h + notes_row_h;
                    draw_ctx.set_clip_rect(
                        0.0,
                        sidebar_content_top,
                        sidebar_w,
                        height - sidebar_content_top,
                    );
                    let notes_display: Vec<usize> = if subsystems.has_notes_mode() {
                        compute_notes_display_order(
                            &sidebar_entries,
                            &notes_search,
                            notes_sort_mode,
                        )
                    } else {
                        (0..sidebar_entries.len()).collect()
                    };
                    let mut ey = toolbar_h + dir_header_h + notes_row_h - sidebar_scroll;
                    for &disp_idx in &notes_display {
                        let entry = &sidebar_entries[disp_idx];
                        if ey + entry_h > sidebar_content_top && ey < height {
                            let indent = entry.depth as f64 * style.padding_x * 1.5;
                            let x = style.padding_x + indent;
                            let text_y = ey + (entry_h - style.font_height) / 2.0;

                            // Highlight active file.
                            let is_active = !entry.is_dir && entry.path == active_path;
                            if is_active {
                                let mut hl = style.line_highlight.to_array();
                                hl[3] = 210.min(hl[3].saturating_add(100));
                                draw_ctx.draw_rect(0.0, ey, sidebar_w, entry_h, hl);
                            }

                            // Icon (vertically centered in the row).
                            if entry.is_dir {
                                let icon = if entry.expanded { "D" } else { "d" };
                                let icon_y = ey + (entry_h - icon_font_h) / 2.0;
                                // Centre the folder glyph's advance in the
                                // icon column the same way file icons are
                                // centred — otherwise folder rows looked
                                // outdented next to the now-centred file
                                // rows.
                                let folder_w = draw_ctx.font_width(style.icon_font, icon);
                                let folder_x = x + (icon_w - folder_w) / 2.0;
                                draw_ctx.draw_text(
                                    style.icon_font,
                                    icon,
                                    folder_x,
                                    icon_y,
                                    style.accent.to_array(),
                                );
                            } else {
                                // Seti file-type icon glyph.
                                let ext = entry.name.rsplit('.').next().unwrap_or("");
                                let icon_info = file_icons
                                    .get(ext)
                                    .or_else(|| file_icons.get(entry.name.as_str()))
                                    .or_else(|| file_icons.get("_default"));
                                if let Some(fi) = icon_info {
                                    let glyph = char::from_u32(fi.codepoint)
                                        .map(|c| c.to_string())
                                        .unwrap_or_default();
                                    // Codepoints below seti.ttf's private-use
                                    // range (U+E000+) aren't in that font; use
                                    // the body font so `file_icons.json` can
                                    // map an extension to a plain ASCII letter
                                    // (e.g. `G` for Gossamer). Body letters
                                    // render smaller than the surrounding
                                    // seti glyphs — the centring math below
                                    // still places them on-axis, just at the
                                    // body font's natural visual weight.
                                    let icon_font = if fi.codepoint < 0xE000 {
                                        style.font
                                    } else {
                                        style.seti_font
                                    };
                                    // Vertical: centre against seti's line
                                    // height regardless of which font drew it,
                                    // so a body-font letter sits on the same
                                    // baseline as the seti icons in adjacent
                                    // rows.
                                    let seti_h = draw_ctx.font_height(style.seti_font);
                                    let icon_y = ey + (entry_h - seti_h) / 2.0;
                                    // Horizontal: centre each glyph's advance
                                    // box in the icon column. The default
                                    // plaintext seti glyph has an advance
                                    // wider than `icon_w` and so produces a
                                    // negative offset — that's intentional, a
                                    // small leftward bleed into the indent
                                    // gutter is invisible and pulls the
                                    // glyph's visual centre back over the
                                    // column centre. Without it, plaintext
                                    // (and any other wide-advance icon) read
                                    // as lopsided to the right.
                                    let glyph_w = draw_ctx.font_width(icon_font, &glyph);
                                    let icon_x = x + (icon_w - glyph_w) / 2.0;
                                    draw_ctx.draw_text(icon_font, &glyph, icon_x, icon_y, fi.color);
                                }
                            }

                            // Name (vertically centered, same baseline alignment).
                            // Add spacing between icon and name.
                            let name_x = x + icon_w + style.padding_x * 0.7;
                            let name_color = if entry.is_dir {
                                style.accent.to_array()
                            } else {
                                style.text.to_array()
                            };
                            // Ellipsize if the name would overflow the sidebar width.
                            let avail = (sidebar_w - name_x - style.padding_x - style.divider_size)
                                .max(0.0);
                            let display_name: String =
                                if draw_ctx.font_width(style.font, &entry.name) <= avail {
                                    entry.name.clone()
                                } else {
                                    let ell = "...";
                                    let ell_w = draw_ctx.font_width(style.font, ell);
                                    let chars: Vec<char> = entry.name.chars().collect();
                                    let mut fit = String::new();
                                    for take in (0..chars.len()).rev() {
                                        let candidate: String = chars[..take].iter().collect();
                                        if draw_ctx.font_width(style.font, &candidate) + ell_w
                                            <= avail
                                        {
                                            fit = format!("{candidate}{ell}");
                                            break;
                                        }
                                    }
                                    if fit.is_empty() { ell.to_string() } else { fit }
                                };
                            draw_ctx.draw_text(
                                style.font,
                                &display_name,
                                name_x,
                                text_y,
                                name_color,
                            );
                        }
                        ey += entry_h;
                    }
                    // Inline new-file input: draws an extra row at the bottom
                    // of the target directory's children.
                    if let Some(ref new_dir) = sidebar_new_file_dir {
                        // Find the display row to insert after (the last entry
                        // still inside `new_dir`, or right after the dir itself).
                        let mut insert_disp_row = notes_display.len();
                        let mut nf_dir_depth = 0usize;
                        let mut found_dir = false;
                        for (row, &disp_idx) in notes_display.iter().enumerate() {
                            let e = &sidebar_entries[disp_idx];
                            if !found_dir {
                                if e.is_dir && &e.path == new_dir {
                                    found_dir = true;
                                    nf_dir_depth = e.depth;
                                }
                            } else if e.depth <= nf_dir_depth {
                                insert_disp_row = row;
                                break;
                            }
                        }
                        let nf_indent = (nf_dir_depth + 1) as f64 * style.padding_x * 1.5;
                        let nf_x = style.padding_x + nf_indent;
                        let nf_y = toolbar_h + dir_header_h + notes_row_h - sidebar_scroll
                            + insert_disp_row as f64 * entry_h;
                        if nf_y + entry_h > sidebar_content_top && nf_y < height {
                            // Selection-tinted row background.
                            draw_ctx.draw_rect(
                                0.0,
                                nf_y,
                                sidebar_w,
                                entry_h,
                                style.selection.to_array(),
                            );
                            // Text and cursor for the filename being typed.
                            let text_x = nf_x + icon_w + style.padding_x * 0.7;
                            let text_y_pos = nf_y + (entry_h - style.font_height) / 2.0;
                            draw_ctx.draw_text(
                                style.font,
                                &sidebar_new_file_name,
                                text_x,
                                text_y_pos,
                                style.text.to_array(),
                            );
                            let cursor_safe =
                                sidebar_new_file_cursor.min(sidebar_new_file_name.len());
                            let before_cursor = &sidebar_new_file_name[..cursor_safe];
                            let cursor_x = text_x + draw_ctx.font_width(style.font, before_cursor);
                            draw_ctx.draw_rect(
                                cursor_x,
                                text_y_pos,
                                style.caret_width,
                                style.font_height,
                                style.caret.to_array(),
                            );
                        }
                    }

                    // Reset clip to full window for the sidebar edge divider.
                    crate::editor::app_state::clip_init(width, height);

                    // Sidebar scrollbar (lite-xl style): proportional thumb
                    // with a minimum size, drawn just inside the right edge.
                    let extra_row = sidebar_new_file_dir.is_some() as usize;
                    let total_entries_h = (notes_display.len() + extra_row) as f64 * entry_h;
                    let sb_area_y = sidebar_content_top;
                    let sb_area_h = (height - sidebar_content_top).max(0.0);
                    sidebar_content_h = total_entries_h;
                    sidebar_sb_top = sb_area_y;
                    sidebar_sb_h = sb_area_h;
                    if total_entries_h > sb_area_h && sb_area_h > 0.0 {
                        let sb_w = style.scrollbar_size;
                        let sb_x = sidebar_w - style.divider_size - sb_w;
                        draw_ctx.draw_rect(
                            sb_x,
                            sb_area_y,
                            sb_w,
                            sb_area_h,
                            style.scrollbar_track.to_array(),
                        );
                        let ratio = sb_area_h / total_entries_h;
                        let min_thumb = style.scrollbar_size * 2.0;
                        let thumb_h = (sb_area_h * ratio).max(min_thumb).min(sb_area_h);
                        let max_scroll = (total_entries_h - sb_area_h).max(1.0);
                        let scroll_frac = (sidebar_scroll / max_scroll).clamp(0.0, 1.0);
                        let thumb_y = sb_area_y + scroll_frac * (sb_area_h - thumb_h);
                        draw_ctx.draw_rect(
                            sb_x,
                            thumb_y,
                            sb_w,
                            thumb_h,
                            style.scrollbar.to_array(),
                        );
                    }

                    // Divider on the right edge.
                    draw_ctx.draw_rect(
                        sidebar_w - style.divider_size,
                        0.0,
                        style.divider_size,
                        height,
                        style.divider.to_array(),
                    );
                    crate::editor::app_state::clip_init(width, height);
                }

                if let Some(doc) = docs.get(active_tab) {
                    let dv = &doc.view;
                    if let Some(buf_id) = dv.buffer_id {
                        let ext = doc.path.rsplit('.').next().unwrap_or("");
                        let policy = crate::editor::open_doc::doc_policy(doc);
                        // Compile-on-demand and bump MRU. Evict the LRU
                        // entry once the cache exceeds SYNTAX_CACHE_CAP
                        // so memory doesn't grow unbounded on sessions
                        // that touch many file types.
                        let compiled_opt = if policy.plain_text {
                            None
                        } else {
                            let ext_owned = ext.to_string();
                            compiled_syntax_mru.retain(|e| e != &ext_owned);
                            compiled_syntax_mru.insert(0, ext_owned.clone());
                            while compiled_syntax_mru.len() > SYNTAX_CACHE_CAP {
                                if let Some(drop_ext) = compiled_syntax_mru.pop() {
                                    compiled_syntax_cache.remove(&drop_ext);
                                }
                            }
                            compiled_syntax_cache
                                .entry(ext_owned)
                                .or_insert_with(|| {
                                    let filename = doc.path.rsplit('/').next().unwrap_or(&doc.path);
                                    match tokenizer::compile_for_filename(filename, &syntax_index) {
                                        Ok(compiled) => compiled,
                                        Err(e) => {
                                            log_to_file(
                                                userdir,
                                                &format!("Syntax compile error: {e:?}"),
                                            );
                                            None
                                        }
                                    }
                                })
                                .as_ref()
                        };
                        let wrap_w = if line_wrapping {
                            Some(dv.rect().w - dv.gutter_width - style.padding_x * 2.0)
                        } else {
                            None
                        };
                        let is_lsp_file = !policy.disable_lsp
                            && ext_to_lsp_filetype(ext)
                                .map(|ft| ft == lsp_state.filetype)
                                .unwrap_or(false);
                        let active_uri = if doc.path.is_empty() {
                            String::new()
                        } else {
                            path_to_uri(&doc.path)
                        };
                        let empty_hints = Vec::new();
                        // Only use held hints if they belong to the active file.
                        // After a tab-switch the cached `inlay_hints` still
                        // contain entries from the previous file; rendering
                        // them here would show ghost hints at mismatched line
                        // numbers until the new file's response arrives.
                        let hints = if subsystems.has_lsp()
                            && is_lsp_file
                            && lsp_state.inlay_hints_uri == active_uri
                        {
                            &lsp_state.inlay_hints
                        } else {
                            &empty_hints
                        };
                        // Cache render lines to avoid re-tokenizing on every
                        // cursor move. Invalidate when hint count changes so LSP
                        // inlay hints appear as soon as they arrive.
                        let current_change_id =
                            buffer::with_buffer(buf_id, |b| Ok(b.change_id)).unwrap_or(0);
                        let scroll_y_now = dv.scroll_y;
                        let hint_count_now = hints.len();
                        // `cached_render` is Arc-shared so the cache-hit
                        // path is a refcount bump rather than a full
                        // `Vec<RenderLine>` clone per redraw.
                        let render_lines: std::sync::Arc<Vec<RenderLine>> =
                            if let Some(doc) = docs.get(active_tab) {
                                if doc.cached_change_id == current_change_id
                                    && (doc.cached_scroll_y - scroll_y_now).abs() < 0.5
                                    && doc.cached_hint_count == hint_count_now
                                    && (doc.cached_rect_w - dv.rect().w).abs() < 0.5
                                    && (doc.cached_rect_h - dv.rect().h).abs() < 0.5
                                    && !doc.cached_render.is_empty()
                                    && !doc.token_cache.borrow().pending
                                {
                                    std::sync::Arc::clone(&doc.cached_render)
                                } else {
                                    std::sync::Arc::new(build_render_lines(
                                        buf_id,
                                        dv,
                                        &style,
                                        ext,
                                        compiled_opt,
                                        wrap_w,
                                        hints,
                                        Some(&doc.token_cache),
                                    ))
                                }
                            } else {
                                std::sync::Arc::new(build_render_lines(
                                    buf_id,
                                    dv,
                                    &style,
                                    ext,
                                    compiled_opt,
                                    wrap_w,
                                    hints,
                                    Some(&doc.token_cache),
                                ))
                            };
                        let (sel, cursor_line, cursor_col, all_cursors) =
                            buffer::with_buffer(buf_id, |b| {
                                let mut sels = Vec::new();
                                let mut cursors = Vec::new();
                                let n = buffer::cursor_count(b);
                                for i in 0..n {
                                    let base = i * 4;
                                    let l1 = b.selections[base];
                                    let c1 = b.selections[base + 1];
                                    let l2 = b.selections[base + 2];
                                    let c2 = b.selections[base + 3];
                                    cursors.push((l2, c2));
                                    if l1 != l2 || c1 != c2 {
                                        let (sl1, sc1, sl2, sc2) =
                                            if l1 < l2 || (l1 == l2 && c1 <= c2) {
                                                (l1, c1, l2, c2)
                                            } else {
                                                (l2, c2, l1, c1)
                                            };
                                        sels.push(crate::editor::doc_view::SelectionRange {
                                            line1: sl1,
                                            col1: sc1,
                                            line2: sl2,
                                            col2: sc2,
                                        });
                                    }
                                }
                                // Primary cursor is the first one (for scrolling).
                                let pl = b.selections.get(2).copied().unwrap_or(1);
                                let pc = b.selections.get(3).copied().unwrap_or(1);
                                Ok((sels, pl, pc, cursors))
                            })
                            .unwrap_or((vec![], 1, 1, vec![(1, 1)]));
                        let elapsed_since_reset = cursor_blink_reset.elapsed().as_secs_f64();
                        let cursor_on = elapsed_since_reset < blink_period
                            || (elapsed_since_reset % (blink_period * 2.0)) < blink_period;
                        // Highlight other occurrences of a compact, single-line,
                        // whitespace-free selection (a "word").
                        let occurrence: String = doc
                            .view
                            .buffer_id
                            .and_then(|bid| {
                                buffer::with_buffer(bid, |b| {
                                    let s = &b.selections;
                                    if s.len() == 4 && s[0] == s[2] && s[1] != s[3] {
                                        let (cs, ce) = if s[1] < s[3] {
                                            (s[1], s[3])
                                        } else {
                                            (s[3], s[1])
                                        };
                                        let text = b
                                            .lines
                                            .get(s[0].saturating_sub(1))
                                            .map(|l| l.trim_end_matches('\n'))
                                            .unwrap_or("");
                                        let word: String = text
                                            .chars()
                                            .skip(cs.saturating_sub(1))
                                            .take(ce - cs)
                                            .collect();
                                        let ok = !word.is_empty()
                                            && word.chars().count() <= 100
                                            && word.chars().all(|ch| !ch.is_whitespace());
                                        Ok(if ok { word } else { String::new() })
                                    } else {
                                        Ok(String::new())
                                    }
                                })
                                .ok()
                            })
                            .unwrap_or_default();
                        dv.draw_native(
                            &mut draw_ctx,
                            &style,
                            &render_lines,
                            &sel,
                            cursor_line,
                            cursor_col,
                            cursor_on,
                            &doc.git_changes,
                            &all_cursors,
                            &occurrence,
                        );

                        // Test-runner badges: scan the doc for recognised
                        // test definitions and paint a "Run test" CodeLens-
                        // style hint in `style.dim` (greys with the theme,
                        // matches VS Code's descriptionForeground). Only
                        // runs if a runner can be detected -- no point
                        // offering the affordance if nothing can execute.
                        use crate::editor::view::DrawContext as _;
                        test_badges.clear();
                        if !doc.path.is_empty()
                            && crate::editor::test_runner::supports_test_discovery(&doc.path)
                            && !(crate::editor::open_doc::doc_policy(doc).plain_text)
                        {
                            // Rescan only when the file or its content changed;
                            // detection probes the filesystem and discovery
                            // clones the whole document, so neither can run on
                            // every redraw (scroll, cursor blink, mouse move).
                            if test_scan_cache.0 != doc.path
                                || test_scan_cache.1 != current_change_id
                            {
                                let has_runner =
                                    crate::editor::test_runner::detect_runner_with_fallback(
                                        &project_root,
                                        &doc.path,
                                    )
                                    .is_some();
                                active_tests = if has_runner {
                                    buffer::with_buffer(buf_id, |b| {
                                        Ok(crate::editor::test_runner::discover_tests(
                                            &doc.path, &b.lines,
                                        ))
                                    })
                                    .unwrap_or_default()
                                } else {
                                    Vec::new()
                                };
                                test_scan_cache = (doc.path.clone(), current_change_id);
                            }
                            // Render loops over `active_tests`, which is empty
                            // when no runner was detected, so this is a no-op in
                            // that case without a second guard.
                            let line_h = style.code_font_height * 1.2;
                            let dv_rect = dv.rect();
                            // Plain ASCII text so no font has to carry a
                            // triangle glyph; the previous "▶" rendered as
                            // a .notdef box in Lilex and other code fonts
                            // that don't cover U+25B6.
                            let badge_text = "Run test";
                            let badge_w =
                                draw_ctx.font_width(style.font, badge_text) + style.padding_x;
                            for (i, test) in active_tests.iter().enumerate() {
                                // Render on the SAME row as the `fn` line
                                // (`test.line`), right-aligned. That puts
                                // the hint visually below any decorator /
                                // #[test] attribute and above the function
                                // body -- the closest single-row
                                // approximation to VS Code's dedicated
                                // CodeLens row. Right-aligning keeps it
                                // away from the fn signature for most
                                // common fn widths.
                                let fn_line = test.line.max(1);
                                let row_y =
                                    dv_rect.y + (fn_line as f64 - 1.0) * line_h - dv.scroll_y;
                                if row_y + line_h < dv_rect.y || row_y >= dv_rect.y + dv_rect.h {
                                    continue;
                                }
                                let badge_x = (dv_rect.x + dv_rect.w
                                    - style.scrollbar_size
                                    - badge_w
                                    - style.padding_x)
                                    .max(dv_rect.x);
                                draw_ctx.draw_text(
                                    style.font,
                                    badge_text,
                                    badge_x,
                                    row_y + (line_h - style.font_height) / 2.0,
                                    style.dim.to_array(),
                                );
                                test_badges.push(crate::editor::test_runner::TestBadgeRegion {
                                    x1: badge_x,
                                    y1: row_y,
                                    x2: badge_x + badge_w,
                                    y2: row_y + line_h,
                                    test_index: i,
                                });
                            }
                        } else {
                            active_tests.clear();
                            test_scan_cache = (doc.path.clone(), current_change_id);
                        }

                        pending_render_cache = Some((
                            active_tab,
                            buf_id,
                            render_lines,
                            current_change_id,
                            scroll_y_now,
                            hint_count_now,
                            dv.rect().w,
                            dv.rect().h,
                        ));
                        syntax_pending_after_frame = doc.token_cache.borrow().pending;
                        // Draw bracket match underlines at cursor position.
                        if let Some(buf_id) = dv.buffer_id
                            && !(crate::editor::open_doc::doc_policy(doc).plain_text)
                        {
                            let bracket = buffer::with_buffer(buf_id, |b| {
                                Ok(crate::editor::picker::bracket_pair(
                                    &b.lines,
                                    cursor_line,
                                    cursor_col,
                                ))
                            })
                            .ok()
                            .flatten();
                            if let Some((l1, c1, l2, c2)) = bracket {
                                use crate::editor::view::DrawContext as _;
                                let line_h = style.code_font_height * 1.2;
                                let gutter_w = dv.gutter_width;
                                let doc_x = dv.rect().x + gutter_w + style.padding_x;
                                let doc_y = dv.rect().y;
                                let char_w = draw_ctx.font_width(style.code_font, "m");
                                let caret_color = style.caret.to_array();
                                // Underline at first bracket.
                                let y1 =
                                    doc_y + (l1 as f64 - 1.0) * line_h + line_h - 2.0 - dv.scroll_y;
                                let x1 = doc_x + (c1 as f64 - 1.0) * char_w - dv.scroll_x;
                                if y1 >= doc_y && y1 <= doc_y + dv.rect().h {
                                    draw_ctx.draw_rect(x1, y1, char_w, 2.0, caret_color);
                                }
                                // Underline at second bracket.
                                let y2 =
                                    doc_y + (l2 as f64 - 1.0) * line_h + line_h - 2.0 - dv.scroll_y;
                                let x2 = doc_x + (c2 as f64 - 1.0) * char_w - dv.scroll_x;
                                if y2 >= doc_y && y2 <= doc_y + dv.rect().h {
                                    draw_ctx.draw_rect(x2, y2, char_w, 2.0, caret_color);
                                }
                            }
                        }
                        // Draw diagnostic underlines from LSP (only for LSP-handled files).
                        if subsystems.has_lsp()
                            && is_lsp_file
                            && let Some(diags) = lsp_state.diagnostics.get(&doc.path)
                        {
                            let line_h = style.code_font_height * 1.2;
                            let gutter_w = dv.gutter_width;
                            let doc_x = dv.rect().x + gutter_w + style.padding_x;
                            let doc_y = dv.rect().y;
                            for diag in diags {
                                let color = match diag.severity {
                                    1 => style.error.to_array(),
                                    2 => style.warn.to_array(),
                                    _ => style.dim.to_array(),
                                };
                                let end_col = if diag.end_col == diag.start_col {
                                    diag.start_col + 1
                                } else {
                                    diag.end_col
                                };
                                // LSP lines are 0-based.
                                let y_pos = doc_y + (diag.start_line as f64) * line_h + line_h
                                    - 2.0
                                    - dv.scroll_y;
                                if y_pos < doc_y || y_pos > doc_y + dv.rect().h {
                                    continue;
                                }
                                use crate::editor::view::DrawContext as _;
                                let char_w = draw_ctx.font_width(style.code_font, "m");
                                let x1 = doc_x + diag.start_col as f64 * char_w - dv.scroll_x;
                                let x2 = doc_x + end_col as f64 * char_w - dv.scroll_x;
                                let w = (x2 - x1).max(char_w);
                                draw_ctx.draw_rect(x1, y_pos, w, 2.0, color);
                            }
                        }
                    }
                    // Git blame annotations (right-aligned, dimmed).
                    if subsystems.has_git() && git_blame_active && !git_blame_lines.is_empty() {
                        if let Some(doc) = docs.get(active_tab) {
                            let dv = &doc.view;
                            use crate::editor::view::DrawContext as _;
                            let line_h = style.code_font_height * 1.2;
                            let first = ((dv.scroll_y / line_h).floor() as usize) + 1;
                            let vis = ((dv.rect().h / line_h).ceil() as usize) + 2;
                            let blame_color = style.dim.to_array();
                            let right_edge = dv.rect().x + dv.rect().w - style.padding_x;
                            for row in 0..vis {
                                let ln = first + row;
                                if ln > git_blame_lines.len() {
                                    break;
                                }
                                let annotation = &git_blame_lines[ln - 1];
                                let aw = draw_ctx.font_width(style.font, annotation);
                                let ax = (right_edge - aw).max(dv.rect().x + dv.gutter_width);
                                let ay = dv.rect().y + (ln as f64 - 1.0) * line_h - dv.scroll_y
                                    + (line_h - style.font_height) / 2.0;
                                if ay >= dv.rect().y
                                    && ay + style.font_height <= dv.rect().y + dv.rect().h
                                {
                                    draw_ctx.draw_text(style.font, annotation, ax, ay, blame_color);
                                }
                            }
                        }
                    }

                    // Inlay hints are injected into render_lines via build_render_lines.
                    // Reset clip before drawing minimap.
                    crate::editor::app_state::clip_init(width, height);
                    if minimap_visible {
                        use crate::editor::view::DrawContext as _;
                        let mm_x = width - minimap_w;
                        let mm_y = tab_h;
                        let mm_h = height - tab_h - terminal_h - status_h;
                        let mlh = 4.0_f64;
                        let text_padding = 4.0;
                        let usable_w = minimap_w - text_padding * 2.0;
                        let ref_cols = 80.0_f64;
                        let fixed_char_w = usable_w / ref_cols;
                        let block_height = (mlh * 0.6).max(1.0);
                        let block_y_pad = (mlh - block_height) / 2.0;

                        // Background.
                        let mut bg = style.background.to_array();
                        bg[3] = 230;
                        draw_ctx.draw_rect(mm_x, mm_y, minimap_w, mm_h, bg);
                        // Left border.
                        draw_ctx.draw_rect(mm_x, mm_y, 1.0, mm_h, [80, 80, 80, 60]);

                        let total_lines =
                            buffer::with_buffer(dv.buffer_id.unwrap_or(0), |b| Ok(b.lines.len()))
                                .unwrap_or(0);
                        if total_lines > 0 {
                            let doc_line_h = style.code_font_height * 1.2;
                            let visible_lines = (dv.rect().h / doc_line_h).ceil() as usize;
                            let first_visible = (dv.scroll_y / doc_line_h).floor() as usize + 1;
                            let last_visible = first_visible + visible_lines;
                            let vis_center = (first_visible + last_visible) / 2;
                            let lines_that_fit = (mm_h / mlh).floor() as usize;

                            let minimap_start = if total_lines <= lines_that_fit {
                                1
                            } else {
                                let half = lines_that_fit / 2;
                                let start = vis_center.saturating_sub(half).max(1);
                                start.min(total_lines.saturating_sub(lines_that_fit) + 1)
                            };
                            let minimap_end = (minimap_start + lines_that_fit).min(total_lines + 1);

                            // Draw colored blocks for each line.
                            let _ = buffer::with_buffer(dv.buffer_id.unwrap_or(0), |b| {
                                for line_idx in minimap_start..minimap_end {
                                    if line_idx > b.lines.len() {
                                        break;
                                    }
                                    let y_pos = mm_y
                                        + (line_idx - minimap_start) as f64 * mlh
                                        + block_y_pad;
                                    let raw = &b.lines[line_idx - 1];
                                    let text = raw.trim_end_matches('\n');
                                    if text.is_empty() {
                                        continue;
                                    }

                                    // Reuse tokens already produced for the
                                    // editor viewport. A cache miss draws a
                                    // neutral block instead of synchronously
                                    // tokenizing minimap lines on every frame.
                                    let cached_tokens = doc
                                        .token_cache
                                        .borrow()
                                        .lines
                                        .get(&line_idx)
                                        .filter(|entry| {
                                            entry.content_hash
                                                == crate::editor::open_doc::line_hash(raw)
                                        })
                                        .and_then(|entry| entry.tokens.clone());
                                    if let Some(toks) = cached_tokens {
                                        let mut x_off = 0.0;
                                        for t in toks.iter() {
                                            let text_len = t.text.len();
                                            if text_len > 0 {
                                                let draw_len = if t.text.ends_with('\n') {
                                                    text_len - 1
                                                } else {
                                                    text_len
                                                };
                                                if draw_len > 0 {
                                                    let trimmed =
                                                        t.text.trim_start_matches([' ', '\t']);
                                                    let leading = text_len - trimmed.len();
                                                    let trimmed_draw =
                                                        draw_len.saturating_sub(leading);
                                                    if trimmed_draw > 0 {
                                                        let seg_x = (x_off
                                                            + leading as f64 * fixed_char_w)
                                                            .min(usable_w);
                                                        let seg_w = (trimmed_draw as f64
                                                            * fixed_char_w)
                                                            .min(usable_w - seg_x + text_padding);
                                                        if seg_w > 0.2 {
                                                            let mut color =
                                                                syntax_color(&t.token_type, &style);
                                                            color[3] = 130;
                                                            draw_ctx.draw_rect(
                                                                mm_x + text_padding + seg_x,
                                                                y_pos,
                                                                seg_w,
                                                                block_height,
                                                                color,
                                                            );
                                                        }
                                                    }
                                                }
                                                x_off += text_len as f64 * fixed_char_w;
                                            }
                                        }
                                    } else {
                                        let trimmed = text.trim_start();
                                        let leading = text.len() - trimmed.len();
                                        let draw_len =
                                            trimmed.len().min((usable_w / fixed_char_w) as usize);
                                        if draw_len > 0 {
                                            let seg_x = leading as f64 * fixed_char_w;
                                            let mut color = style.dim.to_array();
                                            color[3] = 130;
                                            draw_ctx.draw_rect(
                                                mm_x + text_padding + seg_x,
                                                y_pos,
                                                draw_len as f64 * fixed_char_w,
                                                block_height,
                                                color,
                                            );
                                        }
                                    }
                                }
                                Ok(())
                            });

                            // Viewport indicator.
                            if first_visible >= minimap_start && first_visible < minimap_end {
                                let ind_y = mm_y + (first_visible - minimap_start) as f64 * mlh;
                                let ind_h = (last_visible - first_visible) as f64 * mlh;
                                let clamped_h = ind_h.min(mm_h - (ind_y - mm_y));
                                let mut sel = style.selection.to_array();
                                sel[3] = 76;
                                draw_ctx.draw_rect(mm_x, ind_y, minimap_w, clamped_h, sel);
                            }
                        }
                    }
                } else {
                    empty_view.draw_native(&mut draw_ctx, &style);
                }
                crate::editor::app_state::clip_init(width, height);

                // Markdown preview pane (split, drawn to the right of the
                // editor view when enabled on the active doc). Runs after
                // the normal doc draw so it renders into its own rect.
                if let Some(doc) = docs.get_mut(active_tab) {
                    let preview_suppressed = crate::editor::open_doc::doc_policy(doc).plain_text;
                    if !preview_suppressed && doc.preview.enabled && doc.preview.rect.w > 0.0 {
                        if let Some(buf_id) = doc.view.buffer_id {
                            // Reparse the source when the buffer changes.
                            let cur_change_id =
                                buffer::with_buffer(buf_id, |b| Ok(b.change_id)).unwrap_or(0);
                            if cur_change_id != doc.preview.cached_change_id
                                && cur_change_id != doc.preview.pending_change_id
                            {
                                doc.preview.pending_change_id = cur_change_id;
                                doc.preview.reparse_at =
                                    Some(Instant::now() + std::time::Duration::from_millis(150));
                            }
                            let preview_reparse_ready = doc
                                .preview
                                .reparse_at
                                .map(|deadline| Instant::now() >= deadline)
                                .unwrap_or(false);
                            if preview_reparse_ready {
                                let _perf = crate::editor::perf::span("markdown_preview_reparse");
                                let text = buffer::with_buffer(buf_id, |b| Ok(b.lines.join("")))
                                    .unwrap_or_default();
                                doc.preview.blocks = crate::editor::markdown::parse(&text);
                                doc.preview.cached_change_id = doc.preview.pending_change_id;
                                doc.preview.reparse_at = None;
                                doc.preview.layout.clear();
                                // Pre-tokenize every fenced code block with a
                                // resolvable `lang` so the preview can render
                                // it with syntax colours. Lookup reuses the
                                // editor's compiled-syntax cache keyed by file
                                // extension.
                                doc.preview.code_tokens = doc
                                    .preview
                                    .blocks
                                    .iter()
                                    .map(|blk| {
                                        let (lang, code_text) = match blk {
                                            crate::editor::markdown::Block::Code {
                                                lang: Some(l),
                                                text,
                                            } => (l.as_str(), text.as_str()),
                                            _ => return None,
                                        };
                                        let ext = markdown_lang_to_ext(lang);
                                        let ext_owned = ext.to_string();
                                        let pseudo = format!("f.{ext}");
                                        let compiled_opt = compiled_syntax_cache
                                            .entry(ext_owned.clone())
                                            .or_insert_with(|| {
                                                tokenizer::compile_for_filename(
                                                    &pseudo,
                                                    &syntax_index,
                                                )
                                                .ok()
                                                .flatten()
                                            })
                                            .as_ref()?;
                                        // Touch MRU so preview-only highlights
                                        // don't immediately get evicted.
                                        compiled_syntax_mru.retain(|e| e != &ext_owned);
                                        compiled_syntax_mru.insert(0, ext_owned);
                                        // Keep preview highlighting bounded for
                                        // generated or accidentally huge code
                                        // fences. The code still renders, using
                                        // the normal preview text color.
                                        if code_text.len() > 512 * 1024
                                            || code_text.lines().count() > 2000
                                        {
                                            None
                                        } else {
                                            Some(
                                                code_text
                                                    .split('\n')
                                                    .map(|line| {
                                                        tokenizer::tokenize_line(compiled_opt, line)
                                                    })
                                                    .collect(),
                                            )
                                        }
                                    })
                                    .collect();
                            } else if doc.preview.reparse_at.is_some() {
                                // Continue drawing the last complete preview
                                // while the debounce timer is active.
                                syntax_pending_after_frame = true;
                            }
                            let rect = doc.preview.rect;
                            // No smooth scroll: track the target directly so
                            // edits in the source that shrink `content_height`
                            // (and with it `max_scroll`) can't drive a multi-
                            // frame glide, which showed up as the preview
                            // auto-scrolling while the user typed.
                            let max_scroll = (doc.preview.content_height - rect.h).max(0.0);
                            doc.preview.target_scroll_y =
                                doc.preview.target_scroll_y.clamp(0.0, max_scroll);
                            doc.preview.scroll_y = doc.preview.target_scroll_y;
                            // Divider between editor and preview. It takes the
                            // accent colour while the preview holds keyboard
                            // focus, so which pane the arrow keys drive is
                            // visible at a glance.
                            use crate::editor::view::DrawContext as _;
                            let divider_col = if doc.preview.focused {
                                style.accent.to_array()
                            } else {
                                style.divider.to_array()
                            };
                            draw_ctx.draw_rect(
                                rect.x,
                                rect.y,
                                style.divider_size.max(1.0),
                                rect.h,
                                divider_col,
                            );
                            let pane_x = rect.x + style.divider_size.max(1.0);
                            let pane_w = rect.w - style.divider_size.max(1.0);
                            crate::editor::markdown_preview::draw(
                                &mut draw_ctx,
                                &mut doc.preview,
                                &style,
                                pane_x,
                                rect.y,
                                pane_w,
                                rect.h,
                            );
                        }
                    }
                }
                crate::editor::app_state::clip_init(width, height);

                // Draw terminal panel.
                if subsystems.has_terminal() && terminal.visible {
                    use crate::editor::view::DrawContext as _;
                    // Keep the terminal palette in sync with the live theme.
                    let (term_palette, term_default_fg) =
                        crate::editor::terminal_panel::theme_terminal_palette(&style);
                    terminal.set_palette(term_palette, term_default_fg);
                    let term_y = height - terminal_h - status_h;
                    let term_x = sidebar_w;
                    let term_w = width - sidebar_w;
                    // Divider at top of terminal.
                    draw_ctx.draw_rect(
                        term_x,
                        term_y,
                        term_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(
                        term_x,
                        term_y + style.divider_size,
                        term_w,
                        terminal_h - style.divider_size,
                        style.background.to_array(),
                    );
                    // Focus indicator.
                    if terminal.focused {
                        draw_ctx.draw_rect(
                            term_x,
                            term_y,
                            term_w,
                            style.divider_size,
                            style.accent.to_array(),
                        );
                    }
                    // Resize terminal buffer to match panel dimensions.
                    let tab_bar_h_for_resize = if !terminal.terminals.is_empty() {
                        style.font_height + style.padding_y * 3.0
                    } else {
                        0.0
                    };
                    let char_h_resize = style.code_font_height * 1.2;
                    let char_w_resize = draw_ctx.font_width(style.code_font, "m");
                    if char_w_resize > 0.0 && char_h_resize > 0.0 {
                        let avail_h = terminal_h
                            - style.divider_size
                            - tab_bar_h_for_resize
                            - style.padding_y * 2.0;
                        let new_cols =
                            ((term_w - style.padding_x * 2.0) / char_w_resize).max(1.0) as usize;
                        let new_rows = (avail_h / char_h_resize).max(1.0) as usize;
                        if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                            inst.tbuf.resize(new_cols, new_rows);
                        }
                    }
                    // Draw terminal title/tab bar using the same layout as the doc tab bar.
                    let tab_bar_h = if !terminal.terminals.is_empty() {
                        let tbh = style.font_height + style.padding_y * 3.0;
                        let accent_h = 3.0;
                        let tby = term_y + style.divider_size;
                        draw_ctx.draw_rect(term_x, tby, term_w, tbh, style.background2.to_array());
                        let close_w = draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                        let mut tx = term_x;
                        for (i, inst) in terminal.terminals.iter().enumerate() {
                            let label = &inst.title;
                            let label_w = draw_ctx.font_width(style.font, label);
                            let tw = label_w + style.padding_x * 2.0 + close_w;
                            let bg = if i == terminal.active {
                                style.background.to_array()
                            } else {
                                style.background2.to_array()
                            };
                            let fg = if i == terminal.active {
                                style.text.to_array()
                            } else {
                                style.dim.to_array()
                            };
                            draw_ctx.draw_rect(tx, tby + accent_h, tw, tbh - accent_h, bg);
                            if i == terminal.active {
                                draw_ctx.draw_rect(tx, tby, tw, accent_h, style.accent.to_array());
                            }
                            let text_y =
                                tby + accent_h + (tbh - accent_h - style.font_height) / 2.0;
                            draw_ctx.draw_text(style.font, label, tx + style.padding_x, text_y, fg);
                            let close_x = tx + tw - close_w;
                            let close_hovered = mouse_y >= tby
                                && mouse_y < tby + tbh
                                && mouse_x >= close_x
                                && mouse_x < close_x + close_w;
                            if close_hovered {
                                draw_ctx.draw_rect(
                                    close_x,
                                    tby + accent_h,
                                    close_w,
                                    tbh - accent_h,
                                    style.line_highlight.to_array(),
                                );
                            }
                            let close_color = if close_hovered {
                                style.text.to_array()
                            } else {
                                style.dim.to_array()
                            };
                            draw_ctx.draw_text(
                                style.icon_font,
                                "C",
                                close_x + style.padding_x * 0.5,
                                tby + accent_h
                                    + (tbh - accent_h - draw_ctx.font_height(style.icon_font))
                                        / 2.0,
                                close_color,
                            );
                            draw_ctx.draw_rect(
                                tx + tw,
                                tby + style.padding_y * 0.5,
                                style.divider_size,
                                tbh - style.padding_y,
                                style.dim.to_array(),
                            );
                            tx += tw + style.divider_size;
                        }
                        draw_ctx.draw_rect(
                            term_x,
                            tby + tbh - style.divider_size,
                            term_w,
                            style.divider_size,
                            style.divider.to_array(),
                        );
                        tbh
                    } else {
                        0.0
                    };
                    // Draw active terminal buffer text using TerminalBufferInner cell grid.
                    if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                        let char_h = style.code_font_height * 1.2;
                        let char_w = draw_ctx.font_width(style.code_font, "m");
                        let ty_start = term_y + style.divider_size + tab_bar_h + 2.0;
                        let visible_h = (term_y + terminal_h - ty_start - style.padding_y).max(0.0);
                        let rows_visible = (visible_h / char_h).floor().max(1.0) as usize;

                        let cap = inst.tbuf.history_len() as f64;
                        inst.scrollback_target = inst.scrollback_target.clamp(0.0, cap);
                        let diff = inst.scrollback_target - inst.scrollback;
                        if diff.abs() >= 0.5 {
                            inst.scrollback += diff * 0.35;
                            crate::window::force_invalidate();
                        } else if inst.scrollback != inst.scrollback_target {
                            inst.scrollback = inst.scrollback_target;
                        }
                        let scrollback_rows = inst.scrollback.round().max(0.0).min(cap) as usize;
                        let rows_data = inst.tbuf.visible_rows(rows_visible, scrollback_rows);

                        // Normalized selection range for this frame.
                        let sel_range = match (inst.sel_start, inst.sel_end) {
                            (Some(s), Some(e)) => {
                                crate::editor::terminal_panel::normalized_selection(s, e)
                            }
                            _ => None,
                        };

                        let cur_row_1 = inst.tbuf.cursor_row();
                        let cur_col_1 = inst.tbuf.cursor_col();
                        let cur_visible_row = if scrollback_rows == 0 {
                            Some(cur_row_1.saturating_sub(1))
                        } else if scrollback_rows < rows_visible {
                            Some(rows_visible - scrollback_rows + cur_row_1.saturating_sub(1))
                                .filter(|r| *r < rows_visible)
                        } else {
                            None
                        };

                        for (row_idx, row) in rows_data.iter().enumerate() {
                            let ry = ty_start + row_idx as f64 * char_h;
                            if ry + char_h < term_y || ry > term_y + terminal_h {
                                continue;
                            }
                            // Batch adjacent chars with same fg for efficient rendering.
                            let mut run_text = String::new();
                            let mut run_x = term_x + style.padding_x;
                            let mut run_fg: [u8; 4] = style.text.to_array();
                            let mut cx = term_x + style.padding_x;

                            for (col_idx, cell) in row.iter().enumerate() {
                                let ch = char::from_u32(cell.ch).unwrap_or(' ');
                                let fg = crate::editor::terminal::unpack_color(cell.fg)
                                    .unwrap_or(style.text.to_array());
                                let bg = crate::editor::terminal::unpack_color(cell.bg);

                                // Selection highlight for this cell.
                                let in_sel = match sel_range {
                                    Some((a, b)) => {
                                        (row_idx > a.0 && row_idx < b.0)
                                            || (row_idx == a.0
                                                && row_idx == b.0
                                                && col_idx >= a.1
                                                && col_idx < b.1)
                                            || (row_idx == a.0 && row_idx != b.0 && col_idx >= a.1)
                                            || (row_idx == b.0 && row_idx != a.0 && col_idx < b.1)
                                    }
                                    None => false,
                                };
                                if in_sel {
                                    draw_ctx.draw_rect(
                                        cx,
                                        ry,
                                        char_w,
                                        char_h,
                                        style.selection.to_array(),
                                    );
                                }

                                // Draw bg if non-zero (and not already selected).
                                if !in_sel {
                                    if let Some(bg_color) = bg {
                                        if bg_color[3] > 0 && bg_color != [0, 0, 0, 255] {
                                            draw_ctx.draw_rect(cx, ry, char_w, char_h, bg_color);
                                        }
                                    }
                                }

                                // Batch text runs with same fg color.
                                if fg != run_fg && !run_text.is_empty() {
                                    draw_ctx.draw_text(
                                        style.code_font,
                                        &run_text,
                                        run_x,
                                        ry,
                                        run_fg,
                                    );
                                    run_text.clear();
                                    run_x = cx;
                                    run_fg = fg;
                                }
                                if run_text.is_empty() {
                                    run_x = cx;
                                    run_fg = fg;
                                }
                                run_text.push(ch);

                                if terminal.focused
                                    && Some(row_idx) == cur_visible_row
                                    && col_idx == cur_col_1.saturating_sub(1)
                                {
                                    draw_ctx.draw_rect(cx, ry, char_w, char_h, [200, 200, 200, 80]);
                                }
                                cx += char_w;
                            }
                            // Flush remaining text run.
                            if !run_text.is_empty() {
                                draw_ctx.draw_text(style.code_font, &run_text, run_x, ry, run_fg);
                            }
                        }

                        // Scrollbar (shown only when there is history).
                        if cap > 0.0 {
                            let sb_w = style.scrollbar_size.max(6.0);
                            let sb_x = term_x + term_w - sb_w;
                            let sb_y = ty_start;
                            let sb_h = char_h * rows_visible as f64;
                            draw_ctx.draw_rect(
                                sb_x,
                                sb_y,
                                sb_w,
                                sb_h,
                                style.scrollbar_track.to_array(),
                            );
                            let total = cap + rows_visible as f64;
                            let ratio = (rows_visible as f64 / total).clamp(0.0, 1.0);
                            let min_thumb = sb_w * 2.0;
                            let thumb_h = (sb_h * ratio).max(min_thumb).min(sb_h);
                            // scrollback = 0 -> thumb at bottom of track
                            // scrollback = cap -> thumb at top.
                            let pos_from_top = (cap - inst.scrollback) / cap;
                            let thumb_y = sb_y + pos_from_top * (sb_h - thumb_h);
                            draw_ctx.draw_rect(
                                sb_x,
                                thumb_y,
                                sb_w,
                                thumb_h,
                                style.scrollbar.to_array(),
                            );
                        }
                    }
                }

                status_view.draw_native(&mut draw_ctx, &style);

                // Draw find bar (and optionally replace bar) at the top of the editor,
                // just below the tab and breadcrumb bars, so transient UX is consistent.
                // The bar spans only the active editor's column (not the sidebar/minimap)
                // so the user's eye stays anchored to the document being searched.
                if find_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let row_h = style.font_height + style.padding_y * 2.0;
                    let total_rows = if replace_active { 3.0 } else { 2.0 };
                    let bar_x = sidebar_w;
                    let bar_w = (width - sidebar_w - minimap_w).max(0.0);
                    let bar_y = tab_h + breadcrumb_h;
                    let bar_total_h = row_h * total_rows;

                    draw_ctx.draw_rect(
                        bar_x,
                        bar_y,
                        bar_w,
                        bar_total_h,
                        style.background3.to_array(),
                    );
                    draw_ctx.draw_rect(
                        bar_x,
                        bar_y,
                        bar_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(
                        bar_x,
                        bar_y + bar_total_h - style.divider_size,
                        bar_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );

                    // Row 1: Find input + count indicator on the right.
                    let find_cursor = if !find_focus_on_replace { "_" } else { "" };
                    let find_label = format!("Find: {find_query}{find_cursor}");
                    draw_ctx.draw_text(
                        style.font,
                        &find_label,
                        bar_x + style.padding_x,
                        bar_y + style.padding_y,
                        style.text.to_array(),
                    );
                    let count_label = if find_query.is_empty() {
                        String::new()
                    } else if find_matches.is_empty() {
                        "0/0".to_string()
                    } else {
                        let cur = find_current.map(|i| i + 1).unwrap_or(0);
                        format!("{cur}/{}", find_matches.len())
                    };
                    if !count_label.is_empty() {
                        let cw = draw_ctx.font_width(style.font, &count_label);
                        draw_ctx.draw_text(
                            style.font,
                            &count_label,
                            bar_x + bar_w - cw - style.padding_x,
                            bar_y + style.padding_y,
                            if find_matches.is_empty() {
                                style.error.to_array()
                            } else {
                                style.dim.to_array()
                            },
                        );
                    }

                    // Optional Row 2: Replace input.
                    let mut next_row_y = bar_y + row_h;
                    if replace_active {
                        let replace_y = next_row_y;
                        draw_ctx.draw_rect(
                            bar_x,
                            replace_y,
                            bar_w,
                            style.divider_size,
                            style.divider.to_array(),
                        );
                        let repl_cursor = if find_focus_on_replace { "_" } else { "" };
                        let repl_label = format!(
                            "Replace: {replace_query}{repl_cursor}  (Ctrl+Enter replace  Ctrl+Shift+Enter all)"
                        );
                        draw_ctx.draw_text(
                            style.font,
                            &repl_label,
                            bar_x + style.padding_x,
                            replace_y + style.padding_y,
                            style.text.to_array(),
                        );
                        next_row_y += row_h;
                    }

                    // Final row: keybinding hints with on/off indicators for the toggles.
                    let hint_y = next_row_y;
                    draw_ctx.draw_rect(
                        bar_x,
                        hint_y,
                        bar_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    let mark = |on: bool| if on { "[x]" } else { "[ ]" };
                    let hint = format!(
                        "Alt+R Regex {}  Alt+W Word {}  Alt+I Case {}  Alt+S Sel {}   F3 Next  Shift+F3 Prev  Esc Close",
                        mark(find_use_regex),
                        mark(find_whole_word),
                        mark(find_case_insensitive),
                        mark(find_in_selection),
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &hint,
                        bar_x + style.padding_x,
                        hint_y + style.padding_y,
                        style.dim.to_array(),
                    );
                }

                // Loading overlay for large file background loads.
                if let Some(job) = load_job.as_ref() {
                    use crate::editor::view::DrawContext as _;
                    crate::editor::app_state::clip_init(width, height);
                    // Dim background.
                    draw_ctx.draw_rect(0.0, 0.0, width, height, [0, 0, 0, 160]);
                    // Centered dialog.
                    let dlg_w = 520.0_f64.min(width - 40.0);
                    let dlg_h = style.font_height * 3.5 + style.padding_y * 4.0;
                    let dlg_x = (width - dlg_w) / 2.0;
                    let dlg_y = (height - dlg_h) / 2.0;
                    draw_ctx.draw_rect(
                        dlg_x - 1.0,
                        dlg_y - 1.0,
                        dlg_w + 2.0,
                        dlg_h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(dlg_x, dlg_y, dlg_w, dlg_h, style.background3.to_array());
                    // Title.
                    let title = format!("Loading {}", job.name);
                    draw_ctx.draw_text(
                        style.font,
                        &title,
                        dlg_x + style.padding_x,
                        dlg_y + style.padding_y,
                        style.text.to_array(),
                    );
                    // Progress numbers.
                    let bytes = job.bytes_read.load(std::sync::atomic::Ordering::Relaxed);
                    let pct = if job.total_bytes > 0 {
                        (bytes as f64 / job.total_bytes as f64).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let fmt_mb = |b: u64| format!("{:.1} MB", b as f64 / (1024.0 * 1024.0));
                    let status = format!(
                        "{} / {}  ({:.0}%)",
                        fmt_mb(bytes),
                        fmt_mb(job.total_bytes),
                        pct * 100.0,
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &status,
                        dlg_x + style.padding_x,
                        dlg_y + style.padding_y * 2.0 + style.font_height,
                        style.dim.to_array(),
                    );
                    // Progress bar.
                    let bar_x = dlg_x + style.padding_x;
                    let bar_y = dlg_y + dlg_h - style.padding_y - style.font_height / 2.0;
                    let bar_w = dlg_w - style.padding_x * 2.0;
                    let bar_h = style.font_height / 2.0;
                    draw_ctx.draw_rect(bar_x, bar_y, bar_w, bar_h, style.divider.to_array());
                    draw_ctx.draw_rect(bar_x, bar_y, bar_w * pct, bar_h, style.accent.to_array());
                }

                // Nag bar takes priority over all overlays.
                if let Nag::UnsavedChanges { message, .. } = &nag {
                    cmdview_active = false;
                    palette_active = false;
                    completion.hide();
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    // Semi-transparent overlay dims the entire editor.
                    draw_ctx.draw_rect(0.0, 0.0, width, height, [0, 0, 0, 120]);
                    let bar_h = style.font_height + style.padding_y * 2.0;
                    draw_ctx.draw_rect(0.0, 0.0, width, bar_h, style.nagbar.to_array());
                    draw_ctx.draw_text(
                        style.font,
                        message,
                        style.padding_x,
                        style.padding_y,
                        style.nagbar_text.to_array(),
                    );
                    // Draw option buttons.
                    let msg_w = draw_ctx.font_width(style.font, message);
                    let btn_y = style.padding_y * 0.5;
                    let btn_h = style.font_height + style.padding_y;
                    let btn_pad = style.padding_x;
                    let mut bx = style.padding_x + msg_w + btn_pad * 2.0;
                    for label in &["Yes", "No"] {
                        let lw = draw_ctx.font_width(style.font, label) + btn_pad * 2.0;
                        draw_ctx.draw_rect(bx, btn_y, lw, btn_h, style.nagbar_text.to_array());
                        draw_ctx.draw_text(
                            style.font,
                            label,
                            bx + btn_pad,
                            btn_y + style.padding_y * 0.5,
                            style.nagbar.to_array(),
                        );
                        bx += lw + btn_pad;
                    }
                }

                // Warn once per session per codepoint when a drawn character
                // is covered by no configured or installed system font.
                let uncovered = crate::renderer::take_uncovered();
                if let Some(&cp) = uncovered.first() {
                    let more = match uncovered.len() {
                        1 => String::new(),
                        n => format!(" and {} more", n - 1),
                    };
                    let msg = format!(
                        "No installed font covers U+{cp:04X}{more} -- install a font for this script or set fonts.code.paths in config"
                    );
                    log::warn!("{msg}");
                    info_message = Some((msg, Instant::now()));
                }

                // Draw info message (auto-dismiss after 3s, or on any key).
                if let Some((ref msg, at)) = info_message {
                    if at.elapsed().as_secs() >= 3 {
                        info_message = None;
                    } else {
                        crate::editor::app_state::clip_init(width, height);
                        use crate::editor::view::DrawContext as _;
                        let bar_h = style.font_height + style.padding_y * 2.0;
                        draw_ctx.draw_rect(0.0, 0.0, width, bar_h, style.accent.to_array());
                        let ty = (bar_h - style.font_height) / 2.0;
                        draw_ctx.draw_text(
                            style.font,
                            msg,
                            style.padding_x,
                            ty,
                            [255, 255, 255, 255],
                        );
                    }
                }

                // Draw "create missing directory?" confirmation bar.
                if let Nag::CreateDir { parent, .. } = &nag {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let bar_h = style.font_height + style.padding_y * 2.0;
                    draw_ctx.draw_rect(0.0, 0.0, width, bar_h, style.nagbar.to_array());
                    let msg = format!(
                        "Directory does not exist: {parent}. Create it and save?  [Y]es  [N]o"
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &msg,
                        style.padding_x,
                        style.padding_y,
                        style.nagbar_text.to_array(),
                    );
                }

                // Draw "overwrite existing file?" confirmation bar.
                if let Nag::OverwriteFile { save_path, .. } = &nag {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let bar_h = style.font_height + style.padding_y * 2.0;
                    draw_ctx.draw_rect(0.0, 0.0, width, bar_h, style.nagbar.to_array());
                    let msg = format!("{save_path} already exists. Overwrite?  [Y]es  [N]o");
                    draw_ctx.draw_text(
                        style.font,
                        &msg,
                        style.padding_x,
                        style.padding_y,
                        style.nagbar_text.to_array(),
                    );
                }

                // Draw "no extension detected?" confirmation bar.
                if let Nag::NoExtension { save_path, .. } = &nag {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let bar_h = style.font_height + style.padding_y * 2.0;
                    draw_ctx.draw_rect(0.0, 0.0, width, bar_h, style.nagbar.to_array());
                    let msg =
                        format!("No extension detected ({save_path}). Save anyway?  [Y]es  [N]o");
                    draw_ctx.draw_text(
                        style.font,
                        &msg,
                        style.padding_x,
                        style.padding_y,
                        style.nagbar_text.to_array(),
                    );
                }

                // Draw "delete file?" confirmation bar.
                if let Nag::DeleteFile { path } = &nag {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let bar_h = style.font_height + style.padding_y * 2.0;
                    draw_ctx.draw_rect(0.0, 0.0, width, bar_h, style.nagbar.to_array());
                    let msg = format!("Delete {path}?  [Y]es  [N]o");
                    draw_ctx.draw_text(
                        style.font,
                        &msg,
                        style.padding_x,
                        style.padding_y,
                        style.nagbar_text.to_array(),
                    );
                }

                // Draw reload nag bar if active.
                if let Nag::ReloadFromDisk { path } = &nag {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let bar_h = style.font_height + style.padding_y * 2.0;
                    draw_ctx.draw_rect(0.0, 0.0, width, bar_h, style.nagbar.to_array());
                    let msg = format!("File changed on disk: {path}. Reload?  [Y]es  [N]o");
                    draw_ctx.draw_text(
                        style.font,
                        &msg,
                        style.padding_x,
                        style.padding_y,
                        style.nagbar_text.to_array(),
                    );
                }

                // Draw command palette if active.
                if palette_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let pal_w = (width * 0.5).max(400.0).min(width - 20.0);
                    let pal_x = (width - pal_w) / 2.0;
                    let pal_y = style.padding_y * 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_visible = 12usize;
                    let visible = palette_results.len().min(max_visible);
                    let pal_h = line_h * (visible as f64 + 1.0) + style.padding_y * 2.0;

                    draw_ctx.draw_rect(
                        pal_x - 1.0,
                        pal_y - 1.0,
                        pal_w + 2.0,
                        pal_h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(pal_x, pal_y, pal_w, pal_h, style.background3.to_array());

                    let input_y = pal_y + style.padding_y;
                    draw_ctx.draw_text(
                        style.font,
                        &format!("> {palette_query}_"),
                        pal_x + style.padding_x,
                        input_y,
                        style.text.to_array(),
                    );
                    draw_ctx.draw_rect(
                        pal_x,
                        input_y + line_h,
                        pal_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );

                    // Scroll the visible window so palette_selected stays in view.
                    let scroll_off = if palette_selected >= max_visible {
                        palette_selected - max_visible + 1
                    } else {
                        0
                    };
                    for (i, (_, display)) in palette_results
                        .iter()
                        .enumerate()
                        .skip(scroll_off)
                        .take(max_visible)
                    {
                        let display_idx = i - scroll_off;
                        let ry =
                            input_y + line_h + style.divider_size + display_idx as f64 * line_h;
                        if i == palette_selected {
                            draw_ctx.draw_rect(
                                pal_x,
                                ry,
                                pal_w,
                                line_h,
                                style.selection.to_array(),
                            );
                        }
                        let color = if i == palette_selected {
                            style.accent.to_array()
                        } else {
                            style.text.to_array()
                        };
                        draw_ctx.draw_text(
                            style.font,
                            display,
                            pal_x + style.padding_x,
                            ry + style.padding_y / 2.0,
                            color,
                        );
                    }
                }

                // Draw project search overlay.
                if subsystems.has_find_in_files() && project_search_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let ps_w = (width * 0.6).max(500.0).min(width - 20.0);
                    let ps_x = (width - ps_w) / 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_visible = 15usize;
                    let visible_count = project_search_results.len().min(max_visible);
                    // Title + input + hint + results.
                    let ps_h = line_h * (visible_count as f64 + 3.0) + style.padding_y * 2.0;
                    let ps_y = style.padding_y * 2.0;

                    draw_ctx.draw_rect(
                        ps_x - 1.0,
                        ps_y - 1.0,
                        ps_w + 2.0,
                        ps_h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(ps_x, ps_y, ps_w, ps_h, style.background3.to_array());

                    // Title bar.
                    let title_y = ps_y + style.padding_y;
                    draw_ctx.draw_text(
                        style.font,
                        "Find in Files",
                        ps_x + style.padding_x,
                        title_y,
                        style.accent.to_array(),
                    );
                    let match_count = format!("  ({} matches)", project_search_results.len());
                    let title_w = draw_ctx.font_width(style.font, "Find in Files");
                    draw_ctx.draw_text(
                        style.font,
                        &match_count,
                        ps_x + style.padding_x + title_w,
                        title_y,
                        style.dim.to_array(),
                    );
                    draw_ctx.draw_rect(
                        ps_x,
                        title_y + line_h,
                        ps_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );

                    // Input line.
                    let input_y = title_y + line_h;
                    let label = "Search: ";
                    let label_w = draw_ctx.font_width(style.font, label);
                    draw_ctx.draw_text(
                        style.font,
                        label,
                        ps_x + style.padding_x,
                        input_y,
                        style.accent.to_array(),
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &format!("{}_", &project_search_query),
                        ps_x + style.padding_x + label_w + style.padding_x,
                        input_y,
                        style.text.to_array(),
                    );

                    // Toggle hints.
                    let hint_y = input_y + line_h;
                    draw_ctx.draw_rect(
                        ps_x,
                        hint_y,
                        ps_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    let mark = |on: bool| if on { "[x]" } else { "[ ]" };
                    let hint = format!(
                        "Alt+R Regex {}  Alt+W Word {}  Alt+I Case {}   Enter open  Esc close",
                        mark(project_use_regex),
                        mark(project_whole_word),
                        mark(project_case_insensitive),
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &hint,
                        ps_x + style.padding_x,
                        hint_y + style.padding_y * 0.5,
                        style.dim.to_array(),
                    );

                    // Divider below hints.
                    let results_start_y = hint_y + line_h;
                    draw_ctx.draw_rect(
                        ps_x,
                        results_start_y,
                        ps_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );

                    // Scroll offset so selected item is visible.
                    let scroll_off = if project_search_selected >= max_visible {
                        project_search_selected - max_visible + 1
                    } else {
                        0
                    };

                    // Results list.
                    for (i, (path, line_num, text)) in project_search_results
                        .iter()
                        .enumerate()
                        .skip(scroll_off)
                        .take(max_visible)
                    {
                        let display_idx = i - scroll_off;
                        let ry = results_start_y + style.divider_size + display_idx as f64 * line_h;
                        if i == project_search_selected {
                            draw_ctx.draw_rect(ps_x, ry, ps_w, line_h, style.selection.to_array());
                        }
                        // Show path:line then the matched text.
                        let location = format!("{path}:{line_num}");
                        let loc_color = if i == project_search_selected {
                            style.accent.to_array()
                        } else {
                            style.dim.to_array()
                        };
                        draw_ctx.draw_text(
                            style.font,
                            &location,
                            ps_x + style.padding_x,
                            ry + style.padding_y / 2.0,
                            loc_color,
                        );
                        let loc_w = draw_ctx.font_width(style.font, &location);
                        let text_color = style.text.to_array();
                        let max_text_w = ps_w - style.padding_x * 3.0 - loc_w;
                        let truncated: String = if max_text_w > 0.0 {
                            let char_w = draw_ctx.font_width(style.font, "m");
                            let max_chars = (max_text_w / char_w).floor() as usize;
                            text.chars().take(max_chars).collect()
                        } else {
                            String::new()
                        };
                        draw_ctx.draw_text(
                            style.font,
                            &format!("  {truncated}"),
                            ps_x + style.padding_x + loc_w,
                            ry + style.padding_y / 2.0,
                            text_color,
                        );
                    }
                }

                // Draw project replace overlay.
                if subsystems.has_find_in_files() && project_replace_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let pr_w = (width * 0.6).max(500.0).min(width - 20.0);
                    let pr_x = (width - pr_w) / 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_visible = 12usize;
                    let visible_count = project_replace_results.len().min(max_visible);
                    // Title + search + replace + toggles + hint + results.
                    let pr_h = line_h * (visible_count as f64 + 5.0) + style.padding_y * 2.0;
                    let pr_y = style.padding_y * 2.0;

                    draw_ctx.draw_rect(
                        pr_x - 1.0,
                        pr_y - 1.0,
                        pr_w + 2.0,
                        pr_h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(pr_x, pr_y, pr_w, pr_h, style.background3.to_array());

                    // Title bar.
                    let title_y = pr_y + style.padding_y;
                    draw_ctx.draw_text(
                        style.font,
                        "Replace in Files",
                        pr_x + style.padding_x,
                        title_y,
                        style.accent.to_array(),
                    );
                    let match_label = format!("  ({} matches)", project_replace_results.len());
                    let tw = draw_ctx.font_width(style.font, "Replace in Files");
                    draw_ctx.draw_text(
                        style.font,
                        &match_label,
                        pr_x + style.padding_x + tw,
                        title_y,
                        style.dim.to_array(),
                    );
                    draw_ctx.draw_rect(
                        pr_x,
                        title_y + line_h,
                        pr_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );

                    // Search input.
                    let row1_y = title_y + line_h;
                    let search_cursor = if !project_replace_focus_on_replace {
                        "_"
                    } else {
                        ""
                    };
                    let search_label = "Search: ";
                    let sl_w = draw_ctx.font_width(style.font, search_label);
                    draw_ctx.draw_text(
                        style.font,
                        search_label,
                        pr_x + style.padding_x,
                        row1_y,
                        style.accent.to_array(),
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &format!("{project_replace_search}{search_cursor}"),
                        pr_x + style.padding_x + sl_w + style.padding_x,
                        row1_y,
                        style.text.to_array(),
                    );

                    // Replace input.
                    let row2_y = row1_y + line_h;
                    draw_ctx.draw_rect(
                        pr_x,
                        row2_y,
                        pr_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    let replace_cursor = if project_replace_focus_on_replace {
                        "_"
                    } else {
                        ""
                    };
                    let rl = "Replace: ";
                    let rl_w = draw_ctx.font_width(style.font, rl);
                    draw_ctx.draw_text(
                        style.font,
                        rl,
                        pr_x + style.padding_x,
                        row2_y,
                        style.accent.to_array(),
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &format!("{project_replace_with}{replace_cursor}"),
                        pr_x + style.padding_x + rl_w + style.padding_x,
                        row2_y,
                        style.text.to_array(),
                    );

                    // Toggle hints.
                    let toggles_y = row2_y + line_h;
                    draw_ctx.draw_rect(
                        pr_x,
                        toggles_y,
                        pr_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    let mark = |on: bool| if on { "[x]" } else { "[ ]" };
                    let toggle_hint = format!(
                        "Alt+R Regex {}  Alt+W Word {}  Alt+I Case {}",
                        mark(project_use_regex),
                        mark(project_whole_word),
                        mark(project_case_insensitive),
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &toggle_hint,
                        pr_x + style.padding_x,
                        toggles_y + style.padding_y * 0.5,
                        style.dim.to_array(),
                    );

                    // Action hint row.
                    let hint_y = toggles_y + line_h;
                    draw_ctx.draw_rect(
                        pr_x,
                        hint_y,
                        pr_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    let hint =
                        "Tab switch fields  Enter preview  Ctrl+Enter replace all  Esc close";
                    draw_ctx.draw_text(
                        style.font,
                        hint,
                        pr_x + style.padding_x,
                        hint_y + style.padding_y * 0.5,
                        style.dim.to_array(),
                    );

                    // Results preview.
                    let results_y = hint_y + line_h;
                    draw_ctx.draw_rect(
                        pr_x,
                        results_y,
                        pr_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(
                        pr_x,
                        results_y,
                        pr_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    let scroll_off = if project_replace_selected >= max_visible {
                        project_replace_selected - max_visible + 1
                    } else {
                        0
                    };
                    for (i, (path, line_num, text)) in project_replace_results
                        .iter()
                        .enumerate()
                        .skip(scroll_off)
                        .take(max_visible)
                    {
                        let di = i - scroll_off;
                        let ry = results_y + style.divider_size + di as f64 * line_h;
                        if i == project_replace_selected {
                            draw_ctx.draw_rect(pr_x, ry, pr_w, line_h, style.selection.to_array());
                        }
                        let location = format!("{path}:{line_num}");
                        let loc_color = if i == project_replace_selected {
                            style.accent.to_array()
                        } else {
                            style.dim.to_array()
                        };
                        draw_ctx.draw_text(
                            style.font,
                            &location,
                            pr_x + style.padding_x,
                            ry + style.padding_y / 2.0,
                            loc_color,
                        );
                        let loc_w = draw_ctx.font_width(style.font, &location);
                        let max_text_w = pr_w - style.padding_x * 3.0 - loc_w;
                        if max_text_w > 0.0 {
                            let char_w = draw_ctx.font_width(style.font, "m");
                            let max_chars = (max_text_w / char_w).floor() as usize;
                            let truncated: String = text.chars().take(max_chars).collect();
                            draw_ctx.draw_text(
                                style.font,
                                &format!("  {truncated}"),
                                pr_x + style.padding_x + loc_w,
                                ry + style.padding_y / 2.0,
                                style.text.to_array(),
                            );
                        }
                    }
                }

                // Draw git status overlay.
                if subsystems.has_git() && git_status_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let gs_w = (width * 0.5).max(400.0).min(width - 20.0);
                    let gs_x = (width - gs_w) / 2.0;
                    let gs_y = style.padding_y * 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_vis = 20usize;
                    let vis = git_status_entries.len().min(max_vis);
                    let gs_h = line_h * (vis as f64 + 1.0) + style.padding_y * 2.0;
                    draw_ctx.draw_rect(
                        gs_x - 1.0,
                        gs_y - 1.0,
                        gs_w + 2.0,
                        gs_h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(gs_x, gs_y, gs_w, gs_h, style.background3.to_array());
                    let input_y = gs_y + style.padding_y;
                    let title = format!(
                        "Git Status  ({} changed)  [R] refresh  [Enter] open  [Esc] close",
                        git_status_entries.len()
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &title,
                        gs_x + style.padding_x,
                        input_y,
                        style.accent.to_array(),
                    );
                    draw_ctx.draw_rect(
                        gs_x,
                        input_y + line_h,
                        gs_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    let scroll_off = if git_status_selected >= max_vis {
                        git_status_selected - max_vis + 1
                    } else {
                        0
                    };
                    for (i, (code, _path, display)) in git_status_entries
                        .iter()
                        .enumerate()
                        .skip(scroll_off)
                        .take(max_vis)
                    {
                        let di = i - scroll_off;
                        let ry = input_y + line_h + style.divider_size + di as f64 * line_h;
                        if i == git_status_selected {
                            draw_ctx.draw_rect(gs_x, ry, gs_w, line_h, style.selection.to_array());
                        }
                        let color = match code.as_str() {
                            "M" | "MM" => style.warn.to_array(),
                            "A" | "AM" => style.good.to_array(),
                            "D" => style.error.to_array(),
                            "?" | "??" => style.dim.to_array(),
                            _ => style.text.to_array(),
                        };
                        draw_ctx.draw_text(
                            style.font,
                            display,
                            gs_x + style.padding_x,
                            ry + style.padding_y / 2.0,
                            color,
                        );
                    }
                }

                // Draw git log overlay.
                if code_action_active && !code_actions.is_empty() {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let ca_w = (width * 0.5).max(400.0).min(width - 20.0);
                    let ca_x = (width - ca_w) / 2.0;
                    let ca_y = style.padding_y * 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_vis = 15usize;
                    let vis = code_actions.len().min(max_vis);
                    let ca_h = line_h * (vis as f64 + 1.0) + style.padding_y * 2.0;
                    draw_ctx.draw_rect(
                        ca_x - 1.0,
                        ca_y - 1.0,
                        ca_w + 2.0,
                        ca_h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(ca_x, ca_y, ca_w, ca_h, style.background3.to_array());
                    let input_y = ca_y + style.padding_y;
                    let title = format!(
                        "Code Actions  ({})  [Enter] apply  [Esc] close",
                        code_actions.len()
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &title,
                        ca_x + style.padding_x,
                        input_y,
                        style.accent.to_array(),
                    );
                    draw_ctx.draw_rect(
                        ca_x,
                        input_y + line_h,
                        ca_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    let scroll_off = if code_action_selected >= max_vis {
                        code_action_selected - max_vis + 1
                    } else {
                        0
                    };
                    for (i, (action_title, action)) in code_actions
                        .iter()
                        .enumerate()
                        .skip(scroll_off)
                        .take(max_vis)
                    {
                        let di = i - scroll_off;
                        let ry = input_y + line_h + style.divider_size + di as f64 * line_h;
                        if i == code_action_selected {
                            draw_ctx.draw_rect(ca_x, ry, ca_w, line_h, style.selection.to_array());
                        }
                        let color = if code_action_disabled_reason(action).is_some() {
                            style.dim.to_array()
                        } else if i == code_action_selected {
                            style.accent.to_array()
                        } else {
                            style.text.to_array()
                        };
                        draw_ctx.draw_text(
                            style.font,
                            action_title,
                            ca_x + style.padding_x,
                            ry + style.padding_y / 2.0,
                            color,
                        );
                    }
                }

                if subsystems.has_git() && git_log_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let gl_w = (width * 0.6).max(500.0).min(width - 20.0);
                    let gl_x = (width - gl_w) / 2.0;
                    let gl_y = style.padding_y * 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_vis = 20usize;
                    let vis = git_log_entries.len().min(max_vis);
                    let gl_h = line_h * (vis as f64 + 1.0) + style.padding_y * 2.0;
                    draw_ctx.draw_rect(
                        gl_x - 1.0,
                        gl_y - 1.0,
                        gl_w + 2.0,
                        gl_h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(gl_x, gl_y, gl_w, gl_h, style.background3.to_array());
                    let input_y = gl_y + style.padding_y;
                    let title =
                        format!("Git Log  ({} commits)  [Esc] close", git_log_entries.len());
                    draw_ctx.draw_text(
                        style.font,
                        &title,
                        gl_x + style.padding_x,
                        input_y,
                        style.accent.to_array(),
                    );
                    draw_ctx.draw_rect(
                        gl_x,
                        input_y + line_h,
                        gl_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    let scroll_off = if git_log_selected >= max_vis {
                        git_log_selected - max_vis + 1
                    } else {
                        0
                    };
                    for (i, (hash, date, msg)) in git_log_entries
                        .iter()
                        .enumerate()
                        .skip(scroll_off)
                        .take(max_vis)
                    {
                        let di = i - scroll_off;
                        let ry = input_y + line_h + style.divider_size + di as f64 * line_h;
                        if i == git_log_selected {
                            draw_ctx.draw_rect(gl_x, ry, gl_w, line_h, style.selection.to_array());
                        }
                        let entry_text = format!("{hash}  {date}  {msg}");
                        let hash_color = if i == git_log_selected {
                            style.accent.to_array()
                        } else {
                            style.dim.to_array()
                        };
                        draw_ctx.draw_text(
                            style.font,
                            &entry_text,
                            gl_x + style.padding_x,
                            ry + style.padding_y / 2.0,
                            hash_color,
                        );
                    }
                }

                // Draw command view (file/folder open with autocomplete) at top.
                if cmdview_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    // Widen the picker to 70% of the window so common paths
                    // fit without scrolling. The input still hard-scrolls
                    // horizontally for anything longer, and the suggestions
                    // list ellipsis-truncates on the LEFT so the filename
                    // (the interesting part of a long path) stays visible.
                    let cv_w = (width * 0.7).max(500.0).min(width - 20.0);
                    let cv_x = (width - cv_w) / 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_visible = 15usize;
                    let visible_count = cmdview_suggestions.len().min(max_visible);
                    let cv_h = line_h * (visible_count as f64 + 1.0) + style.padding_y * 2.0;
                    // When a nag is active, push the cmdview down so the
                    // nag bar stays visible at the top and its key focus
                    // isn't hidden behind the picker.
                    let nag_offset = if matches!(
                        nag,
                        Nag::OverwriteFile { .. }
                            | Nag::CreateDir { .. }
                            | Nag::ReloadFromDisk { .. }
                            | Nag::NoExtension { .. }
                    ) {
                        style.font_height + style.padding_y * 2.0 + style.padding_y
                    } else {
                        0.0
                    };
                    let cv_y = style.padding_y * 2.0 + nag_offset;

                    // Border + background.
                    draw_ctx.draw_rect(
                        cv_x - 1.0,
                        cv_y - 1.0,
                        cv_w + 2.0,
                        cv_h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(cv_x, cv_y, cv_w, cv_h, style.background3.to_array());

                    // Input line.
                    let input_y = cv_y + style.padding_y;
                    let label = &cmdview_label;
                    let label_w = draw_ctx.font_width(style.font, label);
                    draw_ctx.draw_text(
                        style.font,
                        label,
                        cv_x + style.padding_x,
                        input_y,
                        style.accent.to_array(),
                    );

                    // Horizontal-scrolling input. `text_origin` is where the
                    // first character of the input would land if scroll == 0;
                    // we shift text left (via `text_scroll`) so the caret is
                    // always a few chars inside the visible area even for
                    // long paths. A tiny "<" / ">" indicator marks the edge
                    // when content exists past it so the user can tell
                    // they're scrolled.
                    let text_area_x = cv_x + style.padding_x + label_w + style.padding_x;
                    let text_area_right = cv_x + cv_w - style.padding_x;
                    let text_area_w = (text_area_right - text_area_x).max(0.0);
                    let cursor_safe = cmdview_cursor.min(cmdview_text.len());
                    let before_cursor = &cmdview_text[..cursor_safe];
                    let caret_offset_px = draw_ctx.font_width(style.font, before_cursor);
                    let full_text_w = draw_ctx.font_width(style.font, &cmdview_text);
                    let caret_margin = (style.font_height * 0.5).min(text_area_w * 0.25);
                    let mut text_scroll = if full_text_w <= text_area_w {
                        0.0
                    } else if caret_offset_px > text_area_w - caret_margin {
                        caret_offset_px - (text_area_w - caret_margin)
                    } else {
                        0.0
                    };
                    // Guarantee we don't scroll so far that we reveal blank
                    // space past the end of the text.
                    let max_scroll = (full_text_w - text_area_w).max(0.0);
                    if text_scroll > max_scroll {
                        text_scroll = max_scroll;
                    }
                    let text_origin = text_area_x - text_scroll;

                    // Clip text to the input area so long paths can't bleed
                    // over the label, the box border, or the scrollbar.
                    draw_ctx.set_clip_rect(text_area_x, input_y, text_area_w, style.font_height);
                    draw_ctx.draw_text(
                        style.font,
                        &cmdview_text,
                        text_origin,
                        input_y,
                        style.text.to_array(),
                    );
                    let caret_x = text_origin + caret_offset_px;
                    draw_ctx.draw_rect(
                        caret_x,
                        input_y,
                        style.caret_width,
                        style.font_height,
                        style.caret.to_array(),
                    );
                    draw_ctx.set_clip_rect(0.0, 0.0, width, height);
                    if text_scroll > 0.5 {
                        draw_ctx.draw_text(
                            style.font,
                            "<",
                            text_area_x - draw_ctx.font_width(style.font, "<"),
                            input_y,
                            style.dim.to_array(),
                        );
                    }
                    if full_text_w - text_scroll > text_area_w + 0.5 {
                        draw_ctx.draw_text(
                            style.font,
                            ">",
                            text_area_right,
                            input_y,
                            style.dim.to_array(),
                        );
                    }

                    // Divider below input.
                    draw_ctx.draw_rect(
                        cv_x,
                        input_y + line_h,
                        cv_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );

                    // Scroll offset so selected item is visible.
                    let scroll_off = if cmdview_selected >= max_visible {
                        cmdview_selected - max_visible + 1
                    } else {
                        0
                    };

                    // Suggestions list. Long paths get ellipsis-truncated on
                    // the LEFT so the filename stays visible — that's
                    // typically what the user is trying to pick.
                    let suggestion_area_x = cv_x + style.padding_x;
                    let suggestion_area_w = (cv_w - style.padding_x * 2.0).max(0.0);
                    for (i, suggestion) in cmdview_suggestions
                        .iter()
                        .enumerate()
                        .skip(scroll_off)
                        .take(max_visible)
                    {
                        let display_idx = i - scroll_off;
                        let ry =
                            input_y + line_h + style.divider_size + display_idx as f64 * line_h;
                        if i == cmdview_selected {
                            draw_ctx.draw_rect(cv_x, ry, cv_w, line_h, style.selection.to_array());
                        }
                        let is_dir = suggestion.ends_with('/') || suggestion.ends_with('\\');
                        let color = if i == cmdview_selected || is_dir {
                            style.accent.to_array()
                        } else {
                            style.text.to_array()
                        };
                        let display_text = truncate_left_to_width(
                            suggestion,
                            suggestion_area_w,
                            style.font,
                            &mut draw_ctx,
                        );
                        draw_ctx.draw_text(
                            style.font,
                            &display_text,
                            suggestion_area_x,
                            ry + style.padding_y / 2.0,
                            color,
                        );
                    }
                }

                // Draw LSP completion popup.
                if subsystems.has_lsp() && completion.visible && !completion.items.is_empty() {
                    if let Some(doc) = docs.get(active_tab) {
                        let dv = &doc.view;
                        crate::editor::app_state::clip_init(width, height);
                        use crate::editor::view::DrawContext as _;
                        let line_h_comp = style.code_font_height * 1.2;
                        let gutter_w = dv.gutter_width;
                        let popup_x = dv.rect().x
                            + gutter_w
                            + style.padding_x
                            + (completion.col as f64 - 1.0)
                                * draw_ctx.font_width(style.code_font, "m")
                            - dv.scroll_x;
                        let popup_y =
                            dv.rect().y + completion.line as f64 * line_h_comp - dv.scroll_y;
                        let item_h = style.font_height + style.padding_y;
                        let popup_h = item_h * completion.items.len() as f64 + style.padding_y;
                        // Auto-size the popup to the widest item rather
                        // than a fixed 350px box. Width = max(label +
                        // " " + detail) over visible items, plus padding.
                        // Clamped to the screen edge and to a sensible
                        // minimum so a one-character item doesn't render
                        // as a sliver.
                        let content_w = completion
                            .items
                            .iter()
                            .map(|(label, detail, _)| {
                                let lw = draw_ctx.font_width(style.font, label);
                                if detail.is_empty() {
                                    lw
                                } else {
                                    lw + draw_ctx.font_width(style.font, detail) + style.padding_x
                                }
                            })
                            .fold(0.0_f64, f64::max);
                        let popup_w = (content_w + style.padding_x * 2.0)
                            .max(120.0)
                            .min(width - popup_x - 10.0);
                        // Background.
                        draw_ctx.draw_rect(
                            popup_x,
                            popup_y,
                            popup_w,
                            popup_h,
                            style.background3.to_array(),
                        );
                        // Border.
                        draw_ctx.draw_rect(
                            popup_x,
                            popup_y,
                            popup_w,
                            style.divider_size,
                            style.divider.to_array(),
                        );
                        for (i, (label, detail, _)) in completion.items.iter().enumerate() {
                            let iy = popup_y + style.padding_y / 2.0 + i as f64 * item_h;
                            if i == completion.selected {
                                draw_ctx.draw_rect(
                                    popup_x,
                                    iy,
                                    popup_w,
                                    item_h,
                                    style.selection.to_array(),
                                );
                            }
                            let fg = if i == completion.selected {
                                style.accent.to_array()
                            } else {
                                style.text.to_array()
                            };
                            draw_ctx.draw_text(
                                style.font,
                                label,
                                popup_x + style.padding_x,
                                iy + style.padding_y / 2.0,
                                fg,
                            );
                            if !detail.is_empty() {
                                let label_w = draw_ctx.font_width(style.font, label);
                                draw_ctx.draw_text(
                                    style.font,
                                    detail,
                                    popup_x + style.padding_x + label_w + style.padding_x,
                                    iy + style.padding_y / 2.0,
                                    style.dim.to_array(),
                                );
                            }
                        }
                    }
                }

                // Draw LSP hover tooltip.
                if subsystems.has_lsp()
                    && signature_help.visible
                    && !signature_help.text.is_empty()
                    && let Some(doc) = docs.get(active_tab)
                {
                    let dv = &doc.view;
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let line_h_sig = style.code_font_height * 1.2;
                    let gutter_w = dv.gutter_width;
                    let sig_x = dv.rect().x
                        + gutter_w
                        + style.padding_x
                        + (signature_help.col as f64 - 1.0)
                            * draw_ctx.font_width(style.code_font, "m")
                        - dv.scroll_x;
                    // Below the current line so the popup does not cover the call.
                    let sig_y = dv.rect().y + signature_help.line as f64 * line_h_sig - dv.scroll_y
                        + style.padding_y / 2.0;
                    let text: String = signature_help
                        .text
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect();
                    let w = draw_ctx.font_width(style.font, &text) + style.padding_x * 2.0;
                    let h = style.font_height + style.padding_y * 2.0;
                    draw_ctx.draw_rect(
                        sig_x - 1.0,
                        sig_y - 1.0,
                        w + 2.0,
                        h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(sig_x, sig_y, w, h, style.background3.to_array());
                    draw_ctx.draw_text(
                        style.font,
                        &text,
                        sig_x + style.padding_x,
                        sig_y + style.padding_y,
                        style.accent.to_array(),
                    );
                }

                if subsystems.has_lsp() && hover.visible && !hover.text.is_empty() {
                    if let Some(doc) = docs.get(active_tab) {
                        let dv = &doc.view;
                        crate::editor::app_state::clip_init(width, height);
                        use crate::editor::view::DrawContext as _;
                        let line_h_hover = style.code_font_height * 1.2;
                        let gutter_w = dv.gutter_width;
                        let hover_x = dv.rect().x
                            + gutter_w
                            + style.padding_x
                            + (hover.col as f64 - 1.0) * draw_ctx.font_width(style.code_font, "m")
                            - dv.scroll_x;
                        let hover_y = dv.rect().y + (hover.line as f64 - 1.0) * line_h_hover
                            - dv.scroll_y
                            - style.padding_y;
                        // Wrap text to lines for display.
                        let max_chars = 80;
                        let hover_lines: Vec<&str> = hover
                            .text
                            .lines()
                            .flat_map(|l| {
                                if l.len() <= max_chars {
                                    vec![l]
                                } else {
                                    l.as_bytes()
                                        .chunks(max_chars)
                                        .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
                                        .collect()
                                }
                            })
                            .take(15)
                            .collect();
                        let line_count_h = hover_lines.len();
                        let tooltip_line_h = style.font_height + 2.0;
                        let message_h =
                            tooltip_line_h * line_count_h as f64 + style.padding_y * 2.0;
                        let is_diagnostic_hover = hovered_diagnostic.is_some();
                        let has_quick_fixes = !diagnostic_hover_actions.is_empty();
                        let action_h = if has_quick_fixes {
                            style.font_height + style.padding_y * 2.0
                        } else {
                            0.0
                        };
                        let quick_fix_w =
                            draw_ctx.font_width(style.font, "Quick Fix") + style.padding_x * 2.0;
                        let actions_w = quick_fix_w + style.padding_x * 2.0;
                        let tooltip_h = message_h + action_h;
                        let tooltip_w = (hover_lines
                            .iter()
                            .map(|l| draw_ctx.font_width(style.font, l))
                            .fold(0.0_f64, f64::max)
                            + style.padding_x * 2.0)
                            .max(if has_quick_fixes { actions_w } else { 0.0 });
                        let tooltip_x = hover_x
                            .max(dv.rect().x)
                            .min((width - tooltip_w - style.padding_x).max(dv.rect().x));
                        let tooltip_y = (hover_y - tooltip_h).max(style.padding_y);
                        // Background.
                        draw_ctx.draw_rect(
                            tooltip_x,
                            tooltip_y,
                            tooltip_w,
                            tooltip_h,
                            style.background3.to_array(),
                        );
                        draw_ctx.draw_rect(
                            tooltip_x,
                            tooltip_y,
                            tooltip_w,
                            style.divider_size,
                            style.divider.to_array(),
                        );
                        for (i, line_text) in hover_lines.iter().enumerate() {
                            draw_ctx.draw_text(
                                style.font,
                                line_text,
                                tooltip_x + style.padding_x,
                                tooltip_y + style.padding_y + i as f64 * tooltip_line_h,
                                style.text.to_array(),
                            );
                        }
                        if has_quick_fixes {
                            let action_y = tooltip_y + message_h;
                            draw_ctx.draw_rect(
                                tooltip_x,
                                action_y,
                                tooltip_w,
                                style.divider_size,
                                style.divider.to_array(),
                            );
                            let quick_rect = crate::editor::types::Rect {
                                x: tooltip_x + style.padding_x,
                                y: action_y + style.padding_y / 2.0,
                                w: quick_fix_w,
                                h: action_h - style.padding_y,
                            };
                            draw_ctx.draw_text(
                                style.font,
                                "Quick Fix",
                                quick_rect.x + style.padding_x,
                                quick_rect.y + style.padding_y / 2.0,
                                style.accent.to_array(),
                            );
                            diagnostic_quick_fix_rect = Some(quick_rect);
                        } else {
                            diagnostic_quick_fix_rect = None;
                        }
                        if is_diagnostic_hover {
                            let popup_rect = crate::editor::types::Rect {
                                x: tooltip_x,
                                y: tooltip_y,
                                w: tooltip_w,
                                h: tooltip_h,
                            };
                            let anchor_rect = crate::editor::types::Rect {
                                x: hover_x,
                                y: hover_y + style.padding_y,
                                w: draw_ctx.font_width(style.code_font, "m").max(1.0),
                                h: line_h_hover,
                            };
                            diagnostic_hover_region_rect = Some(diagnostic_hover_intent_rect(
                                popup_rect,
                                anchor_rect,
                                style.padding_x,
                            ));
                        } else {
                            diagnostic_hover_region_rect = None;
                            diagnostic_hover_leave_at = None;
                            diagnostic_quick_fix_rect = None;
                            diagnostic_hover_action_request = None;
                            diagnostic_hover_actions.clear();
                        }
                    }
                }

                // Tab-bar overlays (hover tooltip + overflow dropdown list)
                // render here so the breadcrumb / sidebar / doc view don't
                // paint over them. The tab bar draw pass captured `tab_hover`,
                // `tab_overlay_*`, and the per-tab rects; this pass consumes
                // them without recomputing widths.
                if tab_overlay_tbh > 0.0 {
                    use crate::editor::view::DrawContext as _;
                    crate::editor::app_state::clip_init(width, height);
                    let tbh = tab_overlay_tbh;

                    // Tooltip for a hovered (truncated) tab.
                    if let Some(hi) = tab_hover {
                        if tab_overlay_overflow && !tab_tooltip_suppressed {
                            if let (Some(doc), Some((tx_h, tw_h, _, full_label))) =
                                (docs.get(hi), tab_overlay_rects.get(hi))
                            {
                                let path = doc.path.clone();
                                let tip_font = style.font;
                                let name_w = draw_ctx.font_width(tip_font, full_label);
                                let max_tip_w =
                                    (width - sidebar_w - style.padding_x * 2.0).max(80.0);
                                let path_full_w = draw_ctx.font_width(tip_font, &path);
                                let (path_display, path_w) =
                                    if path_full_w + style.padding_x * 2.0 <= max_tip_w {
                                        (path.clone(), path_full_w)
                                    } else {
                                        // Front-ellipsize: keep the rightmost (most
                                        // specific) part of the path.
                                        let ell = "...";
                                        let ell_w = draw_ctx.font_width(tip_font, ell);
                                        let mut trimmed: String = path.clone();
                                        while trimmed.chars().count() > 1
                                            && ell_w
                                                + draw_ctx.font_width(tip_font, &trimmed)
                                                + style.padding_x * 2.0
                                                > max_tip_w
                                        {
                                            let mut ch = trimmed.chars();
                                            ch.next();
                                            trimmed = ch.as_str().to_string();
                                        }
                                        let out = format!("{ell}{trimmed}");
                                        let w = draw_ctx.font_width(tip_font, &out);
                                        (out, w)
                                    };
                                let tip_w = name_w.max(path_w) + style.padding_x * 2.0;
                                let tip_h = style.font_height * 2.0 + style.padding_y * 1.5;
                                let tip_x = (tx_h + tw_h / 2.0 - tip_w / 2.0)
                                    .max(sidebar_w)
                                    .min((width - tip_w).max(sidebar_w));
                                let tip_y = tbh + 2.0;
                                draw_ctx.draw_rect(
                                    tip_x - 1.0,
                                    tip_y - 1.0,
                                    tip_w + 2.0,
                                    tip_h + 2.0,
                                    style.divider.to_array(),
                                );
                                draw_ctx.draw_rect(
                                    tip_x,
                                    tip_y,
                                    tip_w,
                                    tip_h,
                                    style.background.to_array(),
                                );
                                draw_ctx.draw_text(
                                    tip_font,
                                    full_label,
                                    tip_x + style.padding_x,
                                    tip_y + style.padding_y * 0.5,
                                    style.text.to_array(),
                                );
                                draw_ctx.draw_text(
                                    tip_font,
                                    &path_display,
                                    tip_x + style.padding_x,
                                    tip_y + style.padding_y * 0.5 + style.font_height,
                                    style.dim.to_array(),
                                );
                            }
                        }
                    }

                    // Overflow dropdown list: right edge pinned to the dropdown
                    // button's right edge (= window right), extends leftward.
                    if tab_dropdown_open && tab_overlay_overflow {
                        let item_h = style.font_height + style.padding_y;
                        let mut list_w = 0.0_f64;
                        for doc in docs.iter() {
                            let label = if doc_is_modified(doc) {
                                format!("*{}", doc.name)
                            } else {
                                doc.name.clone()
                            };
                            list_w = list_w.max(
                                draw_ctx.font_width(style.font, &label) + style.padding_x * 3.0,
                            );
                        }
                        let avail_list_w = (width - sidebar_w - 4.0).max(40.0);
                        list_w = list_w.max(120.0).min(avail_list_w);
                        let btn_right = tab_overlay_btn_right;
                        let mut list_x = btn_right - list_w;
                        if list_x < sidebar_w + 2.0 {
                            list_x = sidebar_w + 2.0;
                        }
                        let max_list_h = (height - tbh - 4.0).max(item_h);
                        let raw_list_h = item_h * docs.len() as f64 + style.padding_y;
                        let list_h = raw_list_h.min(max_list_h);
                        let list_y = tbh;
                        draw_ctx.draw_rect(
                            list_x - 1.0,
                            list_y - 1.0,
                            list_w + 2.0,
                            list_h + 2.0,
                            style.divider.to_array(),
                        );
                        draw_ctx.draw_rect(
                            list_x,
                            list_y,
                            list_w,
                            list_h,
                            style.background.to_array(),
                        );
                        let mut iy = list_y + style.padding_y / 2.0;
                        for (i, doc) in docs.iter().enumerate() {
                            let label = if doc_is_modified(doc) {
                                format!("*{}", doc.name)
                            } else {
                                doc.name.clone()
                            };
                            let row_hover = mouse_x >= list_x
                                && mouse_x < list_x + list_w
                                && mouse_y >= iy
                                && mouse_y < iy + item_h;
                            if i == active_tab {
                                draw_ctx.draw_rect(
                                    list_x,
                                    iy,
                                    list_w,
                                    item_h,
                                    style.line_highlight.to_array(),
                                );
                            } else if row_hover {
                                draw_ctx.draw_rect(
                                    list_x,
                                    iy,
                                    list_w,
                                    item_h,
                                    style.selection.to_array(),
                                );
                            }
                            let color = if i == active_tab {
                                style.accent.to_array()
                            } else {
                                style.text.to_array()
                            };
                            draw_ctx.draw_text(
                                style.font,
                                &label,
                                list_x + style.padding_x,
                                iy + (item_h - style.font_height) / 2.0,
                                color,
                            );
                            iy += item_h;
                        }
                    }
                }
                let _ = tab_overlay_btn_w; // reserved for future hit-test overlays

                // Draw context menu on top of everything.
                if context_menu.visible {
                    crate::editor::app_state::clip_init(width, height);
                    context_menu.draw_native(&mut draw_ctx, &style);
                }

                crate::renderer::native_end_frame();

                redraw = syntax_pending_after_frame;
                last_draw = Instant::now();
            }
        }

        if quit {
            break;
        }

        // Block until the next event. While there is recent on-screen motion, a
        // terminal panel open (its PTY is polled here), or a background job
        // running, poll at the frame interval so output and animation stay
        // smooth. Otherwise sleep longer: input and worker-pushed wake-up
        // events return from the blocked wait immediately, so only idle CPU is
        // affected, never responsiveness.
        // Periodic memory telemetry, off unless ANVIL_PERF is set.
        if crate::editor::perf::enabled()
            && last_memory_report.elapsed().as_secs() >= MEMORY_REPORT_INTERVAL_SECS
        {
            crate::editor::perf::report_memory(&docs);
            last_memory_report = Instant::now();
        }

        let busy = last_draw.elapsed().as_secs_f64() < 0.3
            || terminal.visible
            || load_job.is_some()
            || replace_job.is_some()
            || git_status_job.is_some()
            || git_blame_job.is_some()
            || git_log_job.is_some()
            || update_check_job.is_some()
            || !lsp_state.pending_did_open.is_empty();
        let timeout = if busy { frame_interval } else { idle_wait };
        crate::window::wait_event(Some(timeout));
    }

    // Persist recent files: add all currently open docs to recent_files.
    for doc in &docs {
        if !doc.path.is_empty() {
            update_recent(&mut recent_files, &doc.path, 100);
        }
    }
    let _ = crate::editor::storage::save_text(
        userdir_path,
        "session",
        "recent_files",
        &serde_json::to_string(&recent_files).unwrap_or_default(),
    );
    let _ = crate::editor::storage::save_text(
        userdir_path,
        "session",
        "recent_projects",
        &serde_json::to_string(&recent_projects).unwrap_or_default(),
    );

    // Persist expanded sidebar folders for this project.
    if subsystems.has_sidebar() {
        save_expanded_folders(
            &sidebar_entries,
            userdir_path,
            &project_session_key(&project_root),
        );
    }

    // Notes-mode: persist the per-folder session so the next launch
    // reopens the same note. The global "session/files" path below is
    // not used by note-anvil because notes-mode never keeps multiple
    // tabs and a per-folder key keeps switching `NOTE_ANVIL_DIR` clean.
    if subsystems.has_notes_mode() && !project_root.is_empty() {
        save_project_session(userdir_path, &project_root, &docs, active_tab);
    }

    // Session save: persist open files, active tab, and project root via storage.
    // Save session state (Lite-Anvil only -- Nano-Anvil has no session).
    if !single_file_mode {
        let mut open_files = Vec::new();
        let mut unsaved_content = Vec::new();
        for doc in &docs {
            if doc.path.is_empty() {
                open_files.push("__untitled__".to_string());
                let content = doc
                    .view
                    .buffer_id
                    .and_then(|id| buffer::with_buffer(id, |b| Ok(b.lines.join(""))).ok())
                    .unwrap_or_default();
                unsaved_content.push(content);
            } else {
                open_files.push(doc.path.clone());
                unsaved_content.push(String::new());
            }
        }
        let project_root_meaningful = !project_root.is_empty()
            && project_root != "."
            && std::path::Path::new(&project_root).is_dir();
        if !open_files.is_empty() || project_root_meaningful {
            let session = crate::editor::open_doc::SessionData {
                files: open_files,
                active: active_tab,
                active_project: project_root.clone(),
                unsaved_content,
            };
            if let Ok(json) = serde_json::to_string_pretty(&session) {
                if let Err(e) = storage::save_text(userdir_path, "session", "files", &json) {
                    eprintln!("Failed to save session: {e}");
                }
            }
        } else if let Err(e) = storage::clear(userdir_path, "session", Some("files")) {
            eprintln!("Failed to clear session: {e}");
        }
    }

    // Save window size and position.
    let (pw, ph, wx, wy) = crate::window::get_window_size();
    let win_json = serde_json::json!({ "w": pw, "h": ph, "x": wx, "y": wy });
    if let Err(e) = storage::save_text(userdir_path, "session", "window", &win_json.to_string()) {
        eprintln!("Failed to save window size: {e}");
    }

    // Shut down all terminals.
    for inst in &mut terminal.terminals {
        inst.inner.cleanup();
    }

    // Shut down every LSP transport (kill + reap, closing writer threads).
    lsp::clear_all_transports();

    false
}

#[cfg(not(feature = "sdl"))]
pub fn run(
    _config: NativeConfig,
    _args: &[String],
    _datadir: &str,
    _userdir: &str,
    _subsystems: crate::editor::subsystems::EditorSubsystems,
) -> bool {
    false
}

sdl_only! {
fn point_in_rect(rect: crate::editor::types::Rect, x: f64, y: f64) -> bool {
    x >= rect.x && x <= rect.x + rect.w && y >= rect.y && y <= rect.y + rect.h
}

fn diagnostic_hover_intent_rect(
    popup: crate::editor::types::Rect,
    anchor: crate::editor::types::Rect,
    padding: f64,
) -> crate::editor::types::Rect {
    let left = popup.x.min(anchor.x) - padding;
    let top = popup.y.min(anchor.y) - padding;
    let right = (popup.x + popup.w).max(anchor.x + anchor.w) + padding;
    let bottom = (popup.y + popup.h).max(anchor.y + anchor.h) + padding;
    crate::editor::types::Rect {
        x: left,
        y: top,
        w: right - left,
        h: bottom - top,
    }
}

/// Filter command list using fuzzy matching from the picker module.
fn fuzzy_filter_commands(query: &str, all_commands: &[(String, String)]) -> Vec<(String, String)> {
    if query.is_empty() {
        return all_commands.to_vec();
    }
    // Rank on the pretty name only (the part before the "  (ctrl+...)"
    // keybinding tail). `fuzzy_match`'s score includes a -length
    // penalty, so if we rank on the full display string an entry with
    // a keybinding ("Open File  (ctrl+o)") gets pushed below one
    // without a binding ("Open User Settings") on the query "open" —
    // which is exactly backwards for users who are typing the name of
    // a command they already know has a shortcut.
    let strip = |d: &str| d.split("  (").next().unwrap_or(d).to_string();
    let names: Vec<String> = all_commands.iter().map(|(_, d)| strip(d)).collect();
    let ranked = picker::rank_strings(names.clone(), query, false, &[], None);
    ranked
        .into_iter()
        .filter_map(|name| {
            names
                .iter()
                .position(|n| n == &name)
                .and_then(|i| all_commands.get(i).cloned())
        })
        .collect()
}

/// Escape a literal string for safe inclusion in a PCRE2 pattern.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Compile the find pattern based on the current toggle state.
fn build_find_regex(
    query: &str,
    use_regex: bool,
    whole_word: bool,
    case_insensitive: bool,
) -> Option<crate::editor::regex::NativeRegex> {
    if query.is_empty() {
        return None;
    }
    let mut pat = if use_regex {
        query.to_string()
    } else {
        regex_escape(query)
    };
    if whole_word {
        pat = format!(r"\b(?:{pat})\b");
    }
    // The whole document is searched as one subject, so `multiline` keeps
    // `^`/`$` anchored to line boundaries rather than the document ends.
    let flags = crate::editor::regex::CompileFlags {
        caseless: case_insensitive,
        multiline: true,
        ..Default::default()
    };
    crate::editor::regex::NativeRegex::compile(&pat, flags).ok()
}

/// Map a keystroke to a dialog-input undo/redo action via the keymap, so the
/// bindings the user configured for `doc:undo` / `doc:redo` drive the focused
/// dialog field too. Returns `Some(true)` when the key is bound to `doc:redo`,
/// `Some(false)` for `doc:undo`, and `None` for anything else.
fn keymap_field_undo(
    keymap: &NativeKeymap,
    key: &str,
    mods: crate::editor::event::Modifiers,
) -> Option<bool> {
    keymap.commands_for(key, mods).and_then(|cmds| {
        if cmds.iter().any(|c| c == "doc:redo") {
            Some(true)
        } else if cmds.iter().any(|c| c == "doc:undo") {
            Some(false)
        } else {
            None
        }
    })
}

/// A clipboard action resolved from the keymap for a focused dialog input.
#[derive(Clone, Copy)]
enum FieldClipboard {
    Copy,
    Cut,
    Paste,
}

/// Map a keystroke to a dialog-input clipboard action via the keymap, so the
/// bindings the user configured for `doc:copy` / `doc:cut` / `doc:paste` drive
/// the focused dialog field too. Returns `None` for anything else.
fn keymap_field_clipboard(
    keymap: &NativeKeymap,
    key: &str,
    mods: crate::editor::event::Modifiers,
) -> Option<FieldClipboard> {
    keymap.commands_for(key, mods).and_then(|cmds| {
        if cmds.iter().any(|c| c == "doc:paste") {
            Some(FieldClipboard::Paste)
        } else if cmds.iter().any(|c| c == "doc:cut") {
            Some(FieldClipboard::Cut)
        } else if cmds.iter().any(|c| c == "doc:copy") {
            Some(FieldClipboard::Copy)
        } else {
            None
        }
    })
}

/// Append clipboard text onto a single-line input buffer, dropping line breaks
/// so a multi-line paste collapses onto one line instead of corrupting a search
/// query. Used by the append-only find/replace, project-search, palette, and
/// notes inputs, none of which model more than one line.
fn append_clipboard_line(buf: &mut String, clip: &str) {
    buf.extend(clip.chars().filter(|&c| c != '\n' && c != '\r'));
}

/// Insert clipboard text into a caret-tracked single-line input at byte offset
/// `cursor`, dropping line breaks. Returns the caret position after the text.
fn insert_clipboard_line(buf: &mut String, cursor: usize, clip: &str) -> usize {
    let line: String = clip.chars().filter(|&c| c != '\n' && c != '\r').collect();
    buf.insert_str(cursor, &line);
    cursor + line.len()
}

/// Scan the document and return every match as (line, col, end_line, end_col).
/// All values are 1-based. Matches may span newlines; a match consuming a
/// line's trailing `\n` ends at column 1 of the next line.
fn compute_find_matches(
    dv: &DocView,
    query: &str,
    use_regex: bool,
    whole_word: bool,
    case_insensitive: bool,
) -> Vec<(usize, usize, usize, usize)> {
    let Some(re) = build_find_regex(query, use_regex, whole_word, case_insensitive) else {
        return Vec::new();
    };
    let Some(buf_id) = dv.buffer_id else {
        return Vec::new();
    };
    // Reuse the memoized subject across keystrokes, then run the scan without
    // holding the buffer lock.
    let Ok(subject) = buffer::with_buffer_mut(buf_id, |b| Ok(buffer::search_subject_cached(b)))
    else {
        return Vec::new();
    };
    subject.find_all(&re)
}

/// Like `compute_find_matches` but optionally restricts results to the lines
/// covered by `selection`. The range is `(start_line, start_col, end_line,
/// end_col)`, all 1-based.
fn compute_find_matches_filtered(
    dv: &DocView,
    query: &str,
    use_regex: bool,
    whole_word: bool,
    case_insensitive: bool,
    selection: Option<(usize, usize, usize, usize)>,
) -> Vec<(usize, usize, usize, usize)> {
    let all = compute_find_matches(dv, query, use_regex, whole_word, case_insensitive);
    let Some((sl, sc, el, ec)) = selection else {
        return all;
    };
    all.into_iter()
        .filter(|&(line, col, end_line, end_col)| {
            (line, col) >= (sl, sc) && (end_line, end_col) <= (el, ec)
        })
        .collect()
}

/// Index of the first match at or after (line, col). Wraps to 0 if nothing
/// later exists. Returns None only for an empty match list.
fn find_match_at_or_after(
    matches: &[(usize, usize, usize, usize)],
    line: usize,
    col: usize,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    for (i, m) in matches.iter().enumerate() {
        if m.0 > line || (m.0 == line && m.1 >= col) {
            return Some(i);
        }
    }
    Some(0)
}

/// Index of the last match strictly before (line, col). Wraps to the final
/// match if nothing earlier exists. Returns None only for an empty match list.
fn find_match_before(
    matches: &[(usize, usize, usize, usize)],
    line: usize,
    col: usize,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let mut last = None;
    for (i, m) in matches.iter().enumerate() {
        if m.0 < line || (m.0 == line && m.1 < col) {
            last = Some(i);
        } else {
            break;
        }
    }
    Some(last.unwrap_or(matches.len() - 1))
}

/// Move the caret to the given match and scroll the view so it is visible.
fn select_find_match(dv: &mut DocView, m: (usize, usize, usize, usize), replace_active: bool) {
    let (line, col, end_line, end_col) = m;
    let Some(buf_id) = dv.buffer_id else { return };
    let _ = buffer::with_buffer_mut(buf_id, |b| {
        b.selections = vec![line, col, end_line, end_col];
        Ok(())
    });
    // Use the real line height from the current style, not a hardcoded
    // 20.0 — that was off by ~50% at typical fonts, so the computed
    // scroll target landed nowhere near the match and F3 / Enter
    // appeared to do nothing when the match was off-screen.
    let style = crate::editor::style_ctx::current_style();
    let line_h = style.code_font_height * 1.2;
    // The find bar overlays the top of the doc view (2 rows normally,
    // 3 with Replace open). Subtract its height so "centered" means
    // centered in the *visible* area rather than under the bar.
    let bar_row_h = style.font_height + style.padding_y * 2.0;
    let bar_h = bar_row_h * if replace_active { 3.0 } else { 2.0 };
    let cursor_y = (line as f64 - 1.0) * line_h;
    // Always center, unconditionally. The previous "only if off-screen"
    // check used the wrong line_h so it misjudged visibility; forcing a
    // center on every F3 / Enter is both simpler and what users expect.
    let view_h = dv.rect().h;
    dv.target_scroll_y = (cursor_y - (view_h + bar_h) / 2.0).max(0.0);
}

/// Current caret as (line, col) using the "cursor end" of the selection.
fn doc_cursor(dv: &DocView) -> (usize, usize) {
    dv.buffer_id
        .and_then(|id| {
            buffer::with_buffer(id, |b| {
                let line = *b.selections.get(2).unwrap_or(&1);
                let col = *b.selections.get(3).unwrap_or(&1);
                Ok((line, col))
            })
            .ok()
        })
        .unwrap_or((1, 1))
}

/// Selection anchor as (line, col) — the "other end" from the caret.
fn doc_anchor(dv: &DocView) -> (usize, usize) {
    dv.buffer_id
        .and_then(|id| {
            buffer::with_buffer(id, |b| {
                let line = *b.selections.first().unwrap_or(&1);
                let col = *b.selections.get(1).unwrap_or(&1);
                Ok((line, col))
            })
            .ok()
        })
        .unwrap_or((1, 1))
}

/// Replace the current selection (match) with replacement text. Caller must
/// ensure the selection is the active find match — we trust the find state
/// machine to keep the caret aligned with `find_matches[find_current]`.
fn replace_current_match(dv: &mut DocView, find_query: &str, replacement: &str) {
    if find_query.is_empty() {
        return;
    }
    let Some(buf_id) = dv.buffer_id else { return };
    let _ = buffer::with_buffer_mut(buf_id, |b| {
        if buffer::get_selected_text(b).is_empty() {
            return Ok(());
        }
        buffer::push_undo(b);
        buffer::delete_selection(b);
        let line = b.selections[0];
        let col = b.selections[1];
        if line <= b.lines.len() {
            let l = &mut b.lines[line - 1];
            let byte_pos = char_to_byte(l, col - 1);
            l.insert_str(byte_pos, replacement);
            let new_col = col + replacement.chars().count();
            b.selections = vec![line, new_col, line, new_col];
        }
        Ok(())
    });
}

/// When the cursor sits inside a leading run of spaces, returns the number
/// of characters backspace should delete to align with the previous indent
/// boundary. Returns `None` when the normal single-character backspace
/// should run instead (tab-indented documents, non-whitespace before cursor,
/// or cursor at column 1).
fn smart_backspace_span(
    line_text: &str,
    col: usize,
    indent_type: &str,
    indent_size: usize,
) -> Option<usize> {
    if indent_type != "soft" || col <= 1 || indent_size == 0 {
        return None;
    }
    let leading = col - 1;
    let prefix_is_all_spaces = line_text
        .chars()
        .take(leading)
        .all(|c| c == ' ');
    if !prefix_is_all_spaces {
        return None;
    }
    let remove = if leading % indent_size == 0 {
        indent_size
    } else {
        leading % indent_size
    };
    if remove >= 2 { Some(remove) } else { None }
}

/// Returns true when `prefix` (the line content before the cursor) ends —
/// after trailing whitespace and a line-comment if any — with an
/// open-block character: `:`, `{`, `(`, or `[`. Used to drive smart
/// auto-indent on Enter.
fn smart_indent_opens_block(prefix: &str) -> bool {
    let stripped = strip_trailing_line_comment(prefix);
    matches!(stripped.trim_end().chars().last(), Some(':' | '{' | '(' | '['))
}

fn newline_indent(
    line_text: &str,
    before_cursor: &str,
    indent_type: &str,
    indent_size: usize,
    language_aware: bool,
) -> String {
    if !language_aware {
        return String::new();
    }
    let mut indent: String = line_text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    if smart_indent_opens_block(before_cursor) {
        if indent_type == "hard" {
            indent.push('\t');
        } else {
            indent.push_str(&" ".repeat(indent_size.max(1)));
        }
    }
    indent
}

fn carried_indent(line_text: &str, language_aware: bool) -> String {
    if !language_aware {
        return String::new();
    }
    line_text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Drop everything from the first `//`, `--`, or `#` line-comment marker.
/// Tracks simple single/double-quoted strings so that markers inside a
/// string literal are ignored. Heuristic only — does not understand raw
/// strings, escape sequences beyond a single backslash, or nested
/// comments — but it handles the common cases that drive smart-indent.
fn strip_trailing_line_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' {
            quote = Some(c);
            i += 1;
            continue;
        }
        if (c == b'/' && bytes.get(i + 1) == Some(&b'/'))
            || (c == b'-' && bytes.get(i + 1) == Some(&b'-'))
            || c == b'#'
        {
            return &s[..i];
        }
        i += 1;
    }
    s
}

/// Insert `text` at the active caret, replacing any selection and splitting
/// on '\n' across lines. Records an undo step and leaves the caret at the end
/// of the inserted text. Shared by clipboard paste and middle-click (PRIMARY)
/// paste so both behave identically.
fn insert_text_at_caret(b: &mut crate::editor::buffer::BufferState, text: &str) {
    buffer::push_undo(b);
    buffer::delete_selection(b);
    let line = b.selections[0];
    let col = b.selections[1];
    if line <= b.lines.len() {
        let l = &mut b.lines[line - 1];
        let byte_pos = char_to_byte(l, col - 1);
        let after = l[byte_pos..].to_string();
        l.truncate(byte_pos);
        let paste_lines: Vec<&str> = text.split('\n').collect();
        if paste_lines.len() == 1 {
            l.push_str(text);
            l.push_str(&after);
            let new_col = col + text.chars().count();
            b.selections = vec![line, new_col, line, new_col];
        } else {
            l.push_str(paste_lines[0]);
            l.push('\n');
            let mut cur_line = line;
            for (i, pl) in paste_lines.iter().enumerate().skip(1) {
                cur_line += 1;
                if i == paste_lines.len() - 1 {
                    let new_col = pl.chars().count() + 1;
                    let mut new_line = pl.to_string();
                    new_line.push_str(&after);
                    b.lines.insert(cur_line - 1, new_line);
                    b.selections = vec![cur_line, new_col, cur_line, new_col];
                } else {
                    b.lines.insert(cur_line - 1, format!("{pl}\n"));
                }
            }
        }
    }
}

fn code_action_disabled_reason(action: &serde_json::Value) -> Option<&str> {
    action
        .get("disabled")
        .and_then(|disabled| disabled.get("reason"))
        .and_then(|reason| reason.as_str())
}

/// Return the code-action index under a pointer, using the picker's rendered
/// row geometry. The caller supplies the selected index because it determines
/// the visible scroll window.
fn code_action_picker_row_at(
    x: f64,
    y: f64,
    style: &StyleContext,
    action_count: usize,
    selected: usize,
) -> Option<usize> {
    let (window_width, _, _, _) = crate::window::get_window_size();
    code_action_picker_row_at_width(
        x,
        y,
        window_width as f64,
        style,
        action_count,
        selected,
    )
}

fn code_action_picker_row_at_width(
    x: f64,
    y: f64,
    width: f64,
    style: &StyleContext,
    action_count: usize,
    selected: usize,
) -> Option<usize> {
    const MAX_VISIBLE: usize = 15;
    if action_count == 0 {
        return None;
    }
    let picker_width = (width * 0.5).max(400.0).min(width - 20.0);
    let picker_x = (width - picker_width) / 2.0;
    let line_height = style.font_height + style.padding_y;
    if line_height <= 0.0 {
        return None;
    }
    let rows_y = style.padding_y * 3.0 + line_height + style.divider_size;
    let visible = action_count.min(MAX_VISIBLE);
    let scroll_offset = selected.saturating_sub(MAX_VISIBLE - 1);
    if x < picker_x
        || x > picker_x + picker_width
        || y < rows_y
        || y >= rows_y + line_height * visible as f64
    {
        return None;
    }
    let row = ((y - rows_y) / line_height).floor() as usize;
    (row < visible).then_some(scroll_offset + row)
}

fn activate_lsp_state(
    active: &mut LspState,
    inactive: &mut HashMap<(String, String), LspState>,
    filetype: &str,
    root_uri: String,
) {
    if active.filetype == filetype && active.root_uri == root_uri {
        return;
    }
    let next_key = (filetype.to_string(), root_uri);
    let mut next_state = inactive.remove(&next_key).unwrap_or_else(LspState::new);
    // A state is bound to the (filetype, root) pair it serves from the moment
    // it becomes active, not from the moment a server first initializes. That
    // identity is what the caller compares against to decide a swap is needed,
    // and what keys the parked-state map, so a filetype whose server never
    // spawns still keeps its respawn backoff across frames.
    next_state.filetype = next_key.0.clone();
    next_state.root_uri = next_key.1.clone();
    let previous_state = std::mem::replace(active, next_state);
    if previous_state.filetype.is_empty() || previous_state.root_uri.is_empty() {
        return;
    }
    let previous_key = (
        previous_state.filetype.clone(),
        previous_state.root_uri.clone(),
    );
    if let Some(displaced) = inactive.insert(previous_key, previous_state)
        && let Some(displaced_transport) = displaced.transport_id
    {
        lsp::remove_transport(displaced_transport);
    }
}

fn collect_lsp_code_actions(
    result: Option<&serde_json::Value>,
) -> Vec<(String, serde_json::Value)> {
    let mut actions: Vec<_> = result
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|action| {
            let title = action.get("title")?.as_str()?;
            (!title.is_empty()).then(|| (title.to_string(), action.clone()))
        })
        .collect();
    actions.sort_by_key(|(_, action)| {
        !action
            .get("isPreferred")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    });
    actions
}

fn code_action_command(action: &serde_json::Value) -> Option<(String, serde_json::Value)> {
    let command = action.get("command")?;
    if let Some(name) = command.as_str() {
        return Some((
            name.to_string(),
            action
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        ));
    }
    let name = command.get("command")?.as_str()?.to_string();
    let arguments = command
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    Some((name, arguments))
}

fn execute_lsp_code_action(
    action: &serde_json::Value,
    docs: &mut Vec<crate::editor::open_doc::OpenDoc>,
    lsp_state: &mut LspState,
    use_git: bool,
    atomic: bool,
) {
    if let Some(edit) = action.get("edit") {
        let changed = apply_lsp_workspace_edit(edit, docs, use_git, atomic);
        if changed > 0 {
            for doc in docs.iter_mut() {
                doc.cached_change_id = -1;
            }
            crate::window::force_invalidate();
        }
    }
    if let (Some(tid), Some((command, arguments))) =
        (lsp_state.transport_id, code_action_command(action))
    {
        let req_id = lsp_state.next_id();
        lsp_state
            .pending_requests
            .insert(req_id, "workspace/executeCommand".to_string());
        let _ = lsp::send_message(
            tid,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "method": "workspace/executeCommand",
                "params": {
                    "command": command,
                    "arguments": arguments
                }
            }),
        );
    }
}

/// Apply a list of LSP `TextEdit`s to a buffer as one undoable change.
/// Edits are spliced last-position-first so earlier offsets stay valid.
/// LSP positions are 0-based; character is treated as a char column to match
/// the rest of this editor's LSP handling. Returns true if any edit applied.
///
/// A position addresses the document rather than a line, so a line index at
/// or past the last line denotes the end of the document - the range a server
/// sends for a whole-document format when the text ends with a newline, whose
/// final line belongs inside the replacement.
fn apply_lsp_text_edits(
    state: &mut crate::editor::buffer::BufferState,
    edits: &[serde_json::Value],
) -> bool {
    use crate::editor::buffer;
    let text: String = state.lines.concat();
    let mut line_starts: Vec<usize> = Vec::with_capacity(state.lines.len());
    let mut offset = 0usize;
    for line in &state.lines {
        line_starts.push(offset);
        offset += line.len();
    }
    let doc_end = text.len();
    let byte_at = |line: usize, character: usize| -> usize {
        let Some(start) = line_starts.get(line) else {
            return doc_end;
        };
        let content = &state.lines[line];
        let within = content
            .char_indices()
            .nth(character)
            .map_or(content.len(), |(byte, _)| byte);
        start + within
    };
    let mut parsed: Vec<(usize, usize, String)> = Vec::new();
    for e in edits {
        let range = e.get("range");
        let pos = |key: &str, field: &str| -> usize {
            range
                .and_then(|r| r.get(key))
                .and_then(|p| p.get(field))
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .max(0) as usize
        };
        let new_text = e
            .get("newText")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let start = byte_at(pos("start", "line"), pos("start", "character"));
        let end = byte_at(pos("end", "line"), pos("end", "character")).max(start);
        parsed.push((start, end, new_text));
    }
    if parsed.is_empty() {
        return false;
    }
    // The protocol requires the ranges to be disjoint, and splicing them
    // last-position-first is what keeps the earlier offsets valid. A reply that
    // carries overlapping ranges anyway would have the second splice read past
    // the end of text the first one shortened, so order by position and keep
    // only the edits that land inside the region no later edit has replaced.
    parsed.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut text = text;
    let mut untouched_from = text.len();
    let mut applied = false;
    for (start, end, new_text) in parsed {
        if end > untouched_from {
            continue;
        }
        text.replace_range(start..end, &new_text);
        untouched_from = start;
        applied = true;
    }
    if !applied {
        return false;
    }
    buffer::push_undo(state);
    state.lines = text.split_inclusive('\n').map(str::to_string).collect();
    if state.lines.is_empty() {
        state.lines.push(String::new());
    }
    buffer::sanitize_selections(&state.lines, &mut state.selections);
    state.total_bytes = state.lines.iter().map(|l| l.len() as u64).sum();
    true
}

/// Apply an LSP `WorkspaceEdit` (`changes` or `documentChanges` form) across
/// the affected files, opening any that are not already tabs, and save each so
/// the edit takes effect on disk. Returns the number of files changed.
fn apply_lsp_workspace_edit(
    edit: &serde_json::Value,
    docs: &mut Vec<crate::editor::open_doc::OpenDoc>,
    use_git: bool,
    atomic: bool,
) -> usize {
    let mut per_file: Vec<(String, Vec<serde_json::Value>)> = Vec::new();
    if let Some(changes) = edit.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in changes {
            if let Some(arr) = edits.as_array() {
                per_file.push((uri.clone(), arr.clone()));
            }
        }
    } else if let Some(dc) = edit.get("documentChanges").and_then(|d| d.as_array()) {
        for change in dc {
            let uri = change
                .get("textDocument")
                .and_then(|t| t.get("uri"))
                .and_then(|v| v.as_str());
            let edits = change.get("edits").and_then(|e| e.as_array());
            if let (Some(uri), Some(edits)) = (uri, edits) {
                per_file.push((uri.to_string(), edits.clone()));
            }
        }
    }
    let mut changed = 0;
    for (uri, edits) in per_file {
        let path = uri_to_path(&uri);
        if path.is_empty() {
            continue;
        }
        let idx = match docs.iter().position(|d| d.path == path) {
            Some(i) => i,
            None => {
                if !std::path::Path::new(&path).exists()
                    || !crate::editor::open_doc::open_file_into(&path, docs, use_git)
                {
                    continue;
                }
                docs.len() - 1
            }
        };
        let Some(buf_id) = docs[idx].view.buffer_id else {
            continue;
        };
        let applied = buffer::with_buffer_mut(buf_id, |b| Ok(apply_lsp_text_edits(b, &edits)))
            .unwrap_or(false);
        if applied {
            docs[idx].cached_change_id = -1;
            docs[idx].cached_render = std::sync::Arc::new(Vec::new());
            let p = docs[idx].path.clone();
            if let Ok(Ok(cid)) = buffer::with_buffer(buf_id, |b| {
                Ok(buffer::save_file(b, &p, b.crlf, atomic).map(|()| b.change_id))
            }) {
                docs[idx].saved_change_id = cid;
                docs[idx].saved_signature =
                    buffer::with_buffer(buf_id, |b| Ok(buffer::content_signature(&b.lines)))
                        .unwrap_or(0);
            }
            changed += 1;
        }
    }
    changed
}

/// Convert pasted text's leading whitespace to match the document's indent
/// style. Detects whether the clipboard content uses tabs or spaces, then
/// re-indents every line to the target style (preserving relative depth).
fn convert_paste_indent(text: &str, doc_indent_type: &str, doc_indent_size: usize) -> String {
    let size = doc_indent_size.max(1);
    // Detect the paste's dominant indent character: if any non-blank line
    // starts with a tab, treat the paste as tab-indented; otherwise spaces.
    let paste_uses_tabs = text.lines().any(|l| l.starts_with('\t'));
    let paste_uses_spaces = !paste_uses_tabs && text.lines().any(|l| l.starts_with(' '));
    // Detect the paste's space-indent width (smallest leading-space run > 0).
    let paste_space_width = if paste_uses_spaces {
        text.lines()
            .filter(|l| l.starts_with(' '))
            .map(|l| l.chars().take_while(|c| *c == ' ').count())
            .filter(|&n| n > 0)
            .min()
            .unwrap_or(size)
    } else {
        size
    };
    let doc_uses_tabs = doc_indent_type == "hard";
    // No conversion needed if both sides agree.
    if paste_uses_tabs == doc_uses_tabs && (!paste_uses_spaces || paste_space_width == size) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // Count the indent level of this line in the paste's style.
        let (indent_level, rest_start) = if paste_uses_tabs {
            let tabs = line.chars().take_while(|c| *c == '\t').count();
            let byte = line
                .char_indices()
                .nth(tabs)
                .map(|(i, _)| i)
                .unwrap_or(line.len());
            (tabs, byte)
        } else {
            let spaces = line.chars().take_while(|c| *c == ' ').count();
            let byte = line
                .char_indices()
                .nth(spaces)
                .map(|(i, _)| i)
                .unwrap_or(line.len());
            (spaces / paste_space_width, byte)
        };
        // Re-indent in the document's style.
        if doc_uses_tabs {
            for _ in 0..indent_level {
                out.push('\t');
            }
        } else {
            for _ in 0..indent_level * size {
                out.push(' ');
            }
        }
        out.push_str(&line[rest_start..]);
    }
    out
}

/// Convert char index to byte index in a string.
/// Returns true when `a` is a strictly greater semver than `b`.
/// Compares major, minor, patch numerically; non-numeric segments fall back to
/// lexicographic order so malformed tags don't panic.
fn semver_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> (u64, u64, u64) {
        let mut parts = s.splitn(4, '.');
        let major = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let patch = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    };
    parse(a) > parse(b)
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Count chars in a string (for column positioning).
fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// Handle a document command (cursor movement, editing).
/// `auto_scroll`: when true, the view scrolls to keep the cursor visible after
/// movement commands. Pass false for commands triggered by mouse clicks or
/// context menus — the user didn't intend to scroll.
/// `line_wrapping`: when true, horizontal auto-scroll is suppressed since the
/// cursor is always reachable by wrap — scrolling right would push content
/// out of view even though nothing extends past the visual right edge.
#[allow(clippy::too_many_arguments)]
fn handle_doc_command(
    dv: &mut DocView,
    cmd: &str,
    style: &StyleContext,
    indent_type: &str,
    indent_size: usize,
    comment_marker: Option<&CommentMarker>,
    language_aware: bool,
    auto_scroll: bool,
    line_wrapping: bool,
) {
    let Some(buf_id) = dv.buffer_id else { return };
    let line_h = style.code_font_height * 1.2;

    match cmd {
        "doc:copy" | "doc:cut" => {
            let text =
                buffer::with_buffer(buf_id, |b| Ok(buffer::get_selected_text(b)))
                    .unwrap_or_default();
            if !text.is_empty() {
                crate::window::set_clipboard_text(&text);
                if cmd == "doc:cut" {
                    let _ = buffer::with_buffer_mut(buf_id, |b| {
                        buffer::push_undo(b);
                        buffer::delete_selection(b);
                        Ok(())
                    });
                }
            }
            return;
        }
        "doc:paste" => {
            if let Some(text) = crate::window::get_clipboard_text() {
                let text = convert_paste_indent(&text, indent_type, indent_size);
                let _ = buffer::with_buffer_mut(buf_id, |b| {
                    buffer::push_undo(b);
                    buffer::delete_selection(b);
                    let line = b.selections[0];
                    let col = b.selections[1];
                    if line <= b.lines.len() {
                        let l = &mut b.lines[line - 1];
                        let byte_pos = char_to_byte(l, col - 1);
                        let after = l[byte_pos..].to_string();
                        l.truncate(byte_pos);
                        let paste_lines: Vec<&str> = text.split('\n').collect();
                        if paste_lines.len() == 1 {
                            l.push_str(&text);
                            l.push_str(&after);
                            let new_col = col + text.chars().count();
                            b.selections = vec![line, new_col, line, new_col];
                        } else {
                            l.push_str(paste_lines[0]);
                            l.push('\n');
                            let mut cur_line = line;
                            for (i, pl) in paste_lines.iter().enumerate().skip(1) {
                                cur_line += 1;
                                if i == paste_lines.len() - 1 {
                                    let new_col = pl.chars().count() + 1;
                                    let mut new_line = pl.to_string();
                                    new_line.push_str(&after);
                                    b.lines.insert(cur_line - 1, new_line);
                                    b.selections = vec![cur_line, new_col, cur_line, new_col];
                                } else {
                                    b.lines.insert(cur_line - 1, format!("{pl}\n"));
                                }
                            }
                        }
                    }
                    Ok(())
                });
            }
            return;
        }
        _ => {}
    }

    let mut prev_cursor_line: usize = 0;
    let _ = buffer::with_buffer_mut(buf_id, |b| {
        let anchor_line = *b.selections.first().unwrap_or(&1);
        let mut anchor_col = *b.selections.get(1).unwrap_or(&1);
        let cursor_line = *b.selections.get(2).unwrap_or(&anchor_line);
        let cursor_col = *b.selections.get(3).unwrap_or(&anchor_col);
        prev_cursor_line = cursor_line;
        let line_count = b.lines.len();

        // Selection: shift variants move cursor but keep anchor.
        let is_select = cmd.starts_with("doc:select-to-");
        let mut preserve_anchor = false;
        let mut handled = true;

        // Movement always operates on the cursor position.
        let mut line = cursor_line;
        let mut col = cursor_col;

        match cmd {
            "doc:select-none"
                if buffer::cursor_count(b) > 1 => {
                    buffer::remove_extra_cursors(b);
                    return Ok(());
                }
                // Collapse selection to cursor.
            "doc:create-cursor-previous-line" => {
                let last_idx = b.selections.len() - 4;
                let last_line = b.selections[last_idx + 2];
                let last_col = b.selections[last_idx + 3];
                if last_line > 1 {
                    let new_line = last_line - 1;
                    let max_col = char_count(b.lines[new_line - 1].trim_end_matches('\n')) + 1;
                    buffer::add_cursor(b, new_line, last_col.min(max_col));
                }
                return Ok(());
            }
            "doc:create-cursor-next-line" => {
                let last_idx = b.selections.len() - 4;
                let last_line = b.selections[last_idx + 2];
                let last_col = b.selections[last_idx + 3];
                if last_line < line_count {
                    let new_line = last_line + 1;
                    let max_col = char_count(b.lines[new_line - 1].trim_end_matches('\n')) + 1;
                    buffer::add_cursor(b, new_line, last_col.min(max_col));
                }
                return Ok(());
            }
            "doc:select-all" => {
                b.selections[0] = 1;
                b.selections[1] = 1;
                let last = b.lines.len();
                let last_col = char_count(b.lines[last - 1].trim_end_matches('\n')) + 1;
                b.selections[2] = last;
                b.selections[3] = last_col;
                return Ok(());
            }
            "doc:move-to-previous-char" | "doc:select-to-previous-char" => {
                if col > 1 {
                    col -= 1;
                } else if line > 1 {
                    line -= 1;
                    col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                }
            }
            "doc:move-to-next-char" | "doc:select-to-next-char" => {
                let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                if col < max_col {
                    col += 1;
                } else if line < line_count {
                    line += 1;
                    col = 1;
                }
            }
            "doc:move-to-previous-line" | "doc:select-to-previous-line"
                if line > 1 => {
                    line -= 1;
                    let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                    col = col.min(max_col);
                }
            "doc:move-to-next-line" | "doc:select-to-next-line"
                if line < line_count => {
                    line += 1;
                    let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                    col = col.min(max_col);
                }
            "doc:move-to-start-of-indentation" | "doc:select-to-start-of-indentation" => {
                let text = b.lines[line - 1].trim_end_matches('\n');
                let indent = text.len() - text.trim_start().len();
                col = if col == indent + 1 { 1 } else { indent + 1 };
            }
            "doc:move-to-end-of-line" | "doc:select-to-end-of-line" => {
                col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
            }
            "doc:move-to-start-of-doc" | "doc:select-to-start-of-doc" => {
                line = 1;
                col = 1;
            }
            "doc:move-to-end-of-doc" | "doc:select-to-end-of-doc" => {
                line = line_count;
                col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
            }
            "doc:move-to-previous-word-start" | "doc:select-to-previous-word-start" => {
                if col > 1 {
                    let text = b.lines[line - 1].trim_end_matches('\n');
                    let chars: Vec<char> = text.chars().collect();
                    let mut i = (col - 2).min(chars.len().saturating_sub(1));
                    // Skip whitespace backwards.
                    while i > 0 && chars[i].is_whitespace() {
                        i -= 1;
                    }
                    // Skip word chars backwards.
                    while i > 0 && !chars[i - 1].is_whitespace() && chars[i - 1].is_alphanumeric()
                        || chars.get(i.wrapping_sub(1)).is_some_and(|c| *c == '_')
                    {
                        if i == 0 {
                            break;
                        }
                        i -= 1;
                    }
                    col = i + 1;
                } else if line > 1 {
                    line -= 1;
                    col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                }
            }
            "doc:move-to-next-word-end" | "doc:select-to-next-word-end" => {
                let text = b.lines[line - 1].trim_end_matches('\n');
                let chars: Vec<char> = text.chars().collect();
                let max = chars.len();
                let mut i = col - 1;
                if i < max {
                    // Skip word chars forward.
                    while i < max && (chars[i].is_alphanumeric() || chars[i] == '_') {
                        i += 1;
                    }
                    // Skip whitespace forward.
                    while i < max && chars[i].is_whitespace() {
                        i += 1;
                    }
                    col = i + 1;
                } else if line < line_count {
                    line += 1;
                    col = 1;
                }
            }
            "doc:delete-to-previous-word-start" => {
                buffer::push_undo(b);
                let text = b.lines[line - 1].trim_end_matches('\n').to_string();
                let chars: Vec<char> = text.chars().collect();
                let mut i = (col - 2).min(chars.len().saturating_sub(1));
                while i > 0 && chars[i].is_whitespace() {
                    i -= 1;
                }
                while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
                    i -= 1;
                }
                let new_col = i + 1;
                let l = &mut b.lines[line - 1];
                let start = char_to_byte(l, new_col - 1);
                let end = char_to_byte(l, col - 1);
                l.drain(start..end);
                col = new_col;
            }
            "doc:delete-to-next-word-end" => {
                buffer::push_undo(b);
                let text = b.lines[line - 1].trim_end_matches('\n').to_string();
                let chars: Vec<char> = text.chars().collect();
                let max = chars.len();
                let mut i = col - 1;
                while i < max && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                while i < max && chars[i].is_whitespace() {
                    i += 1;
                }
                let l = &mut b.lines[line - 1];
                let start = char_to_byte(l, col - 1);
                let end = char_to_byte(l, i);
                l.drain(start..end);
            }
            "doc:duplicate-lines" => {
                buffer::push_undo(b);
                let dup = b.lines[line - 1].clone();
                b.lines.insert(line, dup);
                line += 1;
            }
            "doc:delete-lines" => {
                buffer::push_undo(b);
                if b.lines.len() > 1 {
                    b.lines.remove(line - 1);
                    if line > b.lines.len() {
                        line = b.lines.len();
                    }
                    let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                    col = col.min(max_col);
                } else {
                    b.lines[0] = "\n".to_string();
                    col = 1;
                }
            }
            "doc:move-lines-up"
                if line > 1 => {
                    buffer::push_undo(b);
                    b.lines.swap(line - 1, line - 2);
                    line -= 1;
                }
            "doc:move-lines-down"
                if line < line_count => {
                    buffer::push_undo(b);
                    b.lines.swap(line - 1, line);
                    line += 1;
                }
            "doc:move-to-previous-page" | "doc:select-to-previous-page" => {
                let page = (dv.rect().h / (style.code_font_height * 1.2)) as usize;
                line = line.saturating_sub(page).max(1);
                let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                col = col.min(max_col);
            }
            "doc:move-to-next-page" | "doc:select-to-next-page" => {
                let page = (dv.rect().h / (style.code_font_height * 1.2)) as usize;
                line = (line + page).min(line_count);
                let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                col = col.min(max_col);
            }
            "doc:backspace" | "doc:delete"
                if anchor_line != cursor_line || anchor_col != cursor_col =>
            {
                // Selection active: delete the selected text.
                buffer::push_undo(b);
                buffer::delete_selection(b);
                line = b.selections[0];
                col = b.selections[1];
            }
            "doc:backspace" => {
                buffer::push_undo(b);
                let n = buffer::cursor_count(b);
                if n > 1 {
                    // Multi-cursor backspace: process bottom-to-top.
                    let mut positions: Vec<(usize, usize, usize)> = (0..n)
                        .map(|i| {
                            let base = i * 4;
                            (i, b.selections[base + 2], b.selections[base + 3])
                        })
                        .collect();
                    positions.sort_by(|a, bp| bp.1.cmp(&a.1).then(bp.2.cmp(&a.2)));
                    let mut results: Vec<(usize, usize, usize)> = Vec::new();
                    for &(idx, cline, ccol) in &positions {
                        if cline <= b.lines.len()
                            && let Some(remove) = smart_backspace_span(
                                &b.lines[cline - 1],
                                ccol,
                                indent_type,
                                indent_size,
                            )
                        {
                            let l = &mut b.lines[cline - 1];
                            let bp = char_to_byte(l, ccol - 1 - remove);
                            let ep = char_to_byte(l, ccol - 1);
                            l.drain(bp..ep);
                            results.push((idx, cline, ccol - remove));
                        } else if ccol > 1 && cline <= b.lines.len() {
                            let l = &mut b.lines[cline - 1];
                            let bp = char_to_byte(l, ccol - 2);
                            let ep = char_to_byte(l, ccol - 1);
                            l.drain(bp..ep);
                            results.push((idx, cline, ccol - 1));
                        } else if cline > 1 {
                            let removed = b.lines.remove(cline - 1);
                            let new_line = cline - 1;
                            let prev_len = char_count(b.lines[new_line - 1].trim_end_matches('\n'));
                            let prev = &mut b.lines[new_line - 1];
                            if prev.ends_with('\n') {
                                prev.pop();
                            }
                            prev.push_str(&removed);
                            results.push((idx, new_line, prev_len + 1));
                        } else {
                            results.push((idx, cline, ccol));
                        }
                    }
                    for (idx, rl, rc) in results {
                        let base = idx * 4;
                        b.selections[base] = rl;
                        b.selections[base + 1] = rc;
                        b.selections[base + 2] = rl;
                        b.selections[base + 3] = rc;
                    }
                    return Ok(());
                }
                buffer::delete_selection(b);
                line = b.selections[0];
                col = b.selections[1];
                if let Some(remove) = smart_backspace_span(
                    &b.lines[line - 1],
                    col,
                    indent_type,
                    indent_size,
                ) {
                    let l = &mut b.lines[line - 1];
                    let byte_start = char_to_byte(l, col - 1 - remove);
                    let byte_end = char_to_byte(l, col - 1);
                    l.drain(byte_start..byte_end);
                    col -= remove;
                } else if col > 1 {
                    let l = &mut b.lines[line - 1];
                    let byte_pos = char_to_byte(l, col - 2);
                    let end = char_to_byte(l, col - 1);
                    l.drain(byte_pos..end);
                    col -= 1;
                } else if line > 1 {
                    let removed = b.lines.remove(line - 1);
                    line -= 1;
                    let prev_len = char_count(b.lines[line - 1].trim_end_matches('\n'));
                    let prev = &mut b.lines[line - 1];
                    if prev.ends_with('\n') {
                        prev.pop();
                    }
                    prev.push_str(&removed);
                    col = prev_len + 1;
                }
            }
            "doc:delete" => {
                buffer::push_undo(b);
                let n = buffer::cursor_count(b);
                if n > 1 {
                    // Multi-cursor delete: process bottom-to-top.
                    let mut positions: Vec<(usize, usize, usize)> = (0..n)
                        .map(|i| {
                            let base = i * 4;
                            (i, b.selections[base + 2], b.selections[base + 3])
                        })
                        .collect();
                    positions.sort_by(|a, bp| bp.1.cmp(&a.1).then(bp.2.cmp(&a.2)));
                    for &(_idx, cline, ccol) in &positions {
                        if cline > b.lines.len() {
                            continue;
                        }
                        let max_c = char_count(b.lines[cline - 1].trim_end_matches('\n')) + 1;
                        if ccol < max_c {
                            let l = &mut b.lines[cline - 1];
                            let bp = char_to_byte(l, ccol - 1);
                            let ep = char_to_byte(l, ccol);
                            l.drain(bp..ep);
                        } else if cline < b.lines.len() {
                            let removed = b.lines.remove(cline);
                            let cur = &mut b.lines[cline - 1];
                            if cur.ends_with('\n') {
                                cur.pop();
                            }
                            cur.push_str(&removed);
                        }
                    }
                    return Ok(());
                }
                let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                if col < max_col {
                    let l = &mut b.lines[line - 1];
                    let byte_pos = char_to_byte(l, col - 1);
                    let end = char_to_byte(l, col);
                    l.drain(byte_pos..end);
                } else if line < b.lines.len() {
                    let removed = b.lines.remove(line);
                    let cur = &mut b.lines[line - 1];
                    if cur.ends_with('\n') {
                        cur.pop();
                    }
                    cur.push_str(&removed);
                }
            }
            "doc:newline" => {
                buffer::push_undo(b);
                buffer::delete_selection(b);
                line = b.selections[0];
                col = b.selections[1];
                let line_text = b.lines[line - 1].clone();
                let l = &mut b.lines[line - 1];
                let byte_pos = char_to_byte(l, col - 1);
                let rest = l[byte_pos..].to_string();
                let before_cursor = l[..byte_pos].to_string();
                l.truncate(byte_pos);
                l.push('\n');
                let indent = newline_indent(
                    &line_text,
                    &before_cursor,
                    indent_type,
                    indent_size,
                    language_aware,
                );
                let new_line = format!("{indent}{rest}");
                let new_col = indent.chars().count() + 1;
                b.lines.insert(line, new_line);
                line += 1;
                col = new_col;
            }
            "doc:newline-below" => {
                buffer::push_undo(b);
                let indent = carried_indent(&b.lines[line - 1], language_aware);
                let new_line = format!("{indent}\n");
                let new_col = indent.len() + 1;
                b.lines.insert(line, new_line);
                line += 1;
                col = new_col;
            }
            "doc:newline-above" => {
                buffer::push_undo(b);
                let indent = carried_indent(&b.lines[line - 1], language_aware);
                let new_line = format!("{indent}\n");
                let new_col = indent.len() + 1;
                b.lines.insert(line - 1, new_line);
                col = new_col;
            }
            "doc:indent" => {
                buffer::push_undo(b);
                let indent_str = if indent_type == "hard" {
                    "\t".to_string()
                } else {
                    " ".repeat(indent_size)
                };
                let indent_len = indent_str.chars().count();
                if anchor_line != cursor_line {
                    let (start, end) =
                        (anchor_line.min(cursor_line), anchor_line.max(cursor_line));
                    for i in start..=end {
                        if let Some(l) = b.lines.get_mut(i - 1) {
                            l.insert_str(0, &indent_str);
                        }
                    }
                    col += indent_len;
                    anchor_col += indent_len;
                    preserve_anchor = true;
                } else {
                    let l = &mut b.lines[line - 1];
                    let byte_pos = char_to_byte(l, col - 1);
                    l.insert_str(byte_pos, &indent_str);
                    col += indent_len;
                }
            }
            "core:sort-lines" => {
                buffer::push_undo(b);
                let (start, end) = if anchor_line != cursor_line || anchor_col != cursor_col {
                    // If cursor is at col 1 of the last selected line, exclude it.
                    let raw_end = if cursor_line > anchor_line && cursor_col <= 1 {
                        cursor_line - 1
                    } else {
                        cursor_line
                    };
                    if anchor_line <= raw_end {
                        (anchor_line, raw_end)
                    } else {
                        (raw_end, anchor_line)
                    }
                } else {
                    (1, b.lines.len())
                };
                let slice = &mut b.lines[start - 1..end];
                slice.sort();
                // Place cursor at the start of the sorted range.
                line = start;
                col = 1;
            }
            "doc:upper-case" | "doc:lower-case"
                if (anchor_line != cursor_line || anchor_col != cursor_col) => {
                    buffer::push_undo(b);
                    let (s_line, s_col, e_line, e_col) = if anchor_line < cursor_line
                        || (anchor_line == cursor_line && anchor_col <= cursor_col)
                    {
                        (anchor_line, anchor_col, cursor_line, cursor_col)
                    } else {
                        (cursor_line, cursor_col, anchor_line, anchor_col)
                    };
                    let is_upper = cmd == "doc:upper-case";
                    if s_line == e_line {
                        let l = &mut b.lines[s_line - 1];
                        let start_byte =
                            l.char_indices().nth(s_col - 1).map(|(i, _)| i).unwrap_or(0);
                        let end_byte = l
                            .char_indices()
                            .nth(e_col - 1)
                            .map(|(i, _)| i)
                            .unwrap_or(l.len());
                        let fragment = &l[start_byte..end_byte];
                        let converted = if is_upper {
                            fragment.to_uppercase()
                        } else {
                            fragment.to_lowercase()
                        };
                        l.replace_range(start_byte..end_byte, &converted);
                    } else {
                        for li in s_line..=e_line {
                            let l = &mut b.lines[li - 1];
                            let start = if li == s_line {
                                l.char_indices().nth(s_col - 1).map(|(i, _)| i).unwrap_or(0)
                            } else {
                                0
                            };
                            let end = if li == e_line {
                                l.char_indices()
                                    .nth(e_col - 1)
                                    .map(|(i, _)| i)
                                    .unwrap_or(l.len())
                            } else {
                                l.trim_end_matches('\n').len()
                            };
                            let fragment = &l[start..end];
                            let converted = if is_upper {
                                fragment.to_uppercase()
                            } else {
                                fragment.to_lowercase()
                            };
                            l.replace_range(start..end, &converted);
                        }
                    }
                }
            "doc:toggle-line-comments" => {
                let Some(marker) = comment_marker else {
                    // Language has no defined comment style; do nothing
                    // rather than guessing and corrupting the file.
                    return Ok(());
                };
                buffer::push_undo(b);
                let (start, end) = if anchor_line != cursor_line {
                    (anchor_line.min(cursor_line), anchor_line.max(cursor_line))
                } else {
                    (line, line)
                };
                match marker {
                    CommentMarker::Line(prefix) => {
                        let prefix_space = format!("{prefix} ");
                        // All non-blank lines must already start with the
                        // prefix for the toggle to remove rather than add.
                        let all_commented = (start..=end)
                            .filter_map(|i| b.lines.get(i - 1))
                            .filter(|l| !l.trim().is_empty())
                            .all(|l| l.trim_start().starts_with(prefix.as_str()));
                        if all_commented {
                            for i in start..=end {
                                if let Some(l) = b.lines.get_mut(i - 1) {
                                    if let Some(pos) = l.find(&prefix_space) {
                                        l.replace_range(pos..pos + prefix_space.len(), "");
                                    } else if let Some(pos) = l.find(prefix.as_str()) {
                                        l.replace_range(pos..pos + prefix.len(), "");
                                    }
                                }
                            }
                        } else {
                            for i in start..=end {
                                if let Some(l) = b.lines.get_mut(i - 1) {
                                    if l.trim().is_empty() {
                                        continue;
                                    }
                                    let indent_len =
                                        l.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                                    let byte = l
                                        .char_indices()
                                        .nth(indent_len)
                                        .map(|(i, _)| i)
                                        .unwrap_or(0);
                                    l.insert_str(byte, &prefix_space);
                                }
                            }
                        }
                    }
                    CommentMarker::Block(open, close) => {
                        // Per-line wrap: open at start (after indent), close at
                        // end (before any trailing whitespace + newline). When
                        // every non-blank line is already wrapped, strip instead.
                        let all_wrapped = (start..=end)
                            .filter_map(|i| b.lines.get(i - 1))
                            .filter(|l| !l.trim().is_empty())
                            .all(|l| {
                                let trimmed = l.trim_end_matches('\n').trim_end();
                                let stripped_left = l.trim_start();
                                stripped_left.starts_with(open.as_str())
                                    && trimmed.ends_with(close.as_str())
                                    && trimmed.len() >= open.len() + close.len()
                            });
                        if all_wrapped {
                            for i in start..=end {
                                if let Some(l) = b.lines.get_mut(i - 1) {
                                    let had_newline = l.ends_with('\n');
                                    let body = l.trim_end_matches('\n').to_string();
                                    let trailing_ws_len = body.len() - body.trim_end().len();
                                    let trailing_ws =
                                        body[body.len() - trailing_ws_len..].to_string();
                                    let core = body[..body.len() - trailing_ws_len].to_string();
                                    // Strip closing marker (with optional preceding space).
                                    let core = if let Some(c) = core.strip_suffix(close.as_str()) {
                                        c.strip_suffix(' ').unwrap_or(c).to_string()
                                    } else {
                                        core
                                    };
                                    // Strip opening marker (with optional trailing space) after indent.
                                    let indent_len = core
                                        .chars()
                                        .take_while(|c| *c == ' ' || *c == '\t')
                                        .count();
                                    let indent_byte = core
                                        .char_indices()
                                        .nth(indent_len)
                                        .map(|(i, _)| i)
                                        .unwrap_or(core.len());
                                    let (indent, rest) = core.split_at(indent_byte);
                                    let rest = rest.strip_prefix(open.as_str()).unwrap_or(rest);
                                    let rest = rest.strip_prefix(' ').unwrap_or(rest);
                                    let mut new_line = format!("{indent}{rest}{trailing_ws}");
                                    if had_newline {
                                        new_line.push('\n');
                                    }
                                    *l = new_line;
                                }
                            }
                        } else {
                            for i in start..=end {
                                if let Some(l) = b.lines.get_mut(i - 1) {
                                    if l.trim().is_empty() {
                                        continue;
                                    }
                                    let had_newline = l.ends_with('\n');
                                    let body = l.trim_end_matches('\n').to_string();
                                    let indent_len = body
                                        .chars()
                                        .take_while(|c| *c == ' ' || *c == '\t')
                                        .count();
                                    let indent_byte = body
                                        .char_indices()
                                        .nth(indent_len)
                                        .map(|(i, _)| i)
                                        .unwrap_or(0);
                                    let (indent, rest) = body.split_at(indent_byte);
                                    let mut new_line =
                                        format!("{indent}{open} {} {close}", rest.trim_end());
                                    // Preserve any trailing whitespace after the close marker.
                                    let trailing_ws_len = rest.len() - rest.trim_end().len();
                                    if trailing_ws_len > 0 {
                                        new_line.push_str(&rest[rest.len() - trailing_ws_len..]);
                                    }
                                    if had_newline {
                                        new_line.push('\n');
                                    }
                                    *l = new_line;
                                }
                            }
                        }
                    }
                }
            }
            "doc:unindent" => {
                buffer::push_undo(b);
                let (start, end) = if anchor_line != cursor_line {
                    (anchor_line.min(cursor_line), anchor_line.max(cursor_line))
                } else {
                    (line, line)
                };
                for i in start..=end {
                    if let Some(l) = b.lines.get_mut(i - 1) {
                        if indent_type == "hard" {
                            if l.starts_with('\t') {
                                l.remove(0);
                            }
                        } else {
                            let remove = l
                                .chars()
                                .take(indent_size)
                                .take_while(|c| *c == ' ')
                                .count();
                            if remove > 0 {
                                l.replace_range(..remove, "");
                            }
                        }
                    }
                }
                col = col.saturating_sub(indent_size).max(1);
                if anchor_line != cursor_line {
                    anchor_col = anchor_col.saturating_sub(indent_size).max(1);
                    preserve_anchor = true;
                }
            }
            "doc:join-lines" => {
                buffer::push_undo(b);
                if line < b.lines.len() {
                    let next = b.lines.remove(line);
                    let trimmed = next.trim_start().trim_end_matches('\n');
                    let l = &mut b.lines[line - 1];
                    if l.ends_with('\n') {
                        l.pop();
                    }
                    if !l.ends_with(' ') && !trimmed.is_empty() {
                        l.push(' ');
                    }
                    col = l.chars().count() + 1;
                    l.push_str(trimmed);
                    l.push('\n');
                }
            }
            _ => {
                handled = false;
            }
        }

        if !handled {
            return Ok(());
        }

        // Collapse to single cursor when a non-create-cursor command runs.
        if buffer::cursor_count(b) > 1 {
            buffer::remove_extra_cursors(b);
        }

        // Update selections: select commands and indent/unindent keep anchor,
        // move commands collapse.
        if is_select || preserve_anchor {
            b.selections[0] = anchor_line;
            b.selections[1] = anchor_col;
        } else {
            b.selections[0] = line;
            b.selections[1] = col;
        }
        b.selections[2] = line;
        b.selections[3] = col;
        Ok(())
    });

    // Auto-scroll to keep cursor visible — only for keyboard-initiated
    // navigation where the cursor's line actually changed. Snap scroll_y
    // to the new target so Enter on the last line doesn't leave the
    // fresh line visibly clipped for the ~6 frames the lerp takes to
    // settle — the old behaviour was only "saved" by the user issuing
    // another command (save, etc.) that triggered an unrelated repaint.
    if auto_scroll {
        let _ = buffer::with_buffer(buf_id, |b| {
            let cursor_line = *b.selections.get(2).unwrap_or(&1);
            if cursor_line == prev_cursor_line {
                return Ok(());
            }
            let cursor_y = (cursor_line as f64 - 1.0) * line_h;
            let view_h = dv.rect().h;
            // One line of margin above and below the cursor so it's never
            // drawn flush with the viewport edge (would otherwise clip
            // the descender, and on the last line the glyph sat at
            // half-height until some later event forced another scroll).
            let margin = line_h;
            let mut new_target = dv.target_scroll_y;
            if cursor_y - margin < new_target {
                new_target = (cursor_y - margin).max(0.0);
            } else if cursor_y + line_h + margin > new_target + view_h {
                new_target = cursor_y + line_h + margin - view_h;
            }
            if new_target != dv.target_scroll_y {
                dv.target_scroll_y = new_target;
                dv.scroll_y = new_target;
            }
            Ok(())
        });
    }

    // Horizontal auto-scroll to keep cursor visible (e.g. End on a long line).
    // Cross-line jumps only scroll LEFT (to reveal a cursor at a small column),
    // never RIGHT (which would push the left-side content of nearby shorter
    // lines off-screen and make the document appear blank). When line
    // wrapping is on, the caret always has a visible visual row, so this
    // whole block would otherwise chase a virtual column that doesn't
    // exist — pin `scroll_x` to 0 instead.
    if line_wrapping {
        if dv.scroll_x != 0.0 || dv.target_scroll_x != 0.0 {
            dv.scroll_x = 0.0;
            dv.target_scroll_x = 0.0;
        }
    } else if dv.code_char_w > 0.0 {
        let _ = buffer::with_buffer(buf_id, |b| {
            let cursor_line_now = *b.selections.get(2).unwrap_or(&1);
            let cursor_col = *b.selections.get(3).unwrap_or(&1);
            let cursor_x = (cursor_col as f64 - 1.0) * dv.code_char_w;
            let text_w =
                (dv.rect().w - dv.gutter_width - style.padding_x * 2.0 - style.scrollbar_size)
                    .max(0.0);
            // Keep one char of trailing padding so the caret isn't flush with the right edge.
            let right_pad = dv.code_char_w;
            let same_line = cursor_line_now == prev_cursor_line;
            if cursor_x < dv.scroll_x {
                dv.scroll_x = cursor_x;
                dv.target_scroll_x = cursor_x;
            } else if same_line && cursor_x + right_pad > dv.scroll_x + text_w {
                dv.scroll_x = (cursor_x + right_pad - text_w).max(0.0);
                dv.target_scroll_x = dv.scroll_x;
            }
            Ok(())
        });
    }

    // Fold/unfold commands operate on dv.folds outside the buffer closure.
    match cmd {
        "doc:fold" => {
            let _ = buffer::with_buffer(buf_id, |b| {
                let cursor_line = *b.selections.get(2).unwrap_or(&1);
                if let Some(end) = crate::editor::picker::get_fold_end(&b.lines, cursor_line) {
                    if !dv.folds.iter().any(|(s, _)| *s == cursor_line) {
                        dv.folds.push((cursor_line, end));
                        dv.folds.sort_by_key(|(s, _)| *s);
                    }
                }
                Ok(())
            });
        }
        "doc:unfold" => {
            let _ = buffer::with_buffer(buf_id, |b| {
                let cursor_line = *b.selections.get(2).unwrap_or(&1);
                dv.folds
                    .retain(|(s, e)| !(cursor_line >= *s && cursor_line <= *e));
                Ok(())
            });
        }
        "doc:unfold-all" => {
            dv.folds.clear();
        }
        "doc:toggle-bookmark" => {
            let _ = buffer::with_buffer(buf_id, |b| {
                let cursor_line = *b.selections.get(2).unwrap_or(&1);
                if let Some(pos) = dv.bookmarks.iter().position(|&l| l == cursor_line) {
                    dv.bookmarks.remove(pos);
                } else {
                    dv.bookmarks.push(cursor_line);
                    dv.bookmarks.sort();
                }
                Ok(())
            });
        }
        "doc:next-bookmark"
            if !dv.bookmarks.is_empty() => {
                let _ = buffer::with_buffer_mut(buf_id, |b| {
                    let cursor_line = *b.selections.get(2).unwrap_or(&1);
                    let target = dv
                        .bookmarks
                        .iter()
                        .find(|&&l| l > cursor_line)
                        .copied()
                        .unwrap_or(dv.bookmarks[0]);
                    b.selections = vec![target, 1, target, 1];
                    Ok(())
                });
                scroll_to_cursor(dv);
            }
        "doc:previous-bookmark"
            if !dv.bookmarks.is_empty() => {
                let _ = buffer::with_buffer_mut(buf_id, |b| {
                    let cursor_line = *b.selections.get(2).unwrap_or(&1);
                    let target = dv
                        .bookmarks
                        .iter()
                        .rev()
                        .find(|&&l| l < cursor_line)
                        .copied()
                        .unwrap_or(*dv.bookmarks.last().unwrap_or(&1));
                    b.selections = vec![target, 1, target, 1];
                    Ok(())
                });
                scroll_to_cursor(dv);
            }
        _ => {}
    }
}

/// Scroll view so the cursor line is visible.
fn scroll_to_cursor(dv: &mut DocView) {
    let Some(buf_id) = dv.buffer_id else { return };
    let _ = buffer::with_buffer(buf_id, |b| {
        let cursor_line = *b.selections.get(2).unwrap_or(&1);
        let line_h = 20.0;
        let cursor_y = (cursor_line as f64 - 1.0) * line_h;
        let view_h = dv.rect().h;
        if cursor_y < dv.target_scroll_y || cursor_y + line_h > dv.target_scroll_y + view_h {
            dv.target_scroll_y = (cursor_y - view_h / 2.0).max(0.0);
        }
        Ok(())
    });
}

/// Parse a hex color string like "#rrggbb" or "#rrggbbaa" or "rgba(r,g,b,a)" into Color.
fn parse_theme_color(s: &str) -> Option<crate::editor::types::Color> {
    use crate::editor::types::Color;
    if let Some(hex) = s.strip_prefix('#') {
        let hex = hex.trim();
        if hex.len() == 6 || hex.len() == 8 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let a = if hex.len() == 8 {
                u8::from_str_radix(&hex[6..8], 16).ok()?
            } else {
                255
            };
            return Some(Color::new(r, g, b, a));
        }
    }
    if s.starts_with("rgba(") {
        let inner = s.trim_start_matches("rgba(").trim_end_matches(')');
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 4 {
            let r = parts[0].trim().parse::<u8>().ok()?;
            let g = parts[1].trim().parse::<u8>().ok()?;
            let b = parts[2].trim().parse::<u8>().ok()?;
            let a = (parts[3].trim().parse::<f64>().ok()? * 255.0) as u8;
            return Some(Color::new(r, g, b, a));
        }
    }
    None
}

/// Apply a loaded theme palette to a StyleContext.
fn apply_theme_to_style(style: &mut StyleContext, palette: &crate::editor::style::ThemePalette) {
    let set = |field: &mut crate::editor::types::Color, key: &str| {
        if let Some(hex) = palette.colors.get(key) {
            if let Some(c) = parse_theme_color(hex) {
                *field = c;
            }
        }
    };
    set(&mut style.background, "background");
    set(&mut style.background2, "background2");
    set(&mut style.background3, "background3");
    set(&mut style.text, "text");
    set(&mut style.caret, "caret");
    set(&mut style.accent, "accent");
    set(&mut style.dim, "dim");
    set(&mut style.divider, "divider");
    set(&mut style.selection, "selection");
    set(&mut style.line_number, "line_number");
    set(&mut style.line_number2, "line_number2");
    set(&mut style.line_highlight, "line_highlight");
    set(&mut style.scrollbar, "scrollbar");
    set(&mut style.scrollbar2, "scrollbar2");
    set(&mut style.scrollbar_track, "scrollbar_track");
    set(&mut style.nagbar, "nagbar");
    set(&mut style.nagbar_text, "nagbar_text");
    set(&mut style.nagbar_dim, "nagbar_dim");
    set(&mut style.good, "good");
    set(&mut style.warn, "warn");
    set(&mut style.error, "error");

    // Store syntax colors in a thread-local for the tokenizer to use.
    if let Some(syn) = palette.sub_palettes.get("syntax") {
        let mut colors = std::collections::HashMap::new();
        for (k, v) in syn {
            if let Some(c) = parse_theme_color(v) {
                colors.insert(k.clone(), c.to_array());
            }
        }
        SYNTAX_COLORS.with(|s| *s.borrow_mut() = colors);
    }
}


/// Load fonts from NativeConfig into a draw context.
fn load_fonts(
    config: &NativeConfig,
) -> Result<crate::editor::draw_context::NativeDrawContext, String> {
    use crate::renderer::{Antialiasing, FontInner, Hinting};

    let mut ctx = crate::editor::draw_context::NativeDrawContext::new();

    // Display scale: ratio of pixel size to logical window size.
    let scale = crate::window::get_display_scale();

    let load = |spec: &crate::editor::config::FontSpec,
                ctx: &mut crate::editor::draw_context::NativeDrawContext|
     -> Result<u64, String> {
        let aa = spec
            .options
            .antialiasing
            .as_deref()
            .map(|s| match s {
                "none" => Antialiasing::None,
                "grayscale" => Antialiasing::Grayscale,
                _ => Antialiasing::Subpixel,
            })
            .unwrap_or_default();
        let hint = spec
            .options
            .hinting
            .as_deref()
            .map(|s| match s {
                "none" => Hinting::None,
                "full" => Hinting::Full,
                _ => Hinting::Slight,
            })
            .unwrap_or_default();
        let paths: Vec<&str> = if let Some(ref ps) = spec.paths {
            ps.iter().map(String::as_str).collect()
        } else if let Some(ref p) = spec.path {
            vec![p.as_str()]
        } else {
            return Err("font spec has no path".into());
        };
        let mut refs = Vec::new();
        for path in paths {
            let scaled_size = spec.size as f32 * scale as f32;
            let inner = FontInner::load(path, scaled_size, aa, hint)?;
            let arc = std::sync::Arc::new(parking_lot::Mutex::new(inner));
            crate::renderer::font::register_font(&arc);
            refs.push(arc);
        }
        Ok(ctx.add_font(refs))
    };

    let ui = load(&config.fonts.ui, &mut ctx)?;
    let code = load(&config.fonts.code, &mut ctx)?;

    // Load scaled heading fonts from the UI font path. Sizes scale the body
    // font size (`config.fonts.ui.size`) by decreasing factors so h1 > h2 >
    // h3 > h4 (= body). h4-h6 reuse the body slot. Any load failure falls
    // back to the body font so a missing path never blocks startup.
    let load_heading = |mul: f64, ctx: &mut crate::editor::draw_context::NativeDrawContext| {
        let spec = crate::editor::config::FontSpec {
            path: config.fonts.ui.path.clone(),
            size: ((config.fonts.ui.size as f64) * mul).round().max(1.0) as u32,
            options: config.fonts.ui.options.clone(),
            ..Default::default()
        };
        load(&spec, ctx).unwrap_or(ui)
    };
    let h1 = load_heading(1.75, &mut ctx);
    let h2 = load_heading(1.45, &mut ctx);
    let h3 = load_heading(1.2, &mut ctx);

    let (icon, big, icon_big, seti) = if is_single_file() {
        (ui, ui, ui, ui)
    } else {
        let icon = load(&config.fonts.icon, &mut ctx)?;
        let big = if config.fonts.big.path.is_some() {
            load(&config.fonts.big, &mut ctx)?
        } else {
            let big_spec = crate::editor::config::FontSpec {
                path: config.fonts.ui.path.clone(),
                size: config.fonts.big.size,
                options: config.fonts.ui.options.clone(),
                ..Default::default()
            };
            load(&big_spec, &mut ctx)?
        };
        let icon_big = {
            let spec = crate::editor::config::FontSpec {
                path: config.fonts.icon.path.clone(),
                size: config.fonts.icon_big.size,
                options: config.fonts.icon.options.clone(),
                ..Default::default()
            };
            load(&spec, &mut ctx)?
        };
        // Load the Seti icon font for file-type icons in the sidebar.
        let seti = {
            let seti_path = config
                .fonts
                .icon
                .path
                .as_deref()
                .map(|p| {
                    let dir = std::path::Path::new(p)
                        .parent()
                        .unwrap_or(std::path::Path::new("."));
                    dir.join("seti.ttf").to_string_lossy().to_string()
                })
                .unwrap_or_default();
            if std::path::Path::new(&seti_path).exists() {
                let spec = crate::editor::config::FontSpec {
                    path: Some(seti_path),
                    // Seti glyphs are designed small; scale to 150% of UI font
                    // to match VS Code's rendering and fill the sidebar row.
                    size: (config.fonts.ui.size as f64 * 1.5) as u32,
                    options: crate::editor::config::FontOptions {
                        antialiasing: Some("grayscale".into()),
                        hinting: Some("full".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                load(&spec, &mut ctx).unwrap_or(icon)
            } else {
                icon
            }
        };
        (icon, big, icon_big, seti)
    };

    FONT_SLOTS.with(|s| *s.borrow_mut() = Some((ui, code, icon, big, icon_big, seti, h1, h2, h3)));

    Ok(ctx)
}

use std::cell::RefCell;

/// (ui, code, icon, big, icon_big, seti, h1, h2, h3) font slot ids.
type FontSlotIds = (u64, u64, u64, u64, u64, u64, u64, u64, u64);

thread_local! {
    static FONT_SLOTS: RefCell<Option<FontSlotIds>> = const { RefCell::new(None) };
}

/// Build a StyleContext from NativeConfig and loaded fonts.
fn build_style(
    config: &NativeConfig,
    ctx: &crate::editor::draw_context::NativeDrawContext,
) -> StyleContext {
    use crate::editor::types::Color;
    use crate::editor::view::DrawContext as _;

    let (ui, code, icon, big, icon_big, seti, h1, h2, h3) =
        FONT_SLOTS.with(|s| s.borrow().unwrap_or((0, 0, 0, 0, 0, 0, 0, 0, 0)));

    StyleContext {
        font: ui,
        code_font: code,
        icon_font: icon,
        icon_big_font: icon_big,
        big_font: big,
        seti_font: seti,
        h1_font: h1,
        h2_font: h2,
        h3_font: h3,
        font_height: ctx.font_height(ui),
        code_font_height: ctx.font_height(code),
        h1_font_height: ctx.font_height(h1),
        h2_font_height: ctx.font_height(h2),
        h3_font_height: ctx.font_height(h3),
        padding_x: config.ui.padding_x as f64,
        padding_y: config.ui.padding_y as f64,
        divider_size: config.ui.divider_size as f64,
        scrollbar_size: config.ui.scrollbar_size as f64,
        caret_width: config.ui.caret_width as f64,
        tab_width: config.ui.tab_width as f64,
        scale: 1.0,
        background: Color::new(40, 42, 54, 255),
        background2: Color::new(34, 36, 46, 255),
        background3: Color::new(48, 50, 62, 255),
        text: Color::new(215, 218, 224, 255),
        caret: Color::new(147, 161, 255, 255),
        accent: Color::new(97, 175, 239, 255),
        dim: Color::new(114, 120, 138, 255),
        divider: Color::new(24, 26, 34, 255),
        selection: Color::new(72, 79, 100, 255),
        line_number: Color::new(82, 88, 106, 255),
        line_number2: Color::new(147, 161, 255, 255),
        line_highlight: Color::new(44, 47, 59, 255),
        scrollbar: Color::new(72, 79, 100, 255),
        scrollbar2: Color::new(97, 175, 239, 255),
        good: Color::new(80, 200, 120, 255),
        warn: Color::new(255, 212, 121, 255),
        error: Color::new(255, 95, 86, 255),
        nagbar: Color::new(64, 64, 64, 255),
        nagbar_text: Color::new(255, 255, 255, 255),
        nagbar_dim: Color::new(0, 0, 0, 115),
        ..Default::default()
    }
}

}
#[cfg(all(test, feature = "sdl"))]
mod diagnostic_hover_tests {
    use super::{diagnostic_hover_intent_rect, point_in_rect};
    use crate::editor::types::Rect;

    #[test]
    fn intent_region_bridges_diagnostic_to_popup() {
        let popup = Rect {
            x: 100.0,
            y: 50.0,
            w: 240.0,
            h: 100.0,
        };
        let diagnostic = Rect {
            x: 120.0,
            y: 170.0,
            w: 10.0,
            h: 20.0,
        };
        let region = diagnostic_hover_intent_rect(popup, diagnostic, 8.0);
        assert!(point_in_rect(region, 125.0, 160.0));
        assert!(point_in_rect(region, 300.0, 80.0));
        assert!(!point_in_rect(region, 80.0, 210.0));
    }
}
#[cfg(all(test, feature = "sdl"))]
mod lsp_code_action_tests {
    use super::{
        apply_lsp_text_edits, apply_lsp_workspace_edit, code_action_command,
        code_action_disabled_reason, code_action_picker_row_at_width, collect_lsp_code_actions,
        path_to_uri,
    };
    use crate::editor::style_ctx::StyleContext;

    fn buffer_with(text: &str) -> crate::editor::buffer::BufferState {
        let mut state = crate::editor::buffer::default_buffer_state();
        state.lines = text.split_inclusive('\n').map(str::to_string).collect();
        if state.lines.is_empty() {
            state.lines.push(String::new());
        }
        state
    }

    #[test]
    fn whole_document_edit_replaces_the_final_line() {
        // The end position a server sends for a full-document format of text
        // that ends with a newline: line == line count, character 0.
        let mut state = buffer_with("for n in 0..2 {\n        println!(\"{}\", n)\n}\n");
        let edits = vec![serde_json::json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 3, "character": 0 }
            },
            "newText": "for n in 0..2 {\n    println!(\"{}\", n)\n}\n"
        })];
        assert!(apply_lsp_text_edits(&mut state, &edits));
        assert_eq!(
            state.lines.concat(),
            "for n in 0..2 {\n    println!(\"{}\", n)\n}\n"
        );
    }

    #[test]
    fn multiple_edits_apply_from_the_end_backwards() {
        let mut state = buffer_with("alpha\nbeta\ngamma\n");
        let edits = vec![
            serde_json::json!({
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 5 }
                },
                "newText": "one"
            }),
            serde_json::json!({
                "range": {
                    "start": { "line": 2, "character": 0 },
                    "end": { "line": 2, "character": 5 }
                },
                "newText": "three"
            }),
        ];
        assert!(apply_lsp_text_edits(&mut state, &edits));
        assert_eq!(state.lines.concat(), "one\nbeta\nthree\n");
    }

    #[test]
    fn overlapping_edits_from_the_same_start_do_not_abort() {
        // A server must send disjoint ranges. One that does not (a duplicated
        // or half-written reply) once made the shorter range read past the end
        // of the text the longer one had already shortened.
        let mut state = buffer_with("abcdef\n");
        let edits = vec![
            serde_json::json!({
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 6 }
                },
                "newText": ""
            }),
            serde_json::json!({
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 3 }
                },
                "newText": ""
            }),
        ];
        assert!(apply_lsp_text_edits(&mut state, &edits));
        assert_eq!(state.lines.concat(), "\n");
    }

    #[test]
    fn partially_overlapping_edits_keep_the_later_one() {
        let mut state = buffer_with("alpha beta gamma\n");
        let edits = vec![
            serde_json::json!({
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 10 }
                },
                "newText": "X"
            }),
            serde_json::json!({
                "range": {
                    "start": { "line": 0, "character": 6 },
                    "end": { "line": 0, "character": 16 }
                },
                "newText": "Y"
            }),
        ];
        assert!(apply_lsp_text_edits(&mut state, &edits));
        assert_eq!(state.lines.concat(), "alpha Y\n");
    }

    #[test]
    fn out_of_range_positions_do_not_abort() {
        let mut state = buffer_with("alpha\nbeta\n");
        let edits = vec![
            serde_json::json!({
                "range": {
                    "start": { "line": 4294967295u32, "character": 4294967295u32 },
                    "end": { "line": 4294967295u32, "character": 4294967295u32 }
                },
                "newText": "tail"
            }),
            serde_json::json!({
                "range": {
                    "start": { "line": -1, "character": -5 },
                    "end": { "line": 0, "character": 5 }
                },
                "newText": "ALPHA"
            }),
        ];
        assert!(apply_lsp_text_edits(&mut state, &edits));
        assert_eq!(state.lines.concat(), "ALPHA\nbeta\ntail");
    }

    #[test]
    fn edits_that_all_overlap_keep_only_the_last_positioned_one() {
        let mut state = buffer_with("alpha\n");
        let edits = vec![
            serde_json::json!({
                "range": {
                    "start": { "line": 0, "character": 1 },
                    "end": { "line": 0, "character": 4 }
                },
                "newText": "X"
            }),
            serde_json::json!({
                "range": {
                    "start": { "line": 0, "character": 1 },
                    "end": { "line": 0, "character": 3 }
                },
                "newText": "Y"
            }),
            serde_json::json!({
                "range": {
                    "start": { "line": 0, "character": 2 },
                    "end": { "line": 0, "character": 5 }
                },
                "newText": "Z"
            }),
        ];
        // Splicing runs last-position-first, so the edit furthest into the
        // document wins and the two that reach into it are dropped rather than
        // taking the editor down.
        assert!(apply_lsp_text_edits(&mut state, &edits));
        assert_eq!(state.lines.concat(), "alZ\n");
    }

    fn temp_source(name: &str, source: &str) -> String {
        let unique = format!(
            "lite-anvil-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, source).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn applies_gossamer_style_quickfix_workspace_edit() {
        let path = temp_source(
            "gossamer-action.gos",
            "fn helper() { }\nfn main() { helpre() }\n",
        );
        let uri = path_to_uri(&path);
        let edit = serde_json::json!({
            "changes": {
                uri.clone(): [{
                    "range": {
                        "start": { "line": 1, "character": 12 },
                        "end": { "line": 1, "character": 18 }
                    },
                    "newText": "helper"
                }]
            }
        });
        let mut docs = Vec::new();
        assert_eq!(apply_lsp_workspace_edit(&edit, &mut docs, false, false), 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn helper() { }\nfn main() { helper() }\n"
        );
        drop(docs);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn applies_text_document_edit_form() {
        let path = temp_source("document-change.cs", "class C { int value; }\n");
        let uri = path_to_uri(&path);
        let edit = serde_json::json!({
            "documentChanges": [{
                "textDocument": { "uri": uri, "version": 1 },
                "edits": [{
                    "range": {
                        "start": { "line": 0, "character": 14 },
                        "end": { "line": 0, "character": 19 }
                    },
                    "newText": "count"
                }]
            }]
        });
        let mut docs = Vec::new();
        assert_eq!(apply_lsp_workspace_edit(&edit, &mut docs, false, false), 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "class C { int count; }\n"
        );
        drop(docs);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reads_command_and_disabled_action_shapes() {
        let command = serde_json::json!({
            "title": "Run fix",
            "command": { "command": "fix.run", "arguments": [1] }
        });
        assert_eq!(
            code_action_command(&command),
            Some(("fix.run".to_string(), serde_json::json!([1])))
        );
        let disabled = serde_json::json!({
            "title": "Unavailable fix",
            "disabled": { "reason": "Project is still loading" }
        });
        assert_eq!(
            code_action_disabled_reason(&disabled),
            Some("Project is still loading")
        );
    }

    #[test]
    fn collects_server_confirmed_actions_with_preferred_first() {
        let result = serde_json::json!([
            { "title": "Second choice", "kind": "quickfix" },
            { "title": "Preferred fix", "kind": "quickfix", "isPreferred": true },
            { "kind": "quickfix" }
        ]);
        let actions = collect_lsp_code_actions(Some(&result));
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].0, "Preferred fix");
        assert_eq!(actions[1].0, "Second choice");
    }

    #[test]
    fn code_action_rows_are_clickable_with_the_rendered_scroll_offset() {
        let style = StyleContext {
            padding_y: 8.0,
            divider_size: 1.0,
            font_height: 16.0,
            ..StyleContext::default()
        };
        // At 1000px wide, the picker spans x=250..750. The first action row
        // begins at y=49 and is 24px tall.
        assert_eq!(
            code_action_picker_row_at_width(300.0, 50.0, 1000.0, &style, 20, 0),
            Some(0)
        );
        assert_eq!(
            code_action_picker_row_at_width(300.0, 74.0, 1000.0, &style, 20, 15),
            Some(2)
        );
        assert_eq!(
            code_action_picker_row_at_width(100.0, 50.0, 1000.0, &style, 20, 0),
            None
        );
    }
}

#[cfg(all(test, feature = "sdl"))]
mod lsp_state_switch_tests {
    use std::collections::HashMap;

    use super::{LspState, activate_lsp_state};

    #[test]
    fn restores_cached_language_even_when_current_server_exited() {
        let mut active = LspState::new();
        active.filetype = "language-a".to_string();
        active.root_uri = "file:///workspace/a".to_string();
        active.transport_id = None;

        let mut cached = LspState::new();
        cached.filetype = "language-b".to_string();
        cached.root_uri = "file:///workspace/b".to_string();
        cached.initialized = true;
        cached.next_request_id = 42;

        let mut inactive = HashMap::from([(
            ("language-b".to_string(), "file:///workspace/b".to_string()),
            cached,
        )]);
        activate_lsp_state(
            &mut active,
            &mut inactive,
            "language-b",
            "file:///workspace/b".to_string(),
        );

        assert_eq!(active.filetype, "language-b");
        assert!(active.initialized);
        assert_eq!(active.next_request_id, 42);
        assert!(
            inactive.contains_key(&("language-a".to_string(), "file:///workspace/a".to_string()))
        );
    }

    #[test]
    fn activation_keeps_respawn_backoff_when_server_never_spawns() {
        let mut active = LspState::new();
        let mut inactive = HashMap::new();

        activate_lsp_state(
            &mut active,
            &mut inactive,
            "language-a",
            "file:///workspace/a".to_string(),
        );
        active.note_spawn_failure();
        assert!(!active.should_attempt_spawn());

        // The caller re-checks the desired pair every frame; an activated state
        // already carries it, so no second swap discards the backoff.
        assert_eq!(active.filetype, "language-a");
        assert_eq!(active.root_uri, "file:///workspace/a");

        activate_lsp_state(
            &mut active,
            &mut inactive,
            "language-a",
            "file:///workspace/a".to_string(),
        );
        assert_eq!(active.respawn_failures, 1);
        assert!(!active.should_attempt_spawn());
    }
}

#[cfg(all(test, feature = "sdl"))]
mod indent_tests {
    use super::{carried_indent, newline_indent, smart_backspace_span, smart_indent_opens_block};

    #[test]
    fn plain_text_newline_does_not_auto_indent() {
        assert_eq!(
            newline_indent("    notes:\n", "    notes:", "soft", 4, false),
            ""
        );
        assert_eq!(carried_indent("\tindented note\n", false), "");
    }

    #[test]
    fn language_newline_keeps_smart_indent() {
        assert_eq!(
            newline_indent("    if ready:\n", "    if ready:", "soft", 4, true),
            "        "
        );
        assert_eq!(
            newline_indent("\tif ready {\n", "\tif ready {", "hard", 4, true),
            "\t\t"
        );
        assert_eq!(carried_indent("    value\n", true), "    ");
    }

    #[test]
    fn smart_indent_opens_block_python_colon() {
        assert!(smart_indent_opens_block("for a in sys.argv:"));
        assert!(smart_indent_opens_block("if x > 0:"));
        assert!(smart_indent_opens_block("def foo():"));
    }

    #[test]
    fn smart_indent_opens_block_braces_brackets() {
        assert!(smart_indent_opens_block("fn main() {"));
        assert!(smart_indent_opens_block("let xs = ["));
        assert!(smart_indent_opens_block("println!("));
    }

    #[test]
    fn smart_indent_opens_block_trailing_whitespace() {
        assert!(smart_indent_opens_block("fn main() {   "));
    }

    #[test]
    fn smart_indent_opens_block_ignores_line_comment() {
        assert!(smart_indent_opens_block("if x: # comment"));
        assert!(smart_indent_opens_block("fn() { // comment"));
    }

    #[test]
    fn smart_indent_opens_block_negatives() {
        assert!(!smart_indent_opens_block("print(a)"));
        assert!(!smart_indent_opens_block("let x = 1"));
        assert!(!smart_indent_opens_block(""));
        assert!(!smart_indent_opens_block("    "));
    }

    #[test]
    fn smart_backspace_full_indent_unit() {
        // Four spaces then cursor at col 5 -> remove all 4.
        assert_eq!(smart_backspace_span("    ", 5, "soft", 4), Some(4));
    }

    #[test]
    fn smart_backspace_aligns_to_boundary() {
        // Six spaces, cursor at col 7 -> remove 2 to reach 4-space boundary.
        assert_eq!(smart_backspace_span("      ", 7, "soft", 4), Some(2));
    }

    #[test]
    fn smart_backspace_deeper_indent() {
        // Eight spaces at col 9 -> remove 4 (one level).
        assert_eq!(smart_backspace_span("        ", 9, "soft", 4), Some(4));
    }

    #[test]
    fn smart_backspace_two_space_doc() {
        assert_eq!(smart_backspace_span("  ", 3, "soft", 2), Some(2));
        assert_eq!(smart_backspace_span("    ", 5, "soft", 2), Some(2));
    }

    #[test]
    fn smart_backspace_skips_when_text_before() {
        assert_eq!(smart_backspace_span("    a", 6, "soft", 4), None);
        assert_eq!(smart_backspace_span("foo", 4, "soft", 4), None);
    }

    #[test]
    fn smart_backspace_skips_for_hard_tabs() {
        assert_eq!(smart_backspace_span("    ", 5, "hard", 4), None);
    }

    #[test]
    fn smart_backspace_skips_single_space() {
        // One space alone should fall through to normal backspace (no jump).
        assert_eq!(smart_backspace_span(" ", 2, "soft", 4), None);
    }

    #[test]
    fn smart_backspace_skips_col_one() {
        assert_eq!(smart_backspace_span("    ", 1, "soft", 4), None);
    }
}

#[cfg(all(test, feature = "sdl"))]
mod clipboard_tests {
    use super::{append_clipboard_line, insert_clipboard_line};

    #[test]
    fn append_clipboard_line_appends_plain_text() {
        let mut buf = String::from("foo");
        append_clipboard_line(&mut buf, "bar");
        assert_eq!(buf, "foobar");
    }

    #[test]
    fn append_clipboard_line_strips_line_breaks() {
        let mut buf = String::new();
        append_clipboard_line(&mut buf, "a\r\nb\nc");
        assert_eq!(buf, "abc");
    }

    #[test]
    fn append_clipboard_line_keeps_tabs_and_unicode() {
        let mut buf = String::new();
        append_clipboard_line(&mut buf, "a\tπ");
        assert_eq!(buf, "a\tπ");
    }

    #[test]
    fn insert_clipboard_line_inserts_at_caret_and_returns_offset() {
        let mut buf = String::from("ac");
        let caret = insert_clipboard_line(&mut buf, 1, "b");
        assert_eq!(buf, "abc");
        assert_eq!(caret, 2);
    }

    #[test]
    fn insert_clipboard_line_advances_caret_past_multibyte() {
        let mut buf = String::from("xy");
        let caret = insert_clipboard_line(&mut buf, 1, "é");
        assert_eq!(buf, "xéy");
        // 'é' is two bytes in UTF-8, so the caret moves from 1 to 3.
        assert_eq!(caret, 3);
    }

    #[test]
    fn insert_clipboard_line_strips_line_breaks_before_inserting() {
        let mut buf = String::from("[]");
        let caret = insert_clipboard_line(&mut buf, 1, "a\nb");
        assert_eq!(buf, "[ab]");
        assert_eq!(caret, 3);
    }
}

#[cfg(all(test, feature = "sdl"))]
mod paste_insert_tests {
    use super::insert_text_at_caret;
    use crate::editor::buffer::default_buffer_state;

    #[test]
    fn inserts_single_line_at_caret_and_advances() {
        let mut b = default_buffer_state();
        b.lines = vec!["hello\n".to_string()];
        b.selections = vec![1, 3, 1, 3];
        insert_text_at_caret(&mut b, "XY");
        assert_eq!(b.lines, vec!["heXYllo\n".to_string()]);
        assert_eq!(b.selections, vec![1, 5, 1, 5]);
    }

    #[test]
    fn inserts_multi_line_and_splits_the_row() {
        let mut b = default_buffer_state();
        b.lines = vec!["hello\n".to_string()];
        b.selections = vec![1, 3, 1, 3];
        insert_text_at_caret(&mut b, "A\nB");
        assert_eq!(b.lines, vec!["heA\n".to_string(), "Bllo\n".to_string()]);
        assert_eq!(b.selections, vec![2, 2, 2, 2]);
    }

    #[test]
    fn replaces_the_active_selection() {
        let mut b = default_buffer_state();
        b.lines = vec!["hello\n".to_string()];
        b.selections = vec![1, 1, 1, 6];
        insert_text_at_caret(&mut b, "bye");
        assert_eq!(b.lines, vec!["bye\n".to_string()]);
        assert_eq!(b.selections, vec![1, 4, 1, 4]);
    }
}
