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
use std::time::Instant;

use crossbeam_channel::Receiver;
use notify::{Event, RecursiveMode, Watcher};

// Module-level editor mode. Set once at startup, read by helper functions.
thread_local! {
    static SINGLE_FILE_MODE: Cell<bool> = const { Cell::new(false) };
}

/// Whether the editor is running in single-file mode (Nano-Anvil).
fn is_single_file() -> bool {
    SINGLE_FILE_MODE.with(|c| c.get())
}

/// Whether git integration is active (inverse of single-file mode).
fn use_git() -> bool {
    !is_single_file()
}

use crate::editor::buffer;
use crate::editor::config::NativeConfig;
use crate::editor::context_menu::{ContextMenu, MenuItem};
use crate::editor::doc_view::{
    build_render_lines, click_to_doc_pos, syntax_color, DocView, RenderLine, SYNTAX_COLORS,
};
use crate::editor::empty_view::EmptyView;
use crate::editor::event::{EditorEvent, MouseButton};
use crate::editor::keymap::NativeKeymap;
use crate::editor::lsp;
use crate::editor::lsp_client::*;
use crate::editor::picker;
use crate::editor::status_view::{StatusItem, StatusView};
use crate::editor::storage;
use crate::editor::style_ctx::StyleContext;
use crate::editor::terminal_panel::*;
use crate::editor::tokenizer::{self, CompiledSyntax};
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
            entries[a].name.to_lowercase().cmp(&entries[b].name.to_lowercase())
        }),
        1 => indices.sort_by(|&a, &b| {
            entries[b].name.to_lowercase().cmp(&entries[a].name.to_lowercase())
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
                if sort_mode == 2 { tb.cmp(&ta) } else { ta.cmp(&tb) }
            });
        }
        _ => {}
    }
    indices
}

/// Wrapper around `scan_directory` that, in notes-mode, flattens to a
/// `*.md`-only top-level list (no folders, no recursion).
fn scan_for_sidebar(
    notes_mode: bool,
    dir: &str,
    show_hidden: bool,
) -> Vec<SidebarEntry> {
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
        let _ = storage::clear(userdir, "project_session", Some(&format!("{session_key}_expanded")));
    } else {
        let _ = storage::save_text(
            userdir,
            "project_session",
            &format!("{session_key}_expanded"),
            &serde_json::to_string(&expanded).unwrap_or_default(),
        );
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
        let mut changed = Vec::new();
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
                                changed.push(s.to_string());
                            }
                        }
                    }
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
        }.to_string(),
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
        check_file_size_limit, doc_is_modified, nag_msg_close, nag_msg_quit, open_file_into,
        project_session_key, restore_project_session, save_project_session,
        scroll_new_doc_to_line, split_path_line, OpenDoc, BG_LOAD_THRESHOLD,
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
            if let Ok(session) = serde_json::from_str::<crate::editor::open_doc::SessionData>(&data) {
                for (i, file) in session.files.iter().enumerate() {
                    if file == "__untitled__" {
                        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
                        if let Some(content) = session.unsaved_content.get(i) {
                            if !content.is_empty() {
                                let _ = buffer::with_buffer_mut(buf_id, |b| {
                                    b.lines =
                                        content.lines().map(|l| format!("{l}\n")).collect();
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
                            dirty_cache: std::cell::Cell::new(None),
                            token_cache: std::cell::RefCell::new(
                                crate::editor::open_doc::TokenCache::default(),
                            ),
            preview: crate::editor::markdown_preview::MarkdownPreviewState::default(),
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
            dirty_cache: std::cell::Cell::new(None),
            token_cache: std::cell::RefCell::new(
                crate::editor::open_doc::TokenCache::default(),
            ),
            preview: crate::editor::markdown_preview::MarkdownPreviewState::default(),
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
    let mut editor_mouse_down = false;
    // Local shift-key tracker. SDL's mouse events don't carry modifier state,
    // so tracking it from keyboard events directly by key name makes shift+click
    // robust against any SDL_GetModState quirks on different platforms/WMs.
    let mut shift_held = false;
    let mut tab_dragging: Option<usize> = None;
    // Dropdown menu shown when the tab bar overflows — lists every open tab
    // so they stay reachable even when their labels don't fit on screen.
    let mut tab_dropdown_open: bool = false;
    // Tab targeted by the most recent right-click; consumed by the
    // tab:close / close-left / close-right / close-all dispatch in the
    // context-menu click handler.
    let mut tab_menu_target: Option<usize> = None;
    let mut mouse_x: f64 = 0.0;
    let mut mouse_y: f64 = 0.0;
    let mut sidebar_entries: Vec<SidebarEntry>;
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
        scan_for_sidebar(subsystems.has_notes_mode(), &project_root, sidebar_show_hidden)
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
    let frame_interval = 1.0 / fps;
    // Deferred render-line cache: written at the top of the next frame to
    // avoid borrow-checker conflicts with the immutable doc borrow during
    // rendering. Includes the tab index so we write to the correct doc even
    // if the user switched tabs between frames.
    // Tuple layout: (tab_idx, buffer_id, lines, change_id, scroll_y). The
    // `buffer_id` is the only stable identity for the doc being rendered —
    // `tab_idx` aliases once the docs list is swapped (e.g. Open Recent
    // replaces the project), so the consumer uses `buffer_id` to skip
    // applying a render captured against a now-defunct doc.
    type PendingRenderCache =
        Option<(usize, u64, std::sync::Arc<Vec<RenderLine>>, i64, f64)>;
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
    let mut find_matches: Vec<(usize, usize, usize)> = Vec::new();
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
        ReloadFromDisk {
            path: String,
        },
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
        OverwriteFile {
            save_path: String,
            doc_tab: usize,
        },
        /// "No extension detected, save anyway?" — Save As target has no
        /// trailing `.ext`. Yes proceeds (and still checks for overwrite
        /// next); No returns to the picker so the user can add one.
        NoExtension {
            save_path: String,
            doc_tab: usize,
        },
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
                !c.0.contains("tab")
                    && c.0 != "root:close-all"
                    && c.0 != "root:close-all-others"
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
    use crate::editor::cmdview::{
        dir_with_trailing_sep, effective_root, path_suggest, refresh_cmdview_suggestions,
        remember_recent_file, update_recent, CmdViewMode,
    };
    #[cfg(feature = "sdl")]
    use crate::editor::cmdview::truncate_left_to_width;
    let mut cmdview_active = false;
    let mut cmdview_mode = CmdViewMode::OpenFile;
    let mut cmdview_text = String::new();
    // Byte position of the input caret within cmdview_text. Always lands on a UTF-8 boundary.
    let mut cmdview_cursor: usize = 0;
    let mut cmdview_suggestions: Vec<String> = Vec::new();
    let mut cmdview_selected: usize = 0;
    let mut cmdview_label = String::new();

    // Project-wide search state.
    // Git status view.
    let mut git_status_active = false;
    let mut git_status_entries: Vec<(String, String, String)> = Vec::new();
    let mut git_status_selected: usize = 0;

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
    let mut project_replace_selected: usize = 0;

    /// Run grep across the project, returning (path, line_number, line_text) tuples.
    fn run_project_search(
        query: &str,
        root: &str,
        use_regex: bool,
        whole_word: bool,
        case_insensitive: bool,
    ) -> Vec<(String, usize, String)> {
        if query.len() < 2 {
            return Vec::new();
        }
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

    /// Execute project-wide find-and-replace using sed. Returns the number of
    /// files modified.
    fn execute_project_replace(root: &str, search: &str, replace: &str) -> usize {
        if search.is_empty() {
            return 0;
        }
        // Find matching files first.
        let grep_out = std::process::Command::new("grep")
            .args([
                "-rl",
                "-i",
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
                search,
                root,
            ])
            .output();
        let Ok(out) = grep_out else { return 0 };
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let files: Vec<&str> = stdout.lines().collect();
        if files.is_empty() {
            return 0;
        }
        // Escape sed special characters in search and replace.
        let sed_escape = |s: &str| -> String {
            s.replace('\\', "\\\\")
                .replace('/', "\\/")
                .replace('&', "\\&")
                .replace('\n', "\\n")
        };
        let sed_search = sed_escape(search);
        let sed_replace = sed_escape(replace);
        let sed_expr = format!("s/{sed_search}/{sed_replace}/gi");
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
    // Sidebar entry targeted by the current context menu (path, is_dir).
    // Set when right-clicking a sidebar row; consumed by the rename flow.
    let mut sidebar_menu_target: Option<(String, bool)> = None;
    // Path being renamed (source). Read by the CmdViewMode::Rename
    // confirm handler to `fs::rename` the file.
    let mut rename_source: String = String::new();

    // LSP completion, hover, and go-to-definition state.
    let mut completion = CompletionState::new();
    let mut hover = HoverState::new();

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
    let lsp_specs = if subsystems.has_lsp() {
        lsp::builtin_specs()
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
        let root = find_project_root(
            Path::new(file_path)
                .parent()
                .map(|p| p.to_str().unwrap_or("."))
                .unwrap_or("."),
            &spec.root_patterns,
        );
        let Some(root_dir) = root else { return };
        let cmd: Vec<String> = spec
            .command
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if cmd.is_empty() {
            return;
        }
        match lsp::spawn_transport(&cmd, &root_dir, &[]) {
            Ok(tid) => {
                lsp_state.transport_id = Some(tid);
                lsp_state.root_uri = path_to_uri(&root_dir);
                lsp_state.filetype = filetype.to_string();
                let req_id = lsp_state.next_id();
                lsp_state
                    .pending_requests
                    .insert(req_id, "initialize".to_string());
                let _ =
                    lsp::send_message(tid, &lsp_initialize_request(req_id, &lsp_state.root_uri));
            }
            Err(e) => {
                log_to_file(userdir, &format!("Failed to spawn LSP: {e}"));
                if verbose {
                    eprintln!("Failed to spawn LSP: {e}");
                }
            }
        }
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
        if !dropped_caches_for_idle
            && last_activity.elapsed().as_secs() >= IDLE_DROP_SECS
        {
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
        while let Some(event) = crate::window::poll_event_native() {
            // Any event counts as activity for idle-drop tracking.
            last_activity = Instant::now();
            dropped_caches_for_idle = false;
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
                    redraw = true;
                }
                EditorEvent::Shown => {
                    window_hidden = false;
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
                        match key.as_str() {
                            "backspace" => {
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
                                            if let Some(doc) = docs.get_mut(active_tab) {
                                                let marker = comment_marker_for_path(
                                                    &doc.path,
                                                    &syntax_index,
                                                );
                                                handle_doc_command(
                                                    &mut doc.view,
                                                    &cmd,
                                                    &style,
                                                    &doc.indent_type,
                                                    doc.indent_size,
                                                    marker.as_ref(),
                                                    false,
                                                    line_wrapping,
                                                );
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
                        redraw = true;
                    }

                    // Command view (file/folder open) intercepts keys — but
                    // only while no nag is active. When a modal nag (overwrite,
                    // create-dir, reload-from-disk) is up the cmdview stays on
                    // screen but its keypress arm must step aside so Y / N /
                    // Enter can reach the nag handler below.
                    if cmdview_active
                        && matches!(nag, Nag::None)
                        && (subsystems.has_picker() || cmdview_mode == CmdViewMode::SaveAs || cmdview_mode == CmdViewMode::OpenFile || cmdview_mode == CmdViewMode::OpenRecent || cmdview_mode == CmdViewMode::Rename) {
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
                                                    if sz > BG_LOAD_THRESHOLD && load_job.is_none() {
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
                                        }
                                    }
                                    CmdViewMode::SaveAs => {
                                        // Save current document to the chosen path.
                                        let save_path = if p.is_dir() {
                                            // User selected a directory -- stay in cmdview.
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
                                        remember_recent_file(&mut recent_files, &path, userdir_path);
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
                            "down"
                                if !project_search_results.is_empty() => {
                                    project_search_selected = (project_search_selected + 1)
                                        .min(project_search_results.len() - 1);
                                }
                            "backspace" => {
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
                                    let count = execute_project_replace(
                                        &project_root,
                                        &project_replace_search,
                                        &project_replace_with,
                                    );
                                    project_replace_active = false;
                                    info_message = Some((
                                        format!("Replaced {count} occurrences across project"),
                                        Instant::now(),
                                    ));
                                    // Reload any open files that may have changed.
                                    for doc in &mut docs {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            if !doc.path.is_empty() {
                                                let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                    let mut fresh = buffer::default_buffer_state();
                                                    if buffer::load_file(&mut fresh, &doc.path)
                                                        .is_ok()
                                                    {
                                                        b.lines = fresh.lines;
                                                        b.change_id += 1;
                                                    }
                                                    Ok(())
                                                });
                                            }
                                        }
                                    }
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
                                    project_replace_with.pop();
                                } else {
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
                                        remember_recent_file(&mut recent_files, &full_path, userdir_path);
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
                            "down"
                                if !git_status_entries.is_empty() => {
                                    git_status_selected =
                                        (git_status_selected + 1).min(git_status_entries.len() - 1);
                                }
                            "r" | "R" => {
                                git_status_entries = run_git_status(&project_root);
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
                            "down"
                                if !git_log_entries.is_empty() => {
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
                                .max(1.0))
                                as usize;
                            if let Some(inst) = terminal.terminals.get_mut(terminal.active)
                            {
                                inst.sel_start = Some((0, 0));
                                inst.sel_end =
                                    Some((rows_visible.saturating_sub(1), usize::MAX));
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
                                if let (Some(s), Some(e)) = (inst.sel_start, inst.sel_end)
                                {
                                    if let Some((a, b)) =
                                        crate::editor::terminal_panel::normalized_selection(
                                            s, e,
                                        )
                                    {
                                        let cap = inst.tbuf.history_len() as f64;
                                        let scrollback_rows = inst
                                            .scrollback
                                            .round()
                                            .max(0.0)
                                            .min(cap)
                                            as usize;
                                        // Recompute rows_visible from current geometry.
                                        let (_, wh, _, _) =
                                            crate::window::get_window_size();
                                        let win_h = wh as f64;
                                        let status_h_c =
                                            style.font_height + style.padding_y * 2.0;
                                        let tab_h_c =
                                            if !single_file_mode && !docs.is_empty() {
                                                style.font_height + style.padding_y * 3.0
                                            } else {
                                                0.0
                                            };
                                        let terminal_h_c = (win_h * 0.3)
                                            .min(win_h - tab_h_c - status_h_c - 50.0)
                                            .max(80.0);
                                        let tab_bar_h_c = if !terminal.terminals.is_empty()
                                        {
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
                                            doc.saved_signature = buffer::with_buffer(
                                                buf_id,
                                                |b| Ok(buffer::content_signature(&b.lines)),
                                            )
                                            .unwrap_or(0);
                                            doc.path = save_path.clone();
                                            doc.name = std::path::Path::new(&save_path)
                                                .file_name()
                                                .map(|n| n.to_string_lossy().to_string())
                                                .unwrap_or_else(|| save_path.clone());
                                            autoreload.watch(&save_path);
                                            log_to_file(
                                                userdir,
                                                &format!("Saved {save_path}"),
                                            );
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
                                            doc.saved_signature = buffer::with_buffer(
                                                buf_id,
                                                |b| Ok(buffer::content_signature(&b.lines)),
                                            )
                                            .unwrap_or(0);
                                            doc.path = save_path.clone();
                                            doc.name = std::path::Path::new(&save_path)
                                                .file_name()
                                                .map(|n| n.to_string_lossy().to_string())
                                                .unwrap_or_else(|| save_path.clone());
                                            autoreload.watch(&save_path);
                                            log_to_file(
                                                userdir,
                                                &format!("Saved {save_path}"),
                                            );
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
                                        format!(
                                            "Could not create directory {parent_str}"
                                        ),
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
                                            doc.saved_signature = buffer::with_buffer(
                                                buf_id,
                                                |b| Ok(buffer::content_signature(&b.lines)),
                                            )
                                            .unwrap_or(0);
                                            if is_save_as {
                                                doc.path = save_path.clone();
                                                doc.name = std::path::Path::new(&save_path)
                                                    .file_name()
                                                    .map(|n| n.to_string_lossy().to_string())
                                                    .unwrap_or_else(|| save_path.clone());
                                            }
                                            autoreload.watch(&save_path);
                                            log_to_file(
                                                userdir,
                                                &format!("Saved {save_path}"),
                                            );
                                            info_message = Some((
                                                format!("Saved {}", doc.name),
                                                Instant::now(),
                                            ));
                                            if !is_save_as && subsystems.has_git() {
                                                doc.git_changes =
                                                    crate::editor::git::diff_file(&save_path);
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
                                                b.change_id =
                                                    b.change_id.wrapping_add(1).max(1);
                                            }
                                            Ok(())
                                        });
                                        doc.cached_change_id = -1;
                                        doc.cached_render = std::sync::Arc::new(Vec::new());
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
                                            remember_recent_file(&mut recent_files, &cmd, userdir_path);
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
                        match key.as_str() {
                            "escape" => {
                                find_active = false;
                                replace_active = false;
                                find_focus_on_replace = false;
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
                            "return" | "keypad enter" if mods.ctrl && replace_active => {
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
                                    replace_query.pop();
                                } else {
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
                                redraw = true;
                                continue;
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
                                if let Ok(palette) = crate::editor::style::load_theme_palette(
                                    &tp.to_string_lossy(),
                                ) {
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
                                let active_doc_path = docs
                                    .get(active_tab)
                                    .map(|d| d.path.as_str())
                                    .unwrap_or("");
                                let cwd =
                                    crate::editor::terminal_panel::resolve_terminal_cwd(
                                        active_doc_path,
                                        &project_root,
                                    );
                                if terminal.spawn(&cwd) {
                                    let n = terminal.terminals.len();
                                    let cd_payload =
                                        crate::editor::terminal_panel::terminal_cd_payload(
                                            &cwd,
                                        );
                                    if let Some(t) = terminal.active_terminal() {
                                        t.title =
                                            crate::editor::terminal_panel::terminal_title(
                                                n, &cwd,
                                            );
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
                            let active_doc_path = docs
                                .get(active_tab)
                                .map(|d| d.path.as_str())
                                .unwrap_or("");
                            let cwd = crate::editor::terminal_panel::resolve_terminal_cwd(
                                active_doc_path,
                                &project_root,
                            );
                            let ok = terminal.spawn(&cwd);
                            if ok {
                                let n = terminal.terminals.len();
                                let cd_payload =
                                    crate::editor::terminal_panel::terminal_cd_payload(
                                        &cwd,
                                    );
                                if let Some(t) = terminal.active_terminal() {
                                    t.title =
                                        crate::editor::terminal_panel::terminal_title(
                                            n, &cwd,
                                        );
                                    let _ = t.inner.write(cd_payload.as_bytes());
                                }
                            }
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
                    if cmdview_active && (subsystems.has_picker() || cmdview_mode == CmdViewMode::SaveAs || cmdview_mode == CmdViewMode::OpenFile || cmdview_mode == CmdViewMode::OpenRecent || cmdview_mode == CmdViewMode::Rename) {
                        let prev_text = cmdview_text.clone();
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
                            project_replace_with.push_str(text);
                        } else {
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
                            replace_query.push_str(text);
                        } else {
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
                    if let Some(doc) = docs.get_mut(active_tab) {
                        let dv = &mut doc.view;
                        if let Some(buf_id) = dv.buffer_id {
                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                let is_single_char = text.chars().count() == 1;
                                let has_sel = b.selections.len() >= 4
                                    && (b.selections[0] != b.selections[2]
                                        || b.selections[1] != b.selections[3]);
                                if is_single_char && !has_sel {
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
                            // Mark LSP change for debounced didChange (only for LSP-handled files).
                            if lsp_state.transport_id.is_some() && lsp_state.initialized {
                                if let Some(doc) = docs.get(active_tab) {
                                    let ext = doc.path.rsplit('.').next().unwrap_or("");
                                    if !doc.path.is_empty() && ext_to_lsp_filetype(ext).is_some() {
                                        lsp_state.last_change = Some(Instant::now());
                                        lsp_state.pending_change_uri =
                                            Some(path_to_uri(&doc.path));
                                        lsp_state.pending_change_version += 1;
                                    }
                                }
                            }
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
                                        }
                                    }
                                }
                            }
                        }
                    }
                    redraw = true;
                }
                EditorEvent::MousePressed {
                    button,
                    x,
                    y,
                    modifiers,
                    ..
                } => {
                    cursor_blink_reset = Instant::now();
                    // Any mouse click cancels pending scroll animation so the
                    // view never jumps unexpectedly.
                    if let Some(doc) = docs.get_mut(active_tab) {
                        doc.view.target_scroll_y = doc.view.scroll_y;
                    }
                    // Nag bar button click handling.
                    if let Nag::UnsavedChanges { message, tab_to_close } = &nag {
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
                                w + draw_ctx.font_width(style.font, info)
                                    + style.padding_x * 3.0
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
                                    if cmd == "sidebar:rename" {
                                        if let Some((path, _is_dir)) =
                                            sidebar_menu_target.take()
                                        {
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
                                    if cmd.starts_with("test:") {
                                        let cmd: String = cmd;
                                        include!("commands_dispatch.rs");
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
                                                    if target < total { vec![target] } else { vec![] }
                                                }
                                                "tab:close-right" => {
                                                    ((target + 1)..total).rev().collect()
                                                }
                                                "tab:close-left" => {
                                                    (0..target).rev().collect()
                                                }
                                                "tab:close-all" => {
                                                    (0..total).rev().collect()
                                                }
                                                _ => vec![],
                                            };
                                            // If any targeted doc is modified, nag
                                            // on the first modified one and skip
                                            // the rest — matches the close-button
                                            // safety net so we don't silently drop
                                            // unsaved buffers in a batch op.
                                            let first_mod = indices
                                                .iter()
                                                .rev()
                                                .copied()
                                                .find(|&i| {
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
                                                    }
                                                    docs.remove(i);
                                                }
                                                if active_tab >= docs.len()
                                                    && !docs.is_empty()
                                                {
                                                    active_tab = docs.len() - 1;
                                                } else if docs.is_empty() {
                                                    active_tab = 0;
                                                } else if cmd == "tab:close-left" {
                                                    // The active tab's index
                                                    // shifted by the number of
                                                    // docs removed from the left.
                                                    active_tab =
                                                        active_tab.saturating_sub(target);
                                                }
                                            }
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    if let Some(doc) = docs.get_mut(active_tab) {
                                        let marker =
                                            comment_marker_for_path(&doc.path, &syntax_index);
                                        handle_doc_command(
                                            &mut doc.view,
                                            &cmd,
                                            &style,
                                            &doc.indent_type,
                                            doc.indent_size,
                                            marker.as_ref(),
                                            false,
                                            line_wrapping,
                                        );
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
                            let sidebar_w_tab = if subsystems.has_sidebar()
                                && sidebar_visible
                            {
                                sidebar_width
                            } else {
                                0.0
                            };
                            let (ww_tr, wh_tr, _, _) = crate::window::get_window_size();
                            let win_w_tr = ww_tr as f64;
                            let win_h_tr = wh_tr as f64;
                            let close_btn_w = draw_ctx.font_width(style.icon_font, "C")
                                + style.padding_x;
                            let dropdown_btn_w =
                                (style.font_height + style.padding_x * 2.0).ceil();
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
                                // Estimate the menu size and clamp its origin so
                                // it never renders off-screen. The context menu's
                                // draw_native sizes itself to the widest label.
                                let item_h = style.font_height + style.padding_y;
                                let menu_h =
                                    item_h * items.len() as f64 + style.padding_y;
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
                        if subsystems.has_sidebar()
                            && sidebar_visible
                            && *x < sidebar_w_rc
                        {
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
                            let disp_idx = ((*y
                                - sidebar_toolbar_h_rc
                                - sidebar_dir_header_h
                                - notes_ui_h_rc
                                + sidebar_scroll)
                                / entry_h)
                                .floor() as i64;
                            let click_idx: i64 = if disp_idx >= 0
                                && (disp_idx as usize) < notes_display_rc.len()
                            {
                                notes_display_rc[disp_idx as usize] as i64
                            } else {
                                -1
                            };
                            if click_idx >= 0
                                && (click_idx as usize) < sidebar_entries.len()
                            {
                                let entry = &sidebar_entries[click_idx as usize];
                                // Only offer rename on regular files for now --
                                // renaming directories is handled by the OS
                                // recursively but would require more UX care
                                // (open tab paths, expanded-folder bookkeeping).
                                if !entry.is_dir {
                                    sidebar_menu_target =
                                        Some((entry.path.clone(), entry.is_dir));
                                    let items = vec![MenuItem {
                                        text: "Rename".into(),
                                        info: None,
                                        command: Some("sidebar:rename".into()),
                                        separator: false,
                                    }];
                                    context_menu.show(*x, *y, items);
                                    redraw = true;
                                    continue;
                                }
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
                        let active_doc_path =
                            docs.get(active_tab).map(|d| d.path.as_str()).unwrap_or("");
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
                        context_menu.show(*x, *y, items);
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
                            let thumb_h = (sidebar_sb_h * ratio)
                                .max(min_thumb)
                                .min(sidebar_sb_h);
                            let max_scroll =
                                (sidebar_content_h - sidebar_sb_h).max(1.0);
                            let scroll_frac =
                                (sidebar_scroll / max_scroll).clamp(0.0, 1.0);
                            let thumb_y = sidebar_sb_top
                                + scroll_frac * (sidebar_sb_h - thumb_h);
                            if *y >= thumb_y && *y < thumb_y + thumb_h {
                                sidebar_sb_drag_offset = *y - thumb_y;
                            } else {
                                sidebar_sb_drag_offset = thumb_h / 2.0;
                                let new_top = (*y - thumb_h / 2.0).clamp(
                                    sidebar_sb_top,
                                    sidebar_sb_top + sidebar_sb_h - thumb_h,
                                );
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

                    // Sidebar click detection.
                    if subsystems.has_sidebar() && sidebar_visible && *x < sidebar_w {
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
                                    notes_sort_mode =
                                        if notes_sort_mode == 0 { 1 } else { 0 };
                                } else {
                                    // Recent: toggle between newest-first (2)
                                    // and oldest-first (3).
                                    notes_sort_mode =
                                        if notes_sort_mode == 2 { 3 } else { 2 };
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
                        let notes_display_click: Vec<usize> = if subsystems.has_notes_mode() {
                            compute_notes_display_order(
                                &sidebar_entries,
                                &notes_search,
                                notes_sort_mode,
                            )
                        } else {
                            (0..sidebar_entries.len()).collect()
                        };
                        let disp_click_idx = ((*y
                            - sidebar_toolbar_h
                            - sidebar_dir_header_h
                            - notes_ui_h
                            + sidebar_scroll)
                            / entry_h)
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
                                    if open_file_into(&entry_path, &mut docs, use_git()) {
                                        autoreload.watch(&entry_path);
                                        active_tab = docs.len() - 1;
                                        remember_recent_file(&mut recent_files, &entry_path, userdir_path);
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
                    let tab_h = if !docs.is_empty() {
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
                        let dropdown_btn_w =
                            (style.font_height + style.padding_x * 2.0).ceil();
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
                                    draw_ctx.font_width(style.font, &label)
                                        + style.padding_x * 3.0,
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
                        let dropdown_btn_w =
                            (style.font_height + style.padding_x * 2.0).ceil();

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
                                        nag = Nag::UnsavedChanges { message: nag_msg_close(&doc.name), tab_to_close: Some(i) };
                                    } else {
                                        autoreload.unwatch(&doc.path);
                                        docs.remove(i);
                                        if active_tab >= docs.len() && !docs.is_empty() {
                                            active_tab = docs.len() - 1;
                                        }
                                    }
                                    clicked_close = true;
                                } else {
                                    active_tab = i;
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
                                let label_w = draw_ctx
                                    .font_width(style.font, &terminal.terminals[i].title);
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
                            let ty_start = term_y_click
                                + style.divider_size
                                + tab_bar_h_click
                                + 2.0;
                            let visible_h = (term_y_click + terminal_h_click
                                - ty_start
                                - style.padding_y)
                                .max(0.0);
                            let rows_visible =
                                (visible_h / char_h_v).floor().max(1.0) as usize;
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
                                    let col = (((*x - term_x_click - style.padding_x)
                                        / char_w_v)
                                        .floor() as i64)
                                        .max(0) as usize;
                                    let vis_row = (((*y - ty_start) / char_h_v)
                                        .floor() as i64)
                                        .max(0) as usize;
                                    if let Some(inst) =
                                        terminal.terminals.get_mut(terminal.active)
                                    {
                                        inst.sel_start = Some((vis_row, col));
                                        inst.sel_end = Some((vis_row, col));
                                        inst.sel_dragging = true;
                                    }
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
                                // Checkbox first.
                                let cb = doc
                                    .preview
                                    .checkbox_regions
                                    .iter()
                                    .find(|r| *x >= r.x1 && *x <= r.x2 && *y >= r.y1 && *y <= r.y2)
                                    .cloned();
                                if let Some(cb) = cb {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let src = buffer::with_buffer(buf_id, |b| {
                                            Ok(b.lines.join(""))
                                        })
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
                                .and_then(|id| {
                                    buffer::with_buffer(id, |b| Ok(b.lines.len())).ok()
                                })
                                .unwrap_or(1) as f64;
                            let total_h = total_lines * line_h_sb;
                            if total_h > dv_rect.h {
                                let ratio = dv_rect.h / total_h;
                                let min_thumb = style.scrollbar_size * 2.0;
                                let thumb_h =
                                    (dv_rect.h * ratio).max(min_thumb).min(dv_rect.h);
                                let scroll_frac =
                                    doc.view.scroll_y / (total_h - dv_rect.h).max(1.0);
                                let thumb_y =
                                    dv_rect.y + scroll_frac * (dv_rect.h - thumb_h);
                                if *y >= thumb_y && *y < thumb_y + thumb_h {
                                    editor_sb_drag_offset = *y - thumb_y;
                                } else {
                                    editor_sb_drag_offset = thumb_h / 2.0;
                                    let new_top =
                                        (*y - thumb_h / 2.0).clamp(
                                            dv_rect.y,
                                            dv_rect.y + dv_rect.h - thumb_h,
                                        );
                                    let new_frac =
                                        (new_top - dv_rect.y) / (dv_rect.h - thumb_h);
                                    let new_scroll =
                                        (new_frac * (total_h - dv_rect.h)).max(0.0);
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
                        let ty_start = term_y_sc
                            + style.divider_size
                            + tab_bar_h_sc
                            + 2.0;
                        let visible_h = (term_y_sc + terminal_h_sc
                            - ty_start
                            - style.padding_y)
                            .max(0.0);
                        let rows_visible =
                            (visible_h / char_h_sc).floor().max(1.0) as usize;
                        let sb_w_sc = style.scrollbar_size.max(6.0);
                        let sb_x_sc = term_x_sc + term_w_sc - sb_w_sc;
                        let sb_h_sc = char_h_sc * rows_visible as f64;
                        if *x >= sb_x_sc
                            && *x < sb_x_sc + sb_w_sc
                            && *y >= ty_start
                            && *y < ty_start + sb_h_sc
                        {
                            if let Some(inst) =
                                terminal.terminals.get_mut(terminal.active)
                            {
                                let cap = inst.tbuf.history_len() as f64;
                                if cap > 0.0 && sb_h_sc > 0.0 {
                                    let total = cap + rows_visible as f64;
                                    let ratio = (rows_visible as f64 / total)
                                        .clamp(0.0, 1.0);
                                    let min_thumb = sb_w_sc * 2.0;
                                    let thumb_h =
                                        (sb_h_sc * ratio).max(min_thumb).min(sb_h_sc);
                                    let pos_from_top =
                                        (cap - inst.scrollback_target) / cap;
                                    let thumb_y =
                                        ty_start + pos_from_top * (sb_h_sc - thumb_h);
                                    if *y >= thumb_y && *y < thumb_y + thumb_h {
                                        terminal_sb_drag_offset = *y - thumb_y;
                                    } else {
                                        terminal_sb_drag_offset = thumb_h / 2.0;
                                        let new_top = (*y - thumb_h / 2.0).clamp(
                                            ty_start,
                                            ty_start + sb_h_sc - thumb_h,
                                        );
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
                        let hit = test_badges.iter().find(|r| {
                            *x >= r.x1 && *x < r.x2 && *y >= r.y1 && *y < r.y2
                        });
                        if let Some(region) = hit {
                            if let Some(test) = active_tests.get(region.test_index) {
                                let doc_path = docs
                                    .get(active_tab)
                                    .map(|d| d.path.clone())
                                    .unwrap_or_default();
                                if !doc_path.is_empty() {
                                    pending_single_test =
                                        Some((doc_path, test.name.clone()));
                                    {
                                        let cmd: String =
                                            "test:run-single".to_string();
                                        include!("commands_dispatch.rs");
                                    }
                                }
                            }
                            redraw = true;
                            continue;
                        }
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
                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                let line = click_line.min(b.lines.len()).max(1);
                                let max_col =
                                    char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                                let col = click_col.min(max_col);
                                if extending && b.selections.len() >= 4 {
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
                                w + draw_ctx.font_width(style.font, info)
                                    + style.padding_x * 3.0
                            } else {
                                w + style.padding_x * 2.0
                            };
                            menu_w = menu_w.max(total_w);
                        }
                        menu_w = menu_w.max(120.0);
                        let menu_h = item_h * context_menu.items.len() as f64
                            + style.padding_y;
                        let new_sel = if *x >= menu_x
                            && *x <= menu_x + menu_w
                            && *y >= menu_y
                            && *y <= menu_y + menu_h
                        {
                            let rel = (*y - menu_y - style.padding_y / 2.0) / item_h;
                            let idx = rel.floor().max(0.0) as usize;
                            if idx < context_menu.items.len()
                                && !context_menu.items[idx].separator
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
                            let dropdown_btn_w =
                                (style.font_height + style.padding_x * 2.0).ceil();
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
                    // Editor scrollbar drag: move the thumb so its grabbed
                    // point stays under the cursor, then derive scroll.
                    if editor_sb_dragging {
                        if let Some(doc) = docs.get_mut(active_tab) {
                            let dv_rect = doc.view.rect();
                            let line_h_sb = style.code_font_height * 1.2;
                            let total_lines = doc
                                .view
                                .buffer_id
                                .and_then(|id| {
                                    buffer::with_buffer(id, |b| Ok(b.lines.len())).ok()
                                })
                                .unwrap_or(1)
                                as f64;
                            let total_h = total_lines * line_h_sb;
                            if total_h > dv_rect.h && dv_rect.h > 0.0 {
                                let ratio = dv_rect.h / total_h;
                                let min_thumb = style.scrollbar_size * 2.0;
                                let thumb_h =
                                    (dv_rect.h * ratio).max(min_thumb).min(dv_rect.h);
                                let new_top = (*y - editor_sb_drag_offset).clamp(
                                    dv_rect.y,
                                    dv_rect.y + dv_rect.h - thumb_h,
                                );
                                let travel = (dv_rect.h - thumb_h).max(1.0);
                                let new_frac = (new_top - dv_rect.y) / travel;
                                let new_scroll =
                                    (new_frac * (total_h - dv_rect.h)).max(0.0);
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
                        let ty_start = term_y_sm
                            + style.divider_size
                            + tab_bar_h_sm
                            + 2.0;
                        let visible_h = (term_y_sm + terminal_h_sm
                            - ty_start
                            - style.padding_y)
                            .max(0.0);
                        let rows_visible =
                            (visible_h / char_h_sm).floor().max(1.0) as usize;
                        let sb_h = char_h_sm * rows_visible as f64;
                        let sb_w_sm = style.scrollbar_size.max(6.0);
                        if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                            let cap = inst.tbuf.history_len() as f64;
                            if cap > 0.0 && sb_h > 0.0 {
                                let total = cap + rows_visible as f64;
                                let ratio =
                                    (rows_visible as f64 / total).clamp(0.0, 1.0);
                                let min_thumb = sb_w_sm * 2.0;
                                let thumb_h =
                                    (sb_h * ratio).max(min_thumb).min(sb_h);
                                let new_top = (*y - terminal_sb_drag_offset).clamp(
                                    ty_start,
                                    ty_start + sb_h - thumb_h,
                                );
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
                        let ty_start = term_y_m
                            + style.divider_size
                            + tab_bar_h_m
                            + 2.0;
                        let visible_h = (term_y_m + terminal_h_m
                            - ty_start
                            - style.padding_y)
                            .max(0.0);
                        let rows_visible =
                            (visible_h / char_h_m).floor().max(1.0) as usize;
                        if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                            if inst.sel_dragging && char_w_m > 0.0 {
                                let col = ((*x - term_x_m - style.padding_x)
                                    / char_w_m)
                                    .floor()
                                    .max(0.0) as usize;
                                let vis_row = (((*y - ty_start) / char_h_m)
                                    .floor()
                                    .max(0.0) as usize)
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
                            let thumb_h = (sidebar_sb_h * ratio)
                                .max(min_thumb)
                                .min(sidebar_sb_h);
                            let new_top = (*y - sidebar_sb_drag_offset).clamp(
                                sidebar_sb_top,
                                sidebar_sb_top + sidebar_sb_h - thumb_h,
                            );
                            let travel = (sidebar_sb_h - thumb_h).max(1.0);
                            let new_frac = (new_top - sidebar_sb_top) / travel;
                            let max_scroll =
                                (sidebar_content_h - sidebar_sb_h).max(1.0);
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
                    let hover_link = docs
                        .get(active_tab)
                        .map(|d| {
                            d.preview.enabled
                                && d.preview.link_regions.iter().any(|r| {
                                    *x >= r.x1 && *x <= r.x2 && *y >= r.y1 && *y <= r.y2
                                })
                        })
                        .unwrap_or(false);
                    if hover_link {
                        crate::window::set_cursor("hand");
                    } else if subsystems.has_sidebar()
                        && sidebar_visible
                        && (*x - sidebar_w).abs() < 5.0
                    {
                        crate::window::set_cursor("sizeh");
                    } else if !sidebar_dragging && !editor_mouse_down {
                        crate::window::set_cursor("arrow");
                    } else if editor_mouse_down {
                        crate::window::set_cursor("ibeam");
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
                    editor_mouse_down = false;
                    tab_dragging = None;
                    editor_sb_dragging = false;
                    terminal_sb_dragging = false;
                    sidebar_sb_dragging = false;
                    // End terminal selection drag; the selection itself
                    // stays visible until dismissed by another click or
                    // the escape / copy key.
                    if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                        inst.sel_dragging = false;
                    }
                    redraw = true;
                    continue;
                }
                EditorEvent::MouseWheel { y, .. } => {
                    let line_h = style.code_font_height * 1.2;
                    let scroll_amt = y * line_h * 3.0;
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
                        let max_scroll =
                            (sidebar_content_h - sidebar_sb_h).max(0.0);
                        sidebar_scroll = (sidebar_scroll - scroll_amt)
                            .clamp(0.0, max_scroll);
                    } else if let Some(doc) = docs.get_mut(active_tab) {
                        // Route wheel to whichever pane the cursor is over
                        // in split preview mode.
                        let over_preview = doc.preview.enabled
                            && doc.preview.rect.w > 0.0
                            && mouse_x >= doc.preview.rect.x
                            && mouse_x < doc.preview.rect.x + doc.preview.rect.w;
                        if over_preview {
                            doc.preview.target_scroll_y =
                                (doc.preview.target_scroll_y - scroll_amt).max(0.0);
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

        // LSP: auto-start for the active file if no transport is running.
        if subsystems.has_lsp() && lsp_state.transport_id.is_none()
            && let Some(doc) = docs.get(active_tab)
            && !doc.path.is_empty()
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
            let finished = job
                .handle
                .as_ref()
                .map(|h| h.is_finished())
                .unwrap_or(true);
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
                            dirty_cache: std::cell::Cell::new(None),
                            token_cache: std::cell::RefCell::new(
                                crate::editor::open_doc::TokenCache::default(),
                            ),
            preview: crate::editor::markdown_preview::MarkdownPreviewState::default(),
                        });
                        active_tab = docs.len() - 1;
                        autoreload.watch(&j.path);
                        remember_recent_file(&mut recent_files, &j.path, userdir_path);
                    }
                    Ok(Err(e)) => {
                        info_message = Some((format!("Load failed: {e}"), Instant::now()));
                    }
                    Err(_) => {
                        info_message =
                            Some(("Load thread panicked".to_string(), Instant::now()));
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
                                let already_pending = lsp_state
                                    .pending_request_uris
                                    .values()
                                    .any(|u| u == &uri);
                                if lsp_state.inlay_hints_uri != uri && !already_pending
                                {
                                    let line_count = doc
                                        .view
                                        .buffer_id
                                        .and_then(|id| {
                                            buffer::with_buffer(id, |b| Ok(b.lines.len()))
                                                .ok()
                                        })
                                        .unwrap_or(100);
                                    let req_id = lsp_state.next_id();
                                    lsp_state.pending_requests.insert(
                                        req_id,
                                        "textDocument/inlayHint".to_string(),
                                    );
                                    lsp_state
                                        .pending_request_uris
                                        .insert(req_id, uri.clone());
                                    let _ = lsp::send_message(
                                        tid,
                                        &lsp_inlay_hint_request(
                                            req_id,
                                            &uri,
                                            0,
                                            line_count,
                                        ),
                                    );
                                    lsp_state.inlay_hints.clear();
                                    lsp_state.inlay_hints_uri = String::new();
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
                                    lsp_state
                                        .pending_request_uris
                                        .insert(req_id, uri.clone());
                                    let _ = lsp::send_message(
                                        tid,
                                        &lsp_inlay_hint_request(req_id, &uri, 0, line_count),
                                    );
                                }
                            }
                        }
                    }
                }
                if let Ok(poll) = lsp::poll_transport(tid, 50) {
                    for msg in &poll.messages {
                        // Handle initialize response.
                        if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("initialize")
                            {
                                lsp_state.pending_requests.remove(&id);
                                lsp_state.initialized = true;
                                // Send initialized notification.
                                let _ = lsp::send_message(
                                    tid,
                                    &serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "method": "initialized",
                                        "params": {}
                                    }),
                                );
                                // Send didOpen only for files matching the LSP filetype.
                                for doc in &docs {
                                    if doc.path.is_empty() {
                                        continue;
                                    }
                                    let ext = doc.path.rsplit('.').next().unwrap_or("");
                                    let Some(ft) = ext_to_lsp_filetype(ext) else {
                                        continue;
                                    };
                                    if ft != lsp_state.filetype {
                                        continue;
                                    }
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let text =
                                            buffer::with_buffer(buf_id, |b| Ok(b.lines.join("")))
                                                .unwrap_or_default();
                                        let uri = path_to_uri(&doc.path);
                                        let _ = lsp::send_message(
                                            tid,
                                            &lsp_did_open(&uri, &lsp_state.filetype, &text),
                                        );
                                    }
                                }
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
                                        lsp_state
                                            .pending_request_uris
                                            .insert(req_id, uri.clone());
                                        let _ = lsp::send_message(
                                            tid,
                                            &lsp_inlay_hint_request(req_id, &uri, 0, line_count),
                                        );
                                    }
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
                                    let old_count = lsp_state.inlay_hints.len();
                                    let new_count = new_hints.len();
                                    if new_hints.is_empty() {
                                        if lsp_state.inlay_retry_count < 20 {
                                            lsp_state.inlay_retry_at = Some(
                                                Instant::now()
                                                    + std::time::Duration::from_secs(2),
                                            );
                                            lsp_state.inlay_retry_count += 1;
                                        }
                                    } else {
                                        let uri_changed =
                                            lsp_state.inlay_hints_uri != req_uri;
                                        let changed =
                                            uri_changed || old_count != new_count;
                                        lsp_state.inlay_hints = new_hints;
                                        lsp_state.inlay_hints_uri = req_uri.clone();
                                        lsp_state.inlay_retry_count = 0;
                                        lsp_state.inlay_retry_at = None;
                                        if changed {
                                            pending_render_cache = None;
                                            for d in &mut docs {
                                                d.cached_change_id = -1;
                                            }
                                            crate::window::force_invalidate();
                                        }
                                    }
                                    redraw = true;
                                }
                            }

                            // Handle completion response.
                            if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                                == Some("textDocument/completion")
                            {
                                lsp_state.pending_requests.remove(&id);
                                let mut items = Vec::new();
                                let result = msg.get("result");
                                // result can be an array or {items: [...]}.
                                let item_arr = result
                                    .and_then(|r| {
                                        r.as_array().cloned().or_else(|| {
                                            r.get("items").and_then(|v| v.as_array()).cloned()
                                        })
                                    })
                                    .unwrap_or_default();
                                for item in item_arr.iter().take(20) {
                                    let label = item
                                        .get("label")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
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
                                let contents = msg.get("result").and_then(|r| r.get("contents"));
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
                                            remember_recent_file(&mut recent_files, &target_path, userdir_path);
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
                                            remember_recent_file(&mut recent_files, &target_path, userdir_path);
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
                                        .map(|arr| {
                                            arr.iter()
                                                .map(|d| {
                                                    let range = d.get("range");
                                                    let start = range.and_then(|r| r.get("start"));
                                                    let end = range.and_then(|r| r.get("end"));
                                                    Diagnostic {
                                                        start_line: start
                                                            .and_then(|s| s.get("line"))
                                                            .and_then(|v| v.as_u64())
                                                            .unwrap_or(0)
                                                            as usize,
                                                        start_col: start
                                                            .and_then(|s| s.get("character"))
                                                            .and_then(|v| v.as_u64())
                                                            .unwrap_or(0)
                                                            as usize,
                                                        end_col: end
                                                            .and_then(|s| s.get("character"))
                                                            .and_then(|v| v.as_u64())
                                                            .unwrap_or(0)
                                                            as usize,
                                                        severity: d
                                                            .get("severity")
                                                            .and_then(|v| v.as_u64())
                                                            .unwrap_or(1)
                                                            as u8,
                                                    }
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    lsp_state.diagnostics.insert(path, diags);
                                    redraw = true;
                                }
                            }
                        }
                    }
                    if !poll.running {
                        lsp_state.transport_id = None;
                        lsp_state.initialized = false;
                    }
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
                                let is_lsp_file = ext_to_lsp_filetype(ext)
                                    .map(|ft| ft == lsp_state.filetype)
                                    .unwrap_or(false);
                                if is_lsp_file {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let text =
                                            buffer::with_buffer(buf_id, |b| Ok(b.lines.join("")))
                                                .unwrap_or_default();
                                        let _ = lsp::send_message(
                                            tid,
                                            &lsp_did_change(
                                                &uri,
                                                lsp_state.pending_change_version,
                                                &text,
                                            ),
                                        );
                                        // Re-request inlay hints after change is flushed.
                                        let line_count =
                                            buffer::with_buffer(buf_id, |b| Ok(b.lines.len()))
                                                .unwrap_or(100);
                                        let req_id = lsp_state.next_id();
                                        lsp_state
                                            .pending_requests
                                            .insert(req_id, "textDocument/inlayHint".to_string());
                                        lsp_state
                                            .pending_request_uris
                                            .insert(req_id, uri.clone());
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

        // Terminal: poll output from each pty.
        if subsystems.has_terminal() && terminal.visible {
            let mut dead_indices = Vec::new();
            for (i, inst) in terminal.terminals.iter_mut().enumerate() {
                inst.inner.poll();
                if !inst.inner.running {
                    dead_indices.push(i);
                } else if let Some(data) = inst.inner.read(4096)
                    && !data.is_empty()
                {
                    inst.tbuf.process_output(&data);
                    redraw = true;
                }
            }
            // Remove dead terminals in reverse order.
            for i in dead_indices.into_iter().rev() {
                terminal.terminals[i].inner.cleanup();
                terminal.terminals.remove(i);
                redraw = true;
            }
            if terminal.terminals.is_empty() {
                terminal.visible = false;
                terminal.focused = false;
                terminal.active = 0;
                // Panel just went away -- force a native repaint so the
                // editor content reclaims the vacated strip in the
                // same frame instead of waiting for the next event.
                crate::window::force_invalidate();
            } else if terminal.active >= terminal.terminals.len() {
                terminal.active = terminal.terminals.len() - 1;
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
                    // Split the content area into a 50/50 editor | preview
                    // pane. The editor keeps float rects (its existing
                    // wrap/click math has always tolerated them); the
                    // preview rect is snapped to integer pixels so the
                    // background fill and clip rect enclose every logical
                    // pixel. Without snapping, `draw_rect`'s i32 cast
                    // truncates the bottom of the fill, leaving a stale
                    // pixel row that reads as a thin blue line from a
                    // previously drawn heading rule.
                    let half_w = (content_rect.w * 0.5).floor();
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

            // Autoreload: check for external file changes.
            let changed_paths = autoreload.poll_changed();
            for changed in &changed_paths {
                // Canonicalize to match doc paths.
                let canonical = std::fs::canonicalize(changed)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| changed.clone());
                for doc in docs.iter_mut() {
                    let doc_canon = std::fs::canonicalize(&doc.path)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| doc.path.clone());
                    if doc_canon == canonical {
                        if doc_is_modified(doc) {
                            nag = Nag::ReloadFromDisk { path: doc.path.clone() };
                        } else if let Some(buf_id) = doc.view.buffer_id {
                            let path = doc.path.clone();
                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                let mut buf_state = buffer::default_buffer_state();
                                if buffer::load_file(&mut buf_state, &path).is_ok() {
                                    b.lines = buf_state.lines;
                                    // `default_buffer_state()` resets
                                    // change_id to 1; copying it back
                                    // means a just-opened buffer (also
                                    // at change_id 1) sees no change
                                    // and the doc-view render cache
                                    // hits on the stale lines.  Bump
                                    // past the current value so every
                                    // downstream cache invalidates.
                                    b.change_id = b.change_id.wrapping_add(1).max(1);
                                }
                                Ok(())
                            });
                            // Force the render cache to rebuild on the
                            // next frame rather than waiting for the
                            // change_id comparison to catch the bump
                            // (cheap, and removes any reliance on it).
                            doc.cached_change_id = -1;
                            doc.cached_render = std::sync::Arc::new(Vec::new());
                            // Keep the "saved" markers aligned with
                            // what we just wrote so the next external
                            // change doesn't misfire the Reload prompt.
                            if let Ok((cid, sig)) = buffer::with_buffer(buf_id, |b| {
                                Ok((b.change_id, buffer::content_signature(&b.lines)))
                            }) {
                                doc.saved_change_id = cid;
                                doc.saved_signature = sig;
                            }
                        }
                        redraw = true;
                        break;
                    }
                }
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
            if let Some((tab_idx, rendered_buf_id, lines, cid, sy)) =
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
                        doc_mut.cached_hint_count = lsp_state.inlay_hints.len();
                    }
                }
            }

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
                        if draw_ctx.font_width(style.font, &modified_label)
                            <= filename_max_w
                        {
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
                    let lang = match ext {
                        "rs" => "Rust",
                        "py" => "Python",
                        "js" => "JavaScript",
                        "ts" => "TypeScript",
                        "c" => "C",
                        "h" => "C",
                        "cpp" | "cc" => "C++",
                        "hpp" => "C++",
                        "go" => "Go",
                        "toml" => "TOML",
                        "json" => "JSON",
                        "yaml" | "yml" => "YAML",
                        "md" => "Markdown",
                        "sh" | "bash" => "Shell",
                        "html" => "HTML",
                        "css" => "CSS",
                        "xml" => "XML",
                        "java" => "Java",
                        "rb" => "Ruby",
                        "php" => "PHP",
                        "sql" => "SQL",
                        _ => {
                            if ext.is_empty() {
                                "Plain Text"
                            } else {
                                ext
                            }
                        }
                    };
                    let indent_label = if doc.indent_type == "hard" {
                        "Tabs".to_string()
                    } else {
                        format!("Spaces: {}", doc.indent_size)
                    };
                    let (crlf, huge) = doc
                        .view
                        .buffer_id
                        .and_then(|id| buffer::with_buffer(id, |b| Ok((b.crlf, b.is_huge()))).ok())
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
                        right_parts.push("No Undo".to_string());
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

                // Smooth scroll interpolation. Snap target and current to integer
                // pixels so the lerp can never have a sub-pixel residual that
                // would cause repeated tiny snaps on every redraw — which the
                // user perceives as scroll drifting when they only move the
                // mouse or tap a modifier.
                #[allow(unused_assignments)]
                if let Some(doc) = docs.get_mut(active_tab) {
                    let dv = &mut doc.view;
                    let target = dv.target_scroll_y.round();
                    dv.target_scroll_y = target;
                    let current = dv.scroll_y;
                    let diff = target - current;
                    if diff.abs() >= 1.0 {
                        // Lerp toward target with ease-out, snap to integer pixels.
                        let new_scroll = (current + diff * 0.45).round();
                        if new_scroll != current {
                            dv.scroll_y = new_scroll;
                            redraw = true;
                        } else if current != target {
                            // The lerp step rounded to the same pixel; finish
                            // the convergence in one final hop.
                            dv.scroll_y = target;
                            redraw = true;
                        }
                    } else if current != target {
                        // Within one pixel: finalize at the exact target so
                        // subsequent frames see diff == 0 and do nothing.
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
                let mut tab_overlay_rects: Vec<(f64, f64, String, String)> = Vec::new();
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

                    let close_w =
                        draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                    let dropdown_btn_w =
                        (style.font_height + style.padding_x * 2.0).ceil();

                    // Measure full-width tab bar (no truncation) to decide whether to
                    // enter overflow mode. Reserving the dropdown button space keeps
                    // the decision stable once overflow is on.
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
                            + close_w
                            + style.divider_size;
                    }
                    let tabs_overflow = full_total > avail_full;
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

                    // Cache displayed labels (with truncation when overflowing) and
                    // per-tab rects so the tooltip pass below and the hit-tests can
                    // reuse them without recomputing widths.
                    let mut tab_rects: Vec<(f64, f64, String, String)> =
                        Vec::with_capacity(docs.len());

                    let mut tx = sidebar_w;
                    for (i, doc) in docs.iter().enumerate() {
                        let full_label = if doc_is_modified(doc) {
                            format!("*{}", doc.name)
                        } else {
                            doc.name.clone()
                        };
                        let display_label = if tabs_overflow {
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
                        tab_rects.push((tx, tw, display_label.clone(), full_label.clone()));
                        // Don't draw tabs that fall entirely past the dropdown limit;
                        // they're still reachable via the dropdown menu.
                        if tx >= tabs_right_limit {
                            tx += tw + style.divider_size;
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
                            &display_label,
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
                        tx += tw + style.divider_size;
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
                        let arrow_top =
                            accent_h + (tbh - accent_h - arrow_h) / 2.0;
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
                            if is_alpha { style.accent.to_array() } else { style.dim.to_array() },
                        );
                        draw_ctx.draw_text(
                            style.font,
                            &recent_label,
                            half + (sidebar_w - half - recent_w) / 2.0,
                            text_y,
                            if is_recent { style.accent.to_array() } else { style.dim.to_array() },
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
                                let folder_w =
                                    draw_ctx.font_width(style.icon_font, icon);
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
                                    let glyph_w =
                                        draw_ctx.font_width(icon_font, &glyph);
                                    let icon_x = x + (icon_w - glyph_w) / 2.0;
                                    draw_ctx.draw_text(
                                        icon_font,
                                        &glyph,
                                        icon_x,
                                        icon_y,
                                        fi.color,
                                    );
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
                    // Reset clip to full window for the sidebar edge divider.
                    crate::editor::app_state::clip_init(width, height);

                    // Sidebar scrollbar (lite-xl style): proportional thumb
                    // with a minimum size, drawn just inside the right edge.
                    let total_entries_h = notes_display.len() as f64 * entry_h;
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
                        let thumb_h =
                            (sb_area_h * ratio).max(min_thumb).min(sb_area_h);
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
                        // Compile-on-demand and bump MRU. Evict the LRU
                        // entry once the cache exceeds SYNTAX_CACHE_CAP
                        // so memory doesn't grow unbounded on sessions
                        // that touch many file types.
                        let ext_owned = ext.to_string();
                        compiled_syntax_mru.retain(|e| e != &ext_owned);
                        compiled_syntax_mru.insert(0, ext_owned.clone());
                        while compiled_syntax_mru.len() > SYNTAX_CACHE_CAP {
                            if let Some(drop_ext) = compiled_syntax_mru.pop() {
                                compiled_syntax_cache.remove(&drop_ext);
                            }
                        }
                        let compiled_opt = compiled_syntax_cache
                            .entry(ext_owned)
                            .or_insert_with(|| {
                                let filename = doc.path.rsplit('/').next().unwrap_or(&doc.path);
                                let entry =
                                    crate::editor::syntax::match_syntax_entry(filename, &syntax_index)?;
                                let def = entry.load_full()?;
                                match tokenizer::compile_from_definition(&def) {
                                    Ok(cs) => Some(cs),
                                    Err(e) => {
                                        log_to_file(
                                            userdir,
                                            &format!("Syntax compile error: {e:?}"),
                                        );
                                        None
                                    }
                                }
                            });
                        let wrap_w = if line_wrapping {
                            Some(dv.rect().w - dv.gutter_width - style.padding_x * 2.0)
                        } else {
                            None
                        };
                        let is_lsp_file = ext_to_lsp_filetype(ext)
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
                        // `cached_render` is Arc-shared so the cache-hit
                        // path is a refcount bump rather than a full
                        // `Vec<RenderLine>` clone per redraw.
                        let render_lines: std::sync::Arc<Vec<RenderLine>> =
                            if let Some(doc) = docs.get(active_tab) {
                                if doc.cached_change_id == current_change_id
                                    && (doc.cached_scroll_y - scroll_y_now).abs() < 0.5
                                    && doc.cached_hint_count == hints.len()
                                    && !doc.cached_render.is_empty()
                                {
                                    std::sync::Arc::clone(&doc.cached_render)
                                } else {
                                    std::sync::Arc::new(build_render_lines(
                                        buf_id,
                                        dv,
                                        &style,
                                        ext,
                                        compiled_opt.as_ref(),
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
                                    compiled_opt.as_ref(),
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
                            && crate::editor::test_runner::detect_runner_with_fallback(
                                &project_root,
                                &doc.path,
                            )
                            .is_some()
                        {
                            active_tests = {
                                let text_lines = buffer::with_buffer(buf_id, |b| {
                                    Ok(b.lines
                                        .iter()
                                        .map(|l| l.trim_end_matches('\n').to_string())
                                        .collect::<Vec<_>>())
                                })
                                .unwrap_or_default();
                                crate::editor::test_runner::discover_tests(
                                    &doc.path, &text_lines,
                                )
                            };
                            let line_h = style.code_font_height * 1.2;
                            let dv_rect = dv.rect();
                            // Plain ASCII text so no font has to carry a
                            // triangle glyph; the previous "▶" rendered as
                            // a .notdef box in Lilex and other code fonts
                            // that don't cover U+25B6.
                            let badge_text = "Run test";
                            let badge_w = draw_ctx.font_width(style.font, badge_text)
                                + style.padding_x;
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
                                let row_y = dv_rect.y
                                    + (fn_line as f64 - 1.0) * line_h
                                    - dv.scroll_y;
                                if row_y + line_h < dv_rect.y
                                    || row_y >= dv_rect.y + dv_rect.h
                                {
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
                                test_badges.push(
                                    crate::editor::test_runner::TestBadgeRegion {
                                        x1: badge_x,
                                        y1: row_y,
                                        x2: badge_x + badge_w,
                                        y2: row_y + line_h,
                                        test_index: i,
                                    },
                                );
                            }
                        } else {
                            active_tests.clear();
                        }

                        pending_render_cache = Some((
                            active_tab,
                            buf_id,
                            render_lines,
                            current_change_id,
                            scroll_y_now,
                        ));
                        // Draw bracket match underlines at cursor position.
                        if let Some(buf_id) = dv.buffer_id {
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
                                let y_pos = doc_y + (diag.start_line as f64) * line_h
                                    + line_h
                                    - 2.0
                                    - dv.scroll_y;
                                if y_pos < doc_y || y_pos > doc_y + dv.rect().h {
                                    continue;
                                }
                                use crate::editor::view::DrawContext as _;
                                let char_w = draw_ctx.font_width(style.code_font, "m");
                                let x1 =
                                    doc_x + diag.start_col as f64 * char_w - dv.scroll_x;
                                let x2 =
                                    doc_x + end_col as f64 * char_w - dv.scroll_x;
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

                            // Get compiled syntax for this file.
                            let ext = doc.path.rsplit('.').next().unwrap_or("");
                            let compiled = compiled_syntax_cache.get(ext).and_then(|o| o.as_ref());

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

                                    if let Some(syntax) = compiled {
                                        let toks = tokenizer::tokenize_line(syntax, raw);
                                        let mut x_off = 0.0;
                                        for t in &toks {
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
                    if doc.preview.enabled && doc.preview.rect.w > 0.0 {
                        if let Some(buf_id) = doc.view.buffer_id {
                            // Reparse the source when the buffer changes.
                            let cur_change_id =
                                buffer::with_buffer(buf_id, |b| Ok(b.change_id)).unwrap_or(0);
                            if cur_change_id != doc.preview.cached_change_id {
                                let text = buffer::with_buffer(buf_id, |b| Ok(b.lines.join("")))
                                    .unwrap_or_default();
                                doc.preview.blocks = crate::editor::markdown::parse(&text);
                                doc.preview.cached_change_id = cur_change_id;
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
                                                let entry = crate::editor::syntax::match_syntax_entry(
                                                    &pseudo,
                                                    &syntax_index,
                                                )?;
                                                let def = entry.load_full()?;
                                                tokenizer::compile_from_definition(&def).ok()
                                            })
                                            .as_ref()?;
                                        // Touch MRU so preview-only highlights
                                        // don't immediately get evicted.
                                        compiled_syntax_mru.retain(|e| e != &ext_owned);
                                        compiled_syntax_mru.insert(0, ext_owned);
                                        Some(
                                            code_text
                                                .split('\n')
                                                .map(|line| {
                                                    tokenizer::tokenize_line(compiled_opt, line)
                                                })
                                                .collect(),
                                        )
                                    })
                                    .collect();
                            }
                            let rect = doc.preview.rect;
                            // No smooth scroll: track the target directly so
                            // edits in the source that shrink `content_height`
                            // (and with it `max_scroll`) can't drive a multi-
                            // frame glide, which showed up as the preview
                            // auto-scrolling while the user typed.
                            let max_scroll =
                                (doc.preview.content_height - rect.h).max(0.0);
                            doc.preview.target_scroll_y =
                                doc.preview.target_scroll_y.clamp(0.0, max_scroll);
                            doc.preview.scroll_y = doc.preview.target_scroll_y;
                            // Divider between editor and preview.
                            use crate::editor::view::DrawContext as _;
                            draw_ctx.draw_rect(
                                rect.x,
                                rect.y,
                                style.divider_size.max(1.0),
                                rect.h,
                                style.divider.to_array(),
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
                        draw_ctx.draw_rect(
                            term_x,
                            tby,
                            term_w,
                            tbh,
                            style.background2.to_array(),
                        );
                        let close_w = draw_ctx.font_width(style.icon_font, "C")
                            + style.padding_x;
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
                            draw_ctx.draw_rect(
                                tx,
                                tby + accent_h,
                                tw,
                                tbh - accent_h,
                                bg,
                            );
                            if i == terminal.active {
                                draw_ctx.draw_rect(
                                    tx,
                                    tby,
                                    tw,
                                    accent_h,
                                    style.accent.to_array(),
                                );
                            }
                            let text_y = tby
                                + accent_h
                                + (tbh - accent_h - style.font_height) / 2.0;
                            draw_ctx.draw_text(
                                style.font,
                                label,
                                tx + style.padding_x,
                                text_y,
                                fg,
                            );
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
                                tby
                                    + accent_h
                                    + (tbh
                                        - accent_h
                                        - draw_ctx.font_height(style.icon_font))
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
                        let visible_h =
                            (term_y + terminal_h - ty_start - style.padding_y).max(0.0);
                        let rows_visible =
                            (visible_h / char_h).floor().max(1.0) as usize;

                        let cap = inst.tbuf.history_len() as f64;
                        inst.scrollback_target =
                            inst.scrollback_target.clamp(0.0, cap);
                        let diff = inst.scrollback_target - inst.scrollback;
                        if diff.abs() >= 0.5 {
                            inst.scrollback += diff * 0.35;
                            crate::window::force_invalidate();
                        } else if inst.scrollback != inst.scrollback_target {
                            inst.scrollback = inst.scrollback_target;
                        }
                        let scrollback_rows = inst
                            .scrollback
                            .round()
                            .max(0.0)
                            .min(cap) as usize;
                        let rows_data =
                            inst.tbuf.visible_rows(rows_visible, scrollback_rows);

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
                            Some(
                                rows_visible
                                    - scrollback_rows
                                    + cur_row_1.saturating_sub(1),
                            )
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
                                            || (row_idx == a.0
                                                && row_idx != b.0
                                                && col_idx >= a.1)
                                            || (row_idx == b.0
                                                && row_idx != a.0
                                                && col_idx < b.1)
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
                                    draw_ctx.draw_rect(
                                        cx,
                                        ry,
                                        char_w,
                                        char_h,
                                        [200, 200, 200, 80],
                                    );
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
                            "Replace: {replace_query}{repl_cursor}  (Ctrl+Enter to replace)"
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
                    draw_ctx.draw_rect(
                        dlg_x,
                        dlg_y,
                        dlg_w,
                        dlg_h,
                        style.background3.to_array(),
                    );
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
                    let bytes = job
                        .bytes_read
                        .load(std::sync::atomic::Ordering::Relaxed);
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
                    draw_ctx.draw_rect(
                        bar_x,
                        bar_y,
                        bar_w * pct,
                        bar_h,
                        style.accent.to_array(),
                    );
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
                    let msg = format!(
                        "{save_path} already exists. Overwrite?  [Y]es  [N]o"
                    );
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
                    let msg = format!(
                        "No extension detected ({save_path}). Save anyway?  [Y]es  [N]o"
                    );
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
                    let msg = format!(
                        "File changed on disk: {path}. Reload?  [Y]es  [N]o"
                    );
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
                    let caret_offset_px =
                        draw_ctx.font_width(style.font, before_cursor);
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
                        let popup_w = 350.0_f64.min(width - popup_x - 10.0);
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
                        let tooltip_h =
                            tooltip_line_h * line_count_h as f64 + style.padding_y * 2.0;
                        let tooltip_w = hover_lines
                            .iter()
                            .map(|l| draw_ctx.font_width(style.font, l))
                            .fold(0.0_f64, f64::max)
                            + style.padding_x * 2.0;
                        let tooltip_y = hover_y - tooltip_h;
                        // Background.
                        draw_ctx.draw_rect(
                            hover_x,
                            tooltip_y,
                            tooltip_w,
                            tooltip_h,
                            style.background3.to_array(),
                        );
                        draw_ctx.draw_rect(
                            hover_x,
                            tooltip_y,
                            tooltip_w,
                            style.divider_size,
                            style.divider.to_array(),
                        );
                        for (i, line_text) in hover_lines.iter().enumerate() {
                            draw_ctx.draw_text(
                                style.font,
                                line_text,
                                hover_x + style.padding_x,
                                tooltip_y + style.padding_y + i as f64 * tooltip_line_h,
                                style.text.to_array(),
                            );
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
                        if tab_overlay_overflow {
                            if let (Some(doc), Some((tx_h, tw_h, _, full_label))) =
                                (docs.get(hi), tab_overlay_rects.get(hi))
                            {
                                let path = doc.path.clone();
                                let tip_font = style.font;
                                let name_w = draw_ctx.font_width(tip_font, full_label);
                                let max_tip_w =
                                    (width - sidebar_w - style.padding_x * 2.0).max(80.0);
                                let path_full_w = draw_ctx.font_width(tip_font, &path);
                                let (path_display, path_w) = if path_full_w
                                    + style.padding_x * 2.0
                                    <= max_tip_w
                                {
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

                redraw = false;
            }
        }

        if quit {
            break;
        }

        // Sleep until next event or frame interval.
        crate::window::wait_event(Some(frame_interval));
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
        let project_root_meaningful =
            !project_root.is_empty() && project_root != "." && std::path::Path::new(&project_root).is_dir();
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

    // Shut down LSP transport.
    if let Some(tid) = lsp_state.transport_id {
        lsp::terminate_transport(tid);
        lsp::remove_transport(tid);
    }

    false
}

#[cfg(not(feature = "sdl"))]
pub fn run(_config: NativeConfig, _args: &[String], _datadir: &str, _userdir: &str) -> bool {
    false
}

sdl_only! {
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
    let flags = crate::editor::regex::CompileFlags {
        caseless: case_insensitive,
        ..Default::default()
    };
    crate::editor::regex::NativeRegex::compile(&pat, flags).ok()
}

/// Scan the document and return every match as (line, col, end_col). All values
/// are 1-based. Multi-line matches are not supported — each line is searched
/// independently, matching the single-line cursor model.
fn compute_find_matches(
    dv: &DocView,
    query: &str,
    use_regex: bool,
    whole_word: bool,
    case_insensitive: bool,
) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let Some(re) = build_find_regex(query, use_regex, whole_word, case_insensitive) else {
        return out;
    };
    let Some(buf_id) = dv.buffer_id else {
        return out;
    };
    let _ = buffer::with_buffer(buf_id, |b| {
        for (i, raw) in b.lines.iter().enumerate() {
            let line = raw.trim_end_matches('\n');
            let bytes = line.as_bytes();
            for m in re.find_iter(bytes, 0) {
                let Ok(m) = m else { break };
                let (s, e) = m.span();
                if e <= s {
                    continue;
                }
                let col = line[..s].chars().count() + 1;
                let end_col = col + line[s..e].chars().count();
                out.push((i + 1, col, end_col));
            }
        }
        Ok(())
    });
    out
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
) -> Vec<(usize, usize, usize)> {
    let all = compute_find_matches(dv, query, use_regex, whole_word, case_insensitive);
    let Some((sl, sc, el, ec)) = selection else {
        return all;
    };
    all.into_iter()
        .filter(|&(line, col, end_col)| {
            if line < sl || line > el {
                return false;
            }
            if line == sl && col < sc {
                return false;
            }
            if line == el && end_col > ec {
                return false;
            }
            true
        })
        .collect()
}

/// Index of the first match at or after (line, col). Wraps to 0 if nothing
/// later exists. Returns None only for an empty match list.
fn find_match_at_or_after(
    matches: &[(usize, usize, usize)],
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
fn find_match_before(matches: &[(usize, usize, usize)], line: usize, col: usize) -> Option<usize> {
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
fn select_find_match(dv: &mut DocView, m: (usize, usize, usize), replace_active: bool) {
    let (line, col, end_col) = m;
    let Some(buf_id) = dv.buffer_id else { return };
    let _ = buffer::with_buffer_mut(buf_id, |b| {
        b.selections = vec![line, col, line, end_col];
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
    auto_scroll: bool,
    line_wrapping: bool,
) {
    let Some(buf_id) = dv.buffer_id else { return };
    let line_h = style.code_font_height * 1.2;

    let mut prev_cursor_line: usize = 0;
    let _ = buffer::with_buffer_mut(buf_id, |b| {
        let anchor_line = *b.selections.first().unwrap_or(&1);
        let anchor_col = *b.selections.get(1).unwrap_or(&1);
        let cursor_line = *b.selections.get(2).unwrap_or(&anchor_line);
        let cursor_col = *b.selections.get(3).unwrap_or(&anchor_col);
        prev_cursor_line = cursor_line;
        let line_count = b.lines.len();

        // Selection: shift variants move cursor but keep anchor.
        let is_select = cmd.starts_with("doc:select-to-");

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
            "doc:cut" | "doc:copy" => {
                // TODO: clipboard integration
            }
            "doc:paste" => {
                // TODO: clipboard integration
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
                        if ccol > 1 && cline <= b.lines.len() {
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
                if col > 1 {
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
                // Capture leading whitespace for auto-indent.
                let indent: String = b.lines[line - 1]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let l = &mut b.lines[line - 1];
                let byte_pos = char_to_byte(l, col - 1);
                let rest = l[byte_pos..].to_string();
                l.truncate(byte_pos);
                l.push('\n');
                let new_line = format!("{indent}{rest}");
                let new_col = indent.len() + 1;
                b.lines.insert(line, new_line);
                line += 1;
                col = new_col;
            }
            "doc:newline-below" => {
                buffer::push_undo(b);
                let indent: String = b.lines[line - 1]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let new_line = format!("{indent}\n");
                let new_col = indent.len() + 1;
                b.lines.insert(line, new_line);
                line += 1;
                col = new_col;
            }
            "doc:newline-above" => {
                buffer::push_undo(b);
                let indent: String = b.lines[line - 1]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
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
                let l = &mut b.lines[line - 1];
                let byte_pos = char_to_byte(l, col - 1);
                l.insert_str(byte_pos, &indent_str);
                col += indent_str.chars().count();
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
            _ => {}
        }

        // Collapse to single cursor when a non-create-cursor command runs.
        if buffer::cursor_count(b) > 1 {
            buffer::remove_extra_cursors(b);
        }

        // Update selections: select commands keep anchor, move commands collapse.
        if is_select {
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
#[cfg(not(feature = "sdl"))]
fn build_style(_config: &NativeConfig, _ctx: &()) -> StyleContext {
    StyleContext::default()
}

#[cfg(not(feature = "sdl"))]
fn load_fonts(_config: &NativeConfig) -> Result<(), String> {
    Ok(())
}
