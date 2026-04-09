//! Editor main loop.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crossbeam_channel::Receiver;
use notify::{Event, RecursiveMode, Watcher};

use crate::editor::buffer;
use crate::editor::config::NativeConfig;
use crate::editor::context_menu::{ContextMenu, MenuItem};
use crate::editor::doc_view::{DocView, RenderLine, RenderToken};
use crate::editor::empty_view::EmptyView;
use crate::editor::event::{EditorEvent, MouseButton};
use crate::editor::keymap::NativeKeymap;
use crate::editor::lsp;
use crate::editor::lsp_client::*;
use crate::editor::picker;
use crate::editor::status_view::{StatusItem, StatusView};
use crate::editor::storage;
use crate::editor::style_ctx::StyleContext;
use crate::editor::syntax::SyntaxDefinition;
use crate::editor::tokenizer::{self, CompiledSyntax};
use crate::editor::terminal_panel::*;
use crate::editor::view::{UpdateContext, View};

/// Append a timestamped message to the log file in the user directory.
#[cfg(feature = "sdl")]
fn log_to_file(userdir: &str, msg: &str) {
    let path = format!("{userdir}/lite-anvil.log");
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
const MAX_SIDEBAR_W: f64 = 600.0;

/// Scan a directory non-recursively and return sorted sidebar entries at the given depth.
fn scan_directory(dir: &str, depth: usize) -> Vec<SidebarEntry> {
    let mut entries = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        return entries;
    };
    for entry in read.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
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
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name))
    });
    entries
}

/// File watcher state for autoreload on external changes.
struct AutoreloadState {
    watcher: Option<notify::RecommendedWatcher>,
    rx: Option<Receiver<notify::Result<Event>>>,
    watched_paths: HashSet<String>,
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
            watched_paths: HashSet::new(),
        }
    }

    /// Start watching a file path for external changes.
    fn watch(&mut self, path: &str) {
        if self.watched_paths.contains(path) {
            return;
        }
        if let Some(ref mut w) = self.watcher {
            if w.watch(Path::new(path), RecursiveMode::NonRecursive).is_ok() {
                self.watched_paths.insert(path.to_string());
            }
        }
    }

    /// Stop watching a file path.
    fn unwatch(&mut self, path: &str) {
        if self.watched_paths.remove(path) {
            if let Some(ref mut w) = self.watcher {
                let _ = w.unwatch(Path::new(path));
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
                    if matches!(ev.kind, EventKind::Modify(_)) {
                        for p in &ev.paths {
                            if let Some(s) = p.to_str() {
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

/// Match a syntax definition to a filename by checking `files` patterns.
fn match_syntax_for_file<'a>(
    filename: &str,
    syntaxes: &'a [SyntaxDefinition],
) -> Option<&'a SyntaxDefinition> {
    syntaxes.iter().find(|def| {
        def.files.iter().any(|pattern| {
            // Patterns look like "%.rs$", "%.py$", etc. Extract the extension.
            if let Some(ext_part) = pattern.strip_prefix("%.") {
                let ext = ext_part.trim_end_matches('$');
                filename.ends_with(&format!(".{ext}"))
            } else if let Some(name_part) = pattern.strip_prefix('%') {
                let name = name_part.trim_end_matches('$');
                filename.ends_with(name)
            } else {
                let clean = pattern.trim_end_matches('$');
                filename.ends_with(clean)
            }
        })
    })
}

/// Session data for save/restore.
#[derive(serde::Serialize, serde::Deserialize)]
struct SessionData {
    files: Vec<String>,
    active: usize,
    #[serde(default)]
    active_project: String,
}

/// Run the editor main loop. Returns true if restart requested.
#[cfg(feature = "sdl")]
pub fn run(config: NativeConfig, _args: &[String], datadir: &str, userdir: &str) -> bool {
    // Create window.
    if !crate::window::restore_window() {
        if let Err(e) = crate::window::create_window("Lite-Anvil") {
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
    if let Ok(Some(size_str)) = crate::editor::storage::load_text(userdir_path, "session", "font_size") {
        if let Ok(size) = size_str.trim().parse::<f32>() {
            let base_size = (size / crate::window::get_display_scale() as f32) as u32;
            config.fonts.ui.size = base_size;
            config.fonts.code.size = base_size;
        }
    }

    let mut draw_ctx = match load_fonts(&config) {
        Ok(ctx) => ctx,
        Err(e) => {
            log::error!("Font loading failed: {e}");
            return false;
        }
    };
    let display_scale = crate::window::get_display_scale();
    let mut style = build_style(&config, &draw_ctx);

    // Load theme colors from JSON.
    let theme_name = &config.theme;
    let theme_path = format!("{datadir}/assets/themes/{theme_name}.json");
    if let Ok(palette) = crate::editor::style::load_theme_palette(&theme_path) {
        apply_theme_to_style(&mut style, &palette);
    } else {
        eprintln!("[native] Theme not found: {theme_path}, using defaults");
    }
    // Build list of available themes.
    let available_themes: Vec<String> = {
        let themes_dir = format!("{datadir}/assets/themes");
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
    let mut current_theme_idx = available_themes.iter().position(|t| t == theme_name).unwrap_or(0);
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
    empty_view.version = env!("CARGO_PKG_VERSION").to_string();
    for (fmt, cmd) in EmptyView::commands() {
        if let Some(binding) = keymap.get_binding_display(cmd) {
            empty_view
                .display_commands
                .push(fmt.replace("%s", &binding));
        }
    }

    let mut status_view = StatusView::new();
    status_view.left_items.push(StatusItem {
        text: "Lite-Anvil".to_string(),
        color: None,
        command: None,
    });
    status_view.right_items.push(StatusItem {
        text: format!("v{}", env!("CARGO_PKG_VERSION")),
        color: None,
        command: None,
    });

    // Open files from CLI args.
    struct OpenDoc {
        view: DocView,
        path: String,
        name: String,
        saved_change_id: i64,
        saved_signature: u32,
        indent_type: String,
        indent_size: usize,
        git_changes: std::collections::HashMap<usize, crate::editor::git::LineChange>,
    }

    /// Check if a document has unsaved modifications by comparing content signature.
    fn doc_is_modified(doc: &OpenDoc) -> bool {
        let Some(buf_id) = doc.view.buffer_id else {
            return false;
        };
        buffer::with_buffer(buf_id, |b| {
            // Fast path: change_id matches saved → definitely not modified.
            if b.change_id == doc.saved_change_id {
                return Ok(false);
            }
            // Slow path: content may match even if change_id differs.
            Ok(buffer::content_signature(&b.lines) != doc.saved_signature)
        }).unwrap_or(false)
    }

    let mut docs: Vec<OpenDoc> = Vec::new();
    let mut active_tab: usize = 0;

    // Helper to open a file into docs.
    fn open_file_into(
        path: &str,
        docs: &mut Vec<OpenDoc>,
    ) -> bool {
        let mut buf_state = buffer::default_buffer_state();
        if let Err(e) = buffer::load_file(&mut buf_state, path) {
            eprintln!("[native] Failed to open {path}: {e}");
            return false;
        }
        let initial_change_id = buf_state.change_id;
        let (indent_type, indent_size, _score) =
            crate::editor::picker::detect_indent(&buf_state.lines, 100, 2);
        let buf_id = buffer::insert_buffer(buf_state);
        let mut dv = DocView::new();
        dv.buffer_id = Some(buf_id);
        dv.indent_size = indent_size;
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
        let git_changes = crate::editor::git::diff_file(path);
        let saved_sig = buffer::with_buffer(buf_id, |b| Ok(buffer::content_signature(&b.lines))).unwrap_or(0);
        docs.push(OpenDoc {
            view: dv,
            path: path.to_string(),
            name,
            saved_change_id: initial_change_id,
            saved_signature: saved_sig,
            indent_type: indent_type.to_string(),
            indent_size,
            git_changes,
        });
        true
    }

    let mut has_cli_files = false;
    for arg in _args.iter().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        has_cli_files = true;
        open_file_into(arg, &mut docs);
    }

    // Session restore: if no CLI file args, try loading from storage.
    let mut restored_project = String::new();
    if !has_cli_files {
        if let Ok(Some(data)) = storage::load_text(userdir_path, "session", "files") {
            if let Ok(session) = serde_json::from_str::<SessionData>(&data) {
                for file in &session.files {
                    open_file_into(file, &mut docs);
                }
                if session.active < docs.len() {
                    active_tab = session.active;
                }
                restored_project = session.active_project;
            }
        }
    }

    // Sidebar state.
    let mut sidebar_visible = true;
    // Load saved sidebar width.
    let mut sidebar_width: f64 = crate::editor::storage::load_text(userdir_path, "session", "sidebar_width")
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(DEFAULT_SIDEBAR_W);
    let mut sidebar_dragging = false;
    let mut editor_mouse_down = false;
    let mut tab_dragging: Option<usize> = None;
    let mut mouse_x: f64 = 0.0;
    let mut mouse_y: f64 = 0.0;
    let mut sidebar_entries: Vec<SidebarEntry>;
    let mut sidebar_scroll: f64 = 0.0;

    // Determine project root for sidebar: prefer restored project, then first open file.
    let mut project_root: String = if !restored_project.is_empty()
        && Path::new(&restored_project).is_dir()
    {
        restored_project
    } else {
        let first_path = docs.first().map(|d| d.path.as_str()).unwrap_or(".");
        let p = PathBuf::from(first_path);
        if p.is_dir() {
            p.to_string_lossy().to_string()
        } else {
            p.parent()
                .map(|pp| pp.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string())
        }
    };
    sidebar_entries = scan_directory(&project_root, 0);

    // Recent projects list (persisted).
    let mut recent_projects: Vec<String> = crate::editor::storage::load_text(userdir_path, "session", "recent_projects")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // Add current project to recents.
    {
        let abs = std::fs::canonicalize(&project_root).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| project_root.clone());
        if !abs.is_empty() && !recent_projects.contains(&abs) {
            recent_projects.insert(0, abs);
            if recent_projects.len() > 20 { recent_projects.truncate(20); }
        }
    }

    let fps = config.fps as f64;
    let mut redraw = true;
    let mut quit = false;
    let mut window_title = String::new();
    let frame_interval = 1.0 / fps;

    // Find bar state.
    let mut find_active = false;
    let mut find_query = String::new();
    let mut replace_active = false;
    let mut replace_query = String::new();
    let mut find_focus_on_replace = false;

    // Nag view state.
    let mut nag_active = false;
    let mut nag_message = String::new();
    let mut nag_tab_to_close: Option<usize> = None;
    let mut info_message: Option<(String, Instant)> = None;

    // Command palette state.
    let mut palette_active = false;
    let mut palette_query = String::new();
    let mut palette_results: Vec<(String, String)> = Vec::new(); // (cmd_name, display_name)
    let mut palette_selected: usize = 0;

    // Build command list for palette from keymap.
    let all_commands: Vec<(String, String)> = {
        let mut cmds = Vec::new();
        // Extract unique command names from keymap bindings.
        let mut seen = std::collections::HashSet::new();
        for (stroke, cmd_names) in keymap.iter_bindings() {
            for cmd in cmd_names {
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
            "doc:upper-case",
            "doc:lower-case",
            "doc:reload",
            "git:pull",
            "git:push",
            "git:commit",
            "git:stash",
            "root:close-all",
            "root:close-all-others",
            "root:close-or-quit",
        ];
        for cmd in palette_extras {
            if seen.insert((*cmd).to_string()) {
                let display = crate::editor::keymap::prettify_name(cmd);
                cmds.push(((*cmd).to_string(), display));
            }
        }
        cmds.sort_by(|a, b| a.1.cmp(&b.1));
        cmds
    };

    // Command view state (keyboard-navigated autocomplete input).
    #[derive(Clone, Copy, PartialEq)]
    enum CmdViewMode { OpenFile, OpenFolder }
    let mut cmdview_active = false;
    let mut cmdview_mode = CmdViewMode::OpenFile;
    let mut cmdview_text = String::new();
    let mut cmdview_suggestions: Vec<String> = Vec::new();
    let mut cmdview_selected: usize = 0;
    let mut cmdview_label = String::new();

    // Project-wide search state.
    // Git status view.
    let mut git_status_active = false;
    let mut git_status_entries: Vec<(String, String, String)> = Vec::new();
    let mut git_status_selected: usize = 0;

    fn run_git_status(root: &str) -> Vec<(String, String, String)> {
        let Ok(output) = std::process::Command::new("git")
            .arg("-C").arg(root)
            .args(["status", "--porcelain=v1"])
            .output()
        else { return Vec::new() };
        if !output.status.success() { return Vec::new(); }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                if line.len() < 4 { return None; }
                let code = line[..2].trim().to_string();
                let path = line[3..].trim().to_string();
                let display = format!("[{code}] {path}");
                Some((code, path, display))
            })
            .collect()
    }

    let mut project_search_active = false;
    let mut project_search_query = String::new();
    let mut project_search_results: Vec<(String, usize, String)> = Vec::new();
    let mut project_search_selected: usize = 0;

    /// Run grep across the project, returning (path, line_number, line_text) tuples.
    fn run_project_search(query: &str, root: &str) -> Vec<(String, usize, String)> {
        if query.len() < 2 {
            return Vec::new();
        }
        let output = std::process::Command::new("grep")
            .args([
                "-rn", "-i",
                "--include=*.rs", "--include=*.toml", "--include=*.json",
                "--include=*.md", "--include=*.txt", "--include=*.js",
                "--include=*.ts", "--include=*.py", "--include=*.go",
                "--include=*.c", "--include=*.h", "--include=*.cpp",
                "--include=*.java",
                query, root,
            ])
            .output();
        let Ok(out) = output else {
            return Vec::new();
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut results = Vec::new();
        for line in stdout.lines().take(100) {
            // Format: path:line_num:text
            let mut parts = line.splitn(3, ':');
            let Some(path) = parts.next() else { continue };
            let Some(num_str) = parts.next() else { continue };
            let Some(text) = parts.next() else { continue };
            let Ok(line_num) = num_str.parse::<usize>() else { continue };
            results.push((path.to_string(), line_num, text.trim().to_string()));
        }
        results
    }

    /// List filesystem entries matching a typed path prefix.
    fn path_suggest(text: &str, project_root: &str, dirs_only: bool) -> Vec<String> {
        let expanded = if let Some(rest) = text.strip_prefix('~') {
            if let Some(home) = std::env::var_os("HOME") {
                format!("{}{rest}", home.to_string_lossy())
            } else {
                text.to_string()
            }
        } else {
            text.to_string()
        };

        let (dir, prefix) = if let Some(pos) = expanded.rfind('/') {
            (&expanded[..=pos], &expanded[pos + 1..])
        } else {
            (project_root, expanded.as_str())
        };

        let lookup = if std::path::Path::new(dir).is_absolute() {
            dir.to_string()
        } else {
            format!("{project_root}/{dir}")
        };

        let mut results = Vec::new();
        let Ok(entries) = std::fs::read_dir(&lookup) else {
            return results;
        };
        let prefix_lower = prefix.to_lowercase();
        let mut entries_sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries_sorted.sort_by_key(|e| e.file_name());

        for entry in entries_sorted {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') && !prefix.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if dirs_only && !is_dir {
                continue;
            }
            if !prefix_lower.is_empty() && !name.to_lowercase().starts_with(&prefix_lower) {
                continue;
            }
            let display = if dir.ends_with('/') || dir.is_empty() {
                format!("{dir}{name}")
            } else {
                format!("{dir}/{name}")
            };
            let display = if is_dir {
                format!("{display}/")
            } else {
                display
            };
            results.push(display);
        }
        results
    }

    // Context menu state.
    let mut context_menu = ContextMenu::new();

    // LSP completion, hover, and go-to-definition state.
    let mut completion = CompletionState::new();
    let mut hover = HoverState::new();

    // Terminal emulator panel (multi-terminal).
    let mut terminal = TerminalPanel::new();

    // Minimap state.
    let mut minimap_visible = false;
    let mut line_wrapping = false;
    let mut overwrite_mode = false;
    let mut cursor_blink_reset = Instant::now();
    let blink_period = 0.5;

    // Autoreload: watch open files for external changes.
    let mut autoreload = AutoreloadState::new();
    // Nag state for autoreload (distinct from close-tab nag).
    let mut reload_nag_active = false;
    let mut reload_nag_path = String::new();
    for doc in &docs {
        autoreload.watch(&doc.path);
    }

    // Syntax highlighting: load definitions and cache compiled syntaxes per extension.
    let syntax_defs = crate::editor::syntax::load_syntax_assets(datadir);
    let mut compiled_syntax_cache: HashMap<String, Option<CompiledSyntax>> = HashMap::new();

    // LSP state.
    let mut lsp_state = LspState::new();
    let lsp_specs = lsp::builtin_specs();

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
        let Some(filetype) = ext_to_lsp_filetype(ext) else { return };
        let Some(spec) = find_lsp_spec(filetype, lsp_specs) else { return };
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
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if cmd.is_empty() { return; }
        match lsp::spawn_transport(&cmd, &root_dir, &[]) {
            Ok(tid) => {
                lsp_state.transport_id = Some(tid);
                lsp_state.root_uri = path_to_uri(&root_dir);
                lsp_state.filetype = filetype.to_string();
                let req_id = lsp_state.next_id();
                lsp_state.pending_requests.insert(req_id, "initialize".to_string());
                let _ = lsp::send_message(tid, &lsp_initialize_request(req_id, &lsp_state.root_uri));
            }
            Err(e) => {
                log_to_file(userdir, &format!("Failed to spawn LSP: {e}"));
                if verbose { eprintln!("[native] Failed to spawn LSP: {e}"); }
            }
        }
    }

    // Try to start LSP for the first open file.
    if let Some(doc) = docs.first() {
        try_start_lsp(&doc.path, &mut lsp_state, &lsp_specs, userdir, config.verbose);
    }

    // Clear any stale shutdown signal from prior runs.
    if crate::signal::shutdown_requested() {
        crate::signal::clear_shutdown();
    }

    loop {
        if crate::signal::shutdown_requested() {
            crate::signal::clear_shutdown();
            if docs.iter().any(doc_is_modified) {
                nag_active = true;
                nag_message = "Unsaved changes. Save all?  [Y]es  [N]o  [Esc]Cancel".to_string();
                nag_tab_to_close = None;
            } else {
                quit = true;
            }
        }

        // Poll all pending events.
        while let Some(event) = crate::window::poll_event_native() {
            match &event {
                EditorEvent::Quit => {
                    if docs.iter().any(doc_is_modified) {
                        nag_active = true;
                        nag_message = "Unsaved changes. Save all?  [Y]es  [N]o  [Esc]Cancel".to_string();
                        nag_tab_to_close = None;
                    } else {
                        quit = true;
                    }
                }
                EditorEvent::Exposed | EditorEvent::Resized { .. } | EditorEvent::FocusGained => {
                    redraw = true;
                }
                EditorEvent::KeyPressed { key, modifiers } => {
                    cursor_blink_reset = Instant::now();
                    let mods = *modifiers;

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
                                    context_menu.selected =
                                        Some(context_menu.items.len() - 1);
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
                                                handle_doc_command(
                                                    &mut doc.view,
                                                    &cmd,
                                                    &style,
                                                    &doc.indent_type,
                                                    doc.indent_size,
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
                                                let col =
                                                    *b.selections.get(1).unwrap_or(&1);
                                                if line <= b.lines.len() {
                                                    let l = &mut b.lines[line - 1];
                                                    let byte_pos =
                                                        char_to_byte(l, col - 1);
                                                    l.insert_str(byte_pos, &text);
                                                    let new_col =
                                                        col + text.chars().count();
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

                    // Command view (file/folder open) intercepts keys.
                    if cmdview_active {
                        /// Expand ~ and resolve relative paths to absolute.
                        fn expand_path(text: &str, project_root: &str) -> String {
                            if let Some(rest) = text.strip_prefix('~') {
                                if let Some(home) = std::env::var_os("HOME") {
                                    return format!("{}{rest}", home.to_string_lossy());
                                }
                            }
                            if !text.starts_with('/') {
                                return format!("{project_root}/{text}");
                            }
                            text.to_string()
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
                                                doc.view.scroll_y = ((target as f64 - 1.0) * line_h - doc.view.rect().h / 2.0).max(0.0);
                                                doc.view.target_scroll_y = doc.view.scroll_y;
                                            }
                                        }
                                    }
                                    cmdview_active = false;
                                    redraw = true;
                                    continue;
                                }
                                // Use highlighted suggestion text, or current input.
                                let chosen = if !cmdview_suggestions.is_empty() && cmdview_selected < cmdview_suggestions.len() {
                                    cmdview_suggestions[cmdview_selected].clone()
                                } else {
                                    cmdview_text.clone()
                                };
                                let path = expand_path(&chosen, &project_root);
                                let path = path.trim_end_matches('/').to_string();
                                let p = std::path::Path::new(&path);
                                match cmdview_mode {
                                    CmdViewMode::OpenFile => {
                                        if p.is_file() {
                                            cmdview_active = false;
                                            if open_file_into(&path, &mut docs) {
                                                active_tab = docs.len() - 1;
                                                autoreload.watch(&path);
                                            }
                                        } else if p.is_dir() {
                                            // Navigate into directory.
                                            cmdview_text = format!("{path}/");
                                            cmdview_suggestions = path_suggest(&cmdview_text, &project_root, false);
                                            cmdview_selected = 0;
                                        }
                                    }
                                    CmdViewMode::OpenFolder => {
                                        if p.is_dir() {
                                            // Check for unsaved changes before switching.
                                            if docs.iter().any(doc_is_modified) {
                                                nag_active = true;
                                                nag_message = "Unsaved changes. Save all before switching project?  [Y]es  [N]o  [Esc]Cancel".to_string();
                                                nag_tab_to_close = None;
                                            } else {
                                                // Close all open docs from previous project.
                                                for d in &docs {
                                                    autoreload.unwatch(&d.path);
                                                }
                                                docs.clear();
                                                active_tab = 0;
                                                cmdview_active = false;
                                                project_root = path;
                                                sidebar_entries = scan_directory(&project_root, 0);
                                                sidebar_visible = true;
                                                // Update recent projects.
                                                let abs = std::fs::canonicalize(&project_root).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| project_root.clone());
                                                recent_projects.retain(|p| p != &abs);
                                                recent_projects.insert(0, abs);
                                                if recent_projects.len() > 20 { recent_projects.truncate(20); }
                                                let _ = crate::editor::storage::save_text(userdir_path, "session", "recent_projects", &serde_json::to_string(&recent_projects).unwrap_or_default());
                                            }
                                        }
                                    }
                                }
                            }
                            "tab" => {
                                // Select current suggestion: replace text, refresh.
                                if !cmdview_suggestions.is_empty() && cmdview_selected < cmdview_suggestions.len() {
                                    cmdview_text = cmdview_suggestions[cmdview_selected].clone();
                                    let dirs_only = cmdview_mode == CmdViewMode::OpenFolder;
                                    cmdview_suggestions = path_suggest(&cmdview_text, &project_root, dirs_only);
                                    cmdview_selected = 0;
                                }
                            }
                            "up" => {
                                if cmdview_selected > 0 {
                                    cmdview_selected -= 1;
                                } else if !cmdview_suggestions.is_empty() {
                                    cmdview_selected = cmdview_suggestions.len() - 1;
                                }
                            }
                            "down" => {
                                if !cmdview_suggestions.is_empty() {
                                    cmdview_selected = (cmdview_selected + 1) % cmdview_suggestions.len();
                                }
                            }
                            "backspace" => {
                                if mods.ctrl {
                                    if cmdview_text.ends_with('/') {
                                        cmdview_text.pop();
                                    }
                                    if let Some(pos) = cmdview_text.rfind('/') {
                                        cmdview_text.truncate(pos + 1);
                                    } else {
                                        cmdview_text.clear();
                                    }
                                } else {
                                    cmdview_text.pop();
                                }
                                let dirs_only = cmdview_mode == CmdViewMode::OpenFolder;
                                if cmdview_text.is_empty() && dirs_only {
                                    cmdview_suggestions = recent_projects.clone();
                                } else {
                                    cmdview_suggestions = path_suggest(&cmdview_text, &project_root, dirs_only);
                                }
                                cmdview_selected = 0;
                            }
                            _ => {}
                        }
                        redraw = true;
                        continue;
                    }

                    // Project search intercepts keys when active.
                    if project_search_active {
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
                                    let tab_idx =
                                        docs.iter().position(|d| d.path == path);
                                    let idx = if let Some(i) = tab_idx {
                                        i
                                    } else if open_file_into(&path, &mut docs) {
                                        autoreload.watch(&path);
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
                            "down" => {
                                if !project_search_results.is_empty() {
                                    project_search_selected = (project_search_selected + 1)
                                        .min(project_search_results.len() - 1);
                                }
                            }
                            "backspace" => {
                                project_search_query.pop();
                                project_search_results =
                                    run_project_search(&project_search_query, &project_root);
                                project_search_selected = 0;
                            }
                            _ => {}
                        }
                        redraw = true;
                        continue;
                    }

                    // Git status view intercepts keys.
                    if git_status_active {
                        match key.as_str() {
                            "escape" => { git_status_active = false; }
                            "return" | "keypad enter" => {
                                if let Some((_, path, _)) = git_status_entries.get(git_status_selected).cloned() {
                                    git_status_active = false;
                                    let full_path = format!("{project_root}/{path}");
                                    let tab_idx = docs.iter().position(|d| d.path == full_path);
                                    let idx = if let Some(i) = tab_idx {
                                        i
                                    } else if open_file_into(&full_path, &mut docs) {
                                        autoreload.watch(&full_path);
                                        docs.len() - 1
                                    } else {
                                        redraw = true;
                                        continue;
                                    };
                                    active_tab = idx;
                                }
                            }
                            "up" => { git_status_selected = git_status_selected.saturating_sub(1); }
                            "down" => {
                                if !git_status_entries.is_empty() {
                                    git_status_selected = (git_status_selected + 1).min(git_status_entries.len() - 1);
                                }
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
                            }
                        }
                        redraw = true;
                        continue;
                    }

                    // Dismiss info message on any key.
                    if info_message.is_some() {
                        info_message = None;
                        redraw = true;
                        if key == "escape" { continue; }
                    }

                    // Nag view intercepts keys when active.
                    if nag_active {
                        match key.as_str() {
                            "y" | "Y" => {
                                if let Some(idx) = nag_tab_to_close {
                                    // Save then close single tab.
                                    let has_path = docs.get(idx).map(|d| !d.path.is_empty()).unwrap_or(false);
                                    if !has_path {
                                        nag_message = "File has no name. Use Save As (Ctrl+Shift+S) first, or press N to discard.".to_string();
                                        redraw = true;
                                        continue;
                                    }
                                    if let Some(doc) = docs.get(idx) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            let path = doc.path.clone();
                                            let _ = buffer::with_buffer(buf_id, |b| {
                                                buffer::save_file(b, &path, b.crlf)
                                                    .map_err(|_| buffer::BufferError::UnknownBuffer)
                                            });
                                        }
                                    }
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
                                    // Save all modified docs then quit.
                                    let has_unnamed = docs.iter().any(|d| doc_is_modified(d) && d.path.is_empty());
                                    if has_unnamed {
                                        nag_message = "Unnamed files cannot be saved. Close them first or use Save As (Ctrl+Shift+S).".to_string();
                                    } else {
                                        for doc in &docs {
                                            if doc_is_modified(doc) {
                                                if let Some(buf_id) = doc.view.buffer_id {
                                                    let path = doc.path.clone();
                                                    if !path.is_empty() {
                                                        let _ = buffer::with_buffer(buf_id, |b| {
                                                            buffer::save_file(b, &path, b.crlf)
                                                                .map_err(|_| buffer::BufferError::UnknownBuffer)
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                        quit = true;
                                    }
                                }
                                nag_active = false;
                                nag_tab_to_close = None;
                                redraw = true;
                                continue;
                            }
                            "n" | "N" => {
                                if let Some(idx) = nag_tab_to_close {
                                    // Close without saving single tab.
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
                                    // Quit without saving.
                                    quit = true;
                                }
                                nag_active = false;
                                nag_tab_to_close = None;
                                redraw = true;
                                continue;
                            }
                            "escape" => {
                                nag_active = false;
                                nag_tab_to_close = None;
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
                    if reload_nag_active {
                        match key.as_str() {
                            "y" | "Y" => {
                                // Reload from disk.
                                let rpath = reload_nag_path.clone();
                                if let Some(doc) = docs.iter_mut().find(|d| d.path == rpath) {
                                    if let Some(buf_id) = doc.view.buffer_id {
                                        let _ = buffer::with_buffer_mut(buf_id, |b| {
                                            let mut buf_state = buffer::default_buffer_state();
                                            if buffer::load_file(&mut buf_state, &rpath).is_ok() {
                                                b.lines = buf_state.lines;
                                                b.change_id = buf_state.change_id;
                                            }
                                            Ok(())
                                        });
                                        if let Ok(cid) = buffer::with_buffer(buf_id, |b| Ok(b.change_id)) {
                                            doc.saved_change_id = cid;
                                        }
                                    }
                                }
                                reload_nag_active = false;
                                reload_nag_path.clear();
                                redraw = true;
                                continue;
                            }
                            "n" | "N" | "escape" => {
                                reload_nag_active = false;
                                reload_nag_path.clear();
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
                            "escape" => { palette_active = false; redraw = true; continue; }
                            "return" | "keypad enter" => {
                                if let Some((cmd, _)) = palette_results.get(palette_selected) {
                                    let cmd = cmd.clone();
                                    palette_active = false;
                                    // If the selected item is a file path, open it.
                                    if cmd.starts_with('/') && std::path::Path::new(&cmd).is_file() {
                                        if open_file_into(&cmd, &mut docs) {
                                            active_tab = docs.len() - 1;
                                            autoreload.watch(&cmd);
                                        }
                                        redraw = true;
                                        continue;
                                    }
                                    // Execute the selected command.
                                    match cmd.as_str() {
                                        "core:quit" => {
                                            if docs.iter().any(doc_is_modified) {
                                                nag_active = true;
                                                nag_message = "Unsaved changes. Save all?  [Y]es  [N]o  [Esc]Cancel".to_string();
                                                nag_tab_to_close = None;
                                            } else {
                                                quit = true;
                                            }
                                        }
                                        "core:new-doc" => {
                                            let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
                                            let mut dv = DocView::new();
                                            dv.buffer_id = Some(buf_id);
                                            docs.push(OpenDoc { view: dv, path: String::new(), name: "[new]".to_string(), saved_change_id: 1, saved_signature: buffer::content_signature(&["\n".to_string()]), indent_type: "soft".to_string(), indent_size: 2, git_changes: std::collections::HashMap::new() });
                                            active_tab = docs.len() - 1;
                                        }
                                        "core:toggle-terminal" => {
                                            terminal.visible = !terminal.visible;
                                            if terminal.visible && terminal.terminals.is_empty() {
                                                terminal.spawn(&project_root);
                                            }
                                            terminal.focused = terminal.visible;
                                        }
                                        "core:new-terminal" => {
                                            terminal.spawn(&project_root);
                                        }
                                        "core:close-terminal" => {
                                            if !terminal.terminals.is_empty() {
                                                terminal.terminals.remove(terminal.active);
                                                if terminal.terminals.is_empty() {
                                                    terminal.visible = false;
                                                    terminal.focused = false;
                                                } else {
                                                    terminal.active = terminal.active.min(terminal.terminals.len() - 1);
                                                }
                                            }
                                        }
                                        "core:toggle-sidebar" => {
                                            sidebar_visible = !sidebar_visible;
                                        }
                                        "core:toggle-minimap" => {
                                            minimap_visible = !minimap_visible;
                                        }
                                        "core:toggle-line-wrapping" => {
                                            line_wrapping = !line_wrapping;
                                        }
                                        "core:toggle-whitespace" => {
                                            if let Some(doc) = docs.get_mut(active_tab) {
                                                doc.view.show_whitespace = !doc.view.show_whitespace;
                                            }
                                        }
                                        "core:cycle-theme" => {
                                            if !available_themes.is_empty() {
                                                current_theme_idx = (current_theme_idx + 1) % available_themes.len();
                                                let new_theme = &available_themes[current_theme_idx];
                                                let tp = format!("{datadir}/assets/themes/{new_theme}.json");
                                                if let Ok(palette) = crate::editor::style::load_theme_palette(&tp) {
                                                    apply_theme_to_style(&mut style, &palette);
                                                }
                                            }
                                        }
                                        "core:open-user-settings" => {
                                            let settings_path = format!("{userdir}/config.toml");
                                            if !std::path::Path::new(&settings_path).exists() {
                                                let _ = std::fs::write(&settings_path, NativeConfig::default_toml_template());
                                            }
                                            if open_file_into(&settings_path, &mut docs) {
                                                active_tab = docs.len() - 1;
                                            }
                                        }
                                        "about:version" => {
                                            info_message = Some(("Lite-Anvil v2.0.0".to_string(), Instant::now()));
                                        }
                                        "core:force-quit" => {
                                            quit = true;
                                        }
                                        "root:close-all" => {
                                            let any_modified = docs.iter().any(|d| doc_is_modified(d));
                                            if any_modified {
                                                nag_active = true;
                                                nag_message = "Unsaved changes. Save all?  [Y]es  [N]o  [Esc]Cancel".to_string();
                                                nag_tab_to_close = None;
                                            } else {
                                                for d in &docs { autoreload.unwatch(&d.path); }
                                                docs.clear();
                                                active_tab = 0;
                                            }
                                        }
                                        "root:close-all-others" => {
                                            let keep = active_tab;
                                            for i in (0..docs.len()).rev() {
                                                if i != keep {
                                                    autoreload.unwatch(&docs[i].path);
                                                    docs.remove(i);
                                                }
                                            }
                                            active_tab = 0;
                                        }
                                        "root:close-or-quit" => {
                                            if docs.is_empty() {
                                                quit = true;
                                            } else if doc_is_modified(&docs[active_tab]) {
                                                let fname = &docs[active_tab].name;
                                                nag_message = format!("Save changes to {fname}?  [Y]es  [N]o  [Esc]Cancel");
                                                nag_active = true;
                                                nag_tab_to_close = Some(active_tab);
                                            } else {
                                                autoreload.unwatch(&docs[active_tab].path);
                                                docs.remove(active_tab);
                                                if docs.is_empty() {
                                                    quit = true;
                                                } else if active_tab >= docs.len() {
                                                    active_tab = docs.len() - 1;
                                                }
                                            }
                                        }
                                        "doc:upper-case" | "doc:lower-case" => {
                                            if let Some(doc) = docs.get_mut(active_tab) {
                                                if let Some(buf_id) = doc.view.buffer_id {
                                                    let is_upper = cmd == "doc:upper-case";
                                                    let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                        let text = buffer::get_selected_text(b);
                                                        if !text.is_empty() {
                                                            buffer::push_undo(b);
                                                            buffer::delete_selection(b);
                                                            let converted = if is_upper { text.to_uppercase() } else { text.to_lowercase() };
                                                            let line = b.selections[0];
                                                            let col = b.selections[1];
                                                            if line <= b.lines.len() {
                                                                let l = &mut b.lines[line - 1];
                                                                let byte_pos = l.char_indices().nth(col - 1).map(|(i,_)| i).unwrap_or(l.len());
                                                                l.insert_str(byte_pos, &converted);
                                                                let new_col = col + converted.chars().count();
                                                                b.selections = vec![line, col, line, new_col];
                                                            }
                                                        }
                                                        Ok(())
                                                    });
                                                }
                                            }
                                        }
                                        "doc:reload" => {
                                            if let Some(doc) = docs.get_mut(active_tab) {
                                                if !doc.path.is_empty() {
                                                    if let Some(buf_id) = doc.view.buffer_id {
                                                        let path = doc.path.clone();
                                                        let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                            let mut fresh = buffer::default_buffer_state();
                                                            let _ = buffer::load_file(&mut fresh, &path);
                                                            b.lines = fresh.lines;
                                                            b.change_id += 1;
                                                            Ok(())
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                        "git:pull" | "git:push" | "git:commit" | "git:stash" => {
                                            let git_cmd = match cmd.as_str() {
                                                "git:pull" => vec!["pull"],
                                                "git:push" => vec!["push"],
                                                "git:commit" => vec!["commit", "--allow-empty-message", "-m", ""],
                                                "git:stash" => vec!["stash"],
                                                _ => vec![],
                                            };
                                            if !git_cmd.is_empty() {
                                                let _ = std::process::Command::new("git")
                                                    .arg("-C").arg(&project_root)
                                                    .args(&git_cmd)
                                                    .output();
                                            }
                                        }
                                        "core:find" => {
                                            find_active = true;
                                            replace_active = false;
                                        }
                                        "core:find-replace" => {
                                            find_active = true;
                                            replace_active = true;
                                        }
                                        "core:project-search" => {
                                            project_search_active = true;
                                            project_search_query.clear();
                                            project_search_results.clear();
                                            project_search_selected = 0;
                                        }
                                        "core:git-status" => {
                                            git_status_active = true;
                                            git_status_entries = run_git_status(&project_root);
                                            git_status_selected = 0;
                                        }
                                        "core:open-project-folder" => {
                                            cmdview_active = true;
                                            cmdview_mode = CmdViewMode::OpenFolder;
                                            cmdview_text = String::new();
                                            cmdview_label = "Open Folder:".to_string();
                                            // Show recent projects when text is empty.
                                            cmdview_suggestions = recent_projects.clone();
                                            cmdview_selected = 0;
                                        }
                                        "core:open-file" | "core:open-file-from-project" => {
                                            cmdview_active = true;
                                            cmdview_mode = CmdViewMode::OpenFile;
                                            if let Some(doc) = docs.get(active_tab) {
                                                if let Some(pos) = doc.path.rfind('/') {
                                                    cmdview_text = format!("{}/", &doc.path[..pos]);
                                                } else {
                                                    cmdview_text = format!("{project_root}/");
                                                }
                                            } else {
                                                cmdview_text = format!("{project_root}/");
                                            }
                                            cmdview_label = "Open File:".to_string();
                                            cmdview_suggestions = path_suggest(&cmdview_text, &project_root, false);
                                            cmdview_selected = 0;
                                        }
                                        _ => {
                                            if let Some(doc) = docs.get_mut(active_tab) {
                                                handle_doc_command(&mut doc.view, &cmd, &style, &doc.indent_type, doc.indent_size);
                                            }
                                        }
                                    }
                                }
                                redraw = true;
                                continue;
                            }
                            "backspace" => { palette_query.pop(); }
                            "up" => { palette_selected = palette_selected.saturating_sub(1); }
                            "down" => { palette_selected += 1; }
                            _ => { continue; }
                        }
                        // Filter commands with fuzzy matching.
                        palette_results = fuzzy_filter_commands(&palette_query, &all_commands);
                        palette_selected = palette_selected.min(palette_results.len().saturating_sub(1));
                        redraw = true;
                        continue;
                    }

                    // Find bar intercepts keys when active.
                    if find_active {
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
                            "return" | "keypad enter" if mods.ctrl && replace_active => {
                                // Replace current match and find next.
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    let dv = &mut doc.view;
                                    replace_current_match(dv, &find_query, &replace_query);
                                    find_next_in_doc(dv, &find_query);
                                }
                                redraw = true;
                                continue;
                            }
                            "return" | "keypad enter" => {
                                if let Some(doc) = docs.get_mut(active_tab) {
                                    let dv = &mut doc.view;
                                    find_next_in_doc(dv, &find_query);
                                }
                                redraw = true;
                                continue;
                            }
                            "backspace" => {
                                if find_focus_on_replace {
                                    replace_query.pop();
                                } else {
                                    find_query.pop();
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
                                if let Ok(palette) = crate::editor::style::load_theme_palette(&format!("{datadir}/assets/themes/{}.json", config.theme)) {
                                    apply_theme_to_style(&mut style, &palette);
                                }
                                crate::editor::style_ctx::set_current_style(style.clone());
                                let _ = crate::editor::storage::save_text(
                                    userdir_path, "session", "font_size", &new_size.to_string(),
                                );
                            }
                            redraw = true;
                            continue;
                        }
                    }

                    // Direct Ctrl+` handling for terminal toggle.
                    if mods.ctrl
                        && !mods.alt
                        && !mods.shift
                        && (key == "`" || key == "grave" || key == "backquote")
                    {
                        terminal.visible = !terminal.visible;
                        if terminal.visible && terminal.terminals.is_empty() {
                            let _ = terminal.spawn(&project_root);
                        }
                        terminal.focused = terminal.visible;
                        redraw = true;
                        continue;
                    }

                    // Direct Ctrl+Shift+T for new terminal.
                    if mods.ctrl && mods.shift && !mods.alt && key == "t" {
                        let ok = terminal.spawn(&project_root);
                        if ok {
                            log_to_file(userdir, &format!("New terminal {} spawned", terminal.terminals.len()));
                        }
                        redraw = true;
                        continue;
                    }

                    if let Some(cmds) = keymap.on_key_pressed(key, mods) {
                        for cmd in Vec::from(cmds) {
                            match cmd.as_str() {
                                "core:quit" => {
                                    if docs.iter().any(doc_is_modified) {
                                        nag_active = true;
                                        nag_message = "Unsaved changes. Save all?  [Y]es  [N]o  [Esc]Cancel".to_string();
                                        nag_tab_to_close = None;
                                    } else {
                                        quit = true;
                                        break;
                                    }
                                }
                                "scale:increase" | "scale:decrease" | "scale:reset" => {
                                    // Handled by direct key check above.
                                }
                                "core:find-command" => {
                                    palette_active = true;
                                    palette_query.clear();
                                    palette_results = all_commands.clone();
                                    palette_selected = 0;
                                }
                                "core:new-doc" => {
                                    let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
                                    let mut dv = DocView::new();
                                    dv.buffer_id = Some(buf_id);
                                    docs.push(OpenDoc {
                                        view: dv,
                                        path: String::new(),
                                        name: "[new]".to_string(),
                                        saved_change_id: 1,
                                        saved_signature: buffer::content_signature(&["\n".to_string()]),
                                        indent_type: "soft".to_string(),
                                        indent_size: 2,
                                        git_changes: std::collections::HashMap::new(),
                                    });
                                    active_tab = docs.len() - 1;
                                }
                                "root:close" => {
                                    if !docs.is_empty() {
                                        if doc_is_modified(&docs[active_tab]) {
                                            let fname = &docs[active_tab].name;
                                            nag_message =
                                                format!("Save changes to {fname}?  [Y]es  [N]o  [Esc]Cancel");
                                            nag_active = true;
                                            nag_tab_to_close = Some(active_tab);
                                        } else {
                                            if let Some(d) = docs.get(active_tab) {
                                                autoreload.unwatch(&d.path);
                                            }
                                            docs.remove(active_tab);
                                            if docs.is_empty() {
                                                active_tab = 0;
                                            } else if active_tab >= docs.len() {
                                                active_tab = docs.len() - 1;
                                            }
                                        }
                                    }
                                }
                                "root:switch-to-next-tab" => {
                                    if !docs.is_empty() {
                                        active_tab = (active_tab + 1) % docs.len();
                                    }
                                }
                                "root:switch-to-previous-tab" => {
                                    if !docs.is_empty() {
                                        active_tab = if active_tab == 0 { docs.len() - 1 } else { active_tab - 1 };
                                    }
                                }
                                "root:toggle-sidebar" => {
                                    sidebar_visible = !sidebar_visible;
                                }
                                "core:toggle-terminal" => {
                                    terminal.visible = !terminal.visible;
                                    if terminal.visible
                                        && terminal.terminals.is_empty()
                                    {
                                        terminal.spawn(&project_root);
                                        log_to_file(
                                            userdir,
                                            "Terminal spawned via toggle",
                                        );
                                    }
                                    terminal.focused = terminal.visible;
                                }
                                "core:new-terminal" => {
                                    if terminal.spawn(&project_root) {
                                        log_to_file(
                                            userdir,
                                            &format!(
                                                "New terminal {} spawned",
                                                terminal.terminals.len()
                                            ),
                                        );
                                    }
                                }
                                "core:close-terminal" => {
                                    terminal.close_active();
                                }
                                "core:toggle-minimap" => {
                                    minimap_visible = !minimap_visible;
                                }
                                "core:toggle-line-wrapping" => {
                                    line_wrapping = !line_wrapping;
                                }
                                "core:toggle-whitespace" => {
                                    if let Some(doc) = docs.get_mut(active_tab) {
                                        doc.view.show_whitespace = !doc.view.show_whitespace;
                                    }
                                }
                                "core:cycle-theme" => {
                                    if !available_themes.is_empty() {
                                        current_theme_idx = (current_theme_idx + 1) % available_themes.len();
                                        let new_theme = &available_themes[current_theme_idx];
                                        let tp = format!("{datadir}/assets/themes/{new_theme}.json");
                                        if let Ok(palette) = crate::editor::style::load_theme_palette(&tp) {
                                            apply_theme_to_style(&mut style, &palette);
                                        }
                                    }
                                }
                                "core:open-user-settings" => {
                                    let settings_path = format!("{userdir}/config.toml");
                                    if !std::path::Path::new(&settings_path).exists() {
                                        let _ = std::fs::write(&settings_path, NativeConfig::default_toml_template());
                                    }
                                    if open_file_into(&settings_path, &mut docs) {
                                        active_tab = docs.len() - 1;
                                    }
                                }
                                "about:version" => {
                                    nag_active = true;
                                    nag_message = "Lite-Anvil v2.0.0 — A lightweight code editor built in Rust with SDL3.".to_string();
                                    nag_tab_to_close = None;
                                }
                                "core:force-quit" => {
                                    quit = true;
                                }
                                "root:close-all" => {
                                    if docs.iter().any(|d| doc_is_modified(d)) {
                                        nag_active = true;
                                        nag_message = "Unsaved changes. Save all?  [Y]es  [N]o  [Esc]Cancel".to_string();
                                        nag_tab_to_close = None;
                                    } else {
                                        for d in &docs { autoreload.unwatch(&d.path); }
                                        docs.clear();
                                        active_tab = 0;
                                    }
                                }
                                "root:close-all-others" => {
                                    let keep = active_tab;
                                    for i in (0..docs.len()).rev() {
                                        if i != keep {
                                            autoreload.unwatch(&docs[i].path);
                                            docs.remove(i);
                                        }
                                    }
                                    active_tab = 0;
                                }
                                "root:close-or-quit" => {
                                    if docs.is_empty() {
                                        quit = true;
                                    } else if doc_is_modified(&docs[active_tab]) {
                                        let fname = &docs[active_tab].name;
                                        nag_message = format!("Save changes to {fname}?  [Y]es  [N]o  [Esc]Cancel");
                                        nag_active = true;
                                        nag_tab_to_close = Some(active_tab);
                                    } else {
                                        autoreload.unwatch(&docs[active_tab].path);
                                        docs.remove(active_tab);
                                        if docs.is_empty() {
                                            quit = true;
                                        } else if active_tab >= docs.len() {
                                            active_tab = docs.len() - 1;
                                        }
                                    }
                                }
                                "git:pull" | "git:push" | "git:commit" | "git:stash" => {
                                    let git_cmd = match cmd.as_str() {
                                        "git:pull" => vec!["pull"],
                                        "git:push" => vec!["push"],
                                        "git:commit" => vec!["commit", "--allow-empty-message", "-m", ""],
                                        "git:stash" => vec!["stash"],
                                        _ => vec![],
                                    };
                                    if !git_cmd.is_empty() {
                                        let _ = std::process::Command::new("git")
                                            .arg("-C").arg(&project_root)
                                            .args(&git_cmd)
                                            .output();
                                    }
                                }
                                "lsp:hover" => {
                                    if let Some(doc) = docs.get(active_tab) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            if let Some(tid) = lsp_state.transport_id {
                                                if lsp_state.initialized
                                                    && !doc.path.is_empty()
                                                {
                                                    let (cl, cc) =
                                                        buffer::with_buffer(buf_id, |b| {
                                                            let l = *b
                                                                .selections
                                                                .get(2)
                                                                .unwrap_or(&1);
                                                            let c = *b
                                                                .selections
                                                                .get(3)
                                                                .unwrap_or(&1);
                                                            Ok((l, c))
                                                        })
                                                        .unwrap_or((1, 1));
                                                    let uri = path_to_uri(&doc.path);
                                                    let req_id = lsp_state.next_id();
                                                    lsp_state.pending_requests.insert(
                                                        req_id,
                                                        "textDocument/hover".to_string(),
                                                    );
                                                    let _ = lsp::send_message(
                                                        tid,
                                                        &lsp_hover_request(
                                                            req_id,
                                                            &uri,
                                                            cl - 1,
                                                            cc - 1,
                                                        ),
                                                    );
                                                    hover.line = cl;
                                                    hover.col = cc;
                                                }
                                            }
                                        }
                                    }
                                }
                                "lsp:go-to-definition" => {
                                    if let Some(doc) = docs.get(active_tab) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            if let Some(tid) = lsp_state.transport_id {
                                                if lsp_state.initialized
                                                    && !doc.path.is_empty()
                                                {
                                                    let (cl, cc) =
                                                        buffer::with_buffer(buf_id, |b| {
                                                            let l = *b
                                                                .selections
                                                                .get(2)
                                                                .unwrap_or(&1);
                                                            let c = *b
                                                                .selections
                                                                .get(3)
                                                                .unwrap_or(&1);
                                                            Ok((l, c))
                                                        })
                                                        .unwrap_or((1, 1));
                                                    let uri = path_to_uri(&doc.path);
                                                    let req_id = lsp_state.next_id();
                                                    lsp_state.pending_requests.insert(
                                                        req_id,
                                                        "textDocument/definition"
                                                            .to_string(),
                                                    );
                                                    let _ = lsp::send_message(
                                                        tid,
                                                        &lsp_definition_request(
                                                            req_id,
                                                            &uri,
                                                            cl - 1,
                                                            cc - 1,
                                                        ),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                "lsp:go-to-implementation"
                                | "lsp:go-to-type-definition"
                                | "lsp:find-references" => {
                                    let method = match cmd.as_str() {
                                        "lsp:go-to-implementation" => "textDocument/implementation",
                                        "lsp:go-to-type-definition" => "textDocument/typeDefinition",
                                        "lsp:find-references" => "textDocument/references",
                                        _ => unreachable!(),
                                    };
                                    if let Some(doc) = docs.get(active_tab) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            if let Some(tid) = lsp_state.transport_id {
                                                if lsp_state.initialized && !doc.path.is_empty() {
                                                    let (cl, cc) = buffer::with_buffer(buf_id, |b| {
                                                        Ok((*b.selections.get(2).unwrap_or(&1), *b.selections.get(3).unwrap_or(&1)))
                                                    }).unwrap_or((1, 1));
                                                    let uri = path_to_uri(&doc.path);
                                                    let req_id = lsp_state.next_id();
                                                    lsp_state.pending_requests.insert(req_id, method.to_string());
                                                    let _ = lsp::send_message(tid, &lsp_position_request(req_id, method, &uri, cl - 1, cc - 1));
                                                }
                                            }
                                        }
                                    }
                                }
                                "doc:go-to-line" => {
                                    cmdview_active = true;
                                    cmdview_mode = CmdViewMode::OpenFile; // reuse mode, Enter parses as line number
                                    cmdview_text.clear();
                                    cmdview_label = "Go To Line:".to_string();
                                    cmdview_suggestions.clear();
                                    cmdview_selected = 0;
                                }
                                "find-replace:find" => {
                                    find_active = true;
                                    replace_active = false;
                                    find_focus_on_replace = false;
                                    find_query.clear();
                                }
                                "find-replace:replace" => {
                                    find_active = true;
                                    replace_active = true;
                                    find_focus_on_replace = false;
                                    find_query.clear();
                                    replace_query.clear();
                                }
                                "doc:save" => {
                                    if let Some(doc) = docs.get_mut(active_tab) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            let path = doc.path.clone();
                                            if !path.is_empty() {
                                                let saved_id = buffer::with_buffer(buf_id, |b| {
                                                    buffer::save_file(b, &path, b.crlf)
                                                        .map_err(|_| buffer::BufferError::UnknownBuffer)?;
                                                    Ok(b.change_id)
                                                });
                                                if let Ok(id) = saved_id {
                                                    doc.saved_change_id = id;
                                                    doc.saved_signature = buffer::with_buffer(buf_id, |b| Ok(buffer::content_signature(&b.lines))).unwrap_or(0);
                                                }
                                                log_to_file(userdir, &format!("Saved {path}"));
                                                doc.git_changes = crate::editor::git::diff_file(&path);
                                                // Notify LSP of save and refresh inlay hints (only for LSP-handled files).
                                                let save_ext = path.rsplit('.').next().unwrap_or("");
                                                if ext_to_lsp_filetype(save_ext).is_some() {
                                                    if let Some(tid) = lsp_state.transport_id {
                                                        if lsp_state.initialized {
                                                            let uri = path_to_uri(&path);
                                                            let _ = lsp::send_message(tid, &lsp_did_save(&uri));
                                                            let line_count = buffer::with_buffer(buf_id, |b| Ok(b.lines.len())).unwrap_or(100);
                                                            let req_id = lsp_state.next_id();
                                                            lsp_state.pending_requests.insert(req_id, "textDocument/inlayHint".to_string());
                                                            lsp_state.inlay_hints.clear();
                                                            let _ = lsp::send_message(tid, &lsp_inlay_hint_request(req_id, &uri, 0, line_count));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                "doc:undo" | "doc:redo" => {
                                    if let Some(doc) = docs.get(active_tab) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                if cmd == "doc:undo" { buffer::undo(b); } else { buffer::redo(b); }
                                                Ok(())
                                            });
                                        }
                                        // Clear stale hints and notify LSP.
                                        if lsp_state.transport_id.is_some() && lsp_state.initialized {
                                            lsp_state.inlay_hints.clear();
                                            let ext = doc.path.rsplit('.').next().unwrap_or("");
                                            if !doc.path.is_empty() && ext_to_lsp_filetype(ext).is_some() {
                                                lsp_state.last_change = Some(Instant::now());
                                                lsp_state.pending_change_uri = Some(path_to_uri(&doc.path));
                                                lsp_state.pending_change_version += 1;
                                            }
                                        }
                                    }
                                }
                                "doc:cut" => {
                                    if let Some(doc) = docs.get(active_tab) { let dv = &doc.view;
                                        if let Some(buf_id) = dv.buffer_id {
                                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                let text = buffer::get_selected_text(b);
                                                if !text.is_empty() {
                                                    crate::window::set_clipboard_text(&text);
                                                    buffer::push_undo(b);
                                                    buffer::delete_selection(b);
                                                }
                                                Ok(())
                                            });
                                        }
                                    }
                                }
                                "doc:copy" => {
                                    if let Some(doc) = docs.get(active_tab) { let dv = &doc.view;
                                        if let Some(buf_id) = dv.buffer_id {
                                            let _ = buffer::with_buffer(buf_id, |b| {
                                                let text = buffer::get_selected_text(b);
                                                if !text.is_empty() {
                                                    crate::window::set_clipboard_text(&text);
                                                }
                                                Ok(())
                                            });
                                        }
                                    }
                                }
                                "doc:paste" => {
                                    if let Some(doc) = docs.get_mut(active_tab) { let dv = &mut doc.view;
                                        if let Some(buf_id) = dv.buffer_id {
                                            if let Some(text) = crate::window::get_clipboard_text() {
                                                let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                    buffer::push_undo(b);
                                                    buffer::delete_selection(b);
                                                    let line = b.selections[0];
                                                    let col = b.selections[1];
                                                    if line <= b.lines.len() {
                                                        // Insert text at cursor, handling newlines.
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
                                        }
                                    }
                                }
                                "core:open-project-folder" => {
                                    cmdview_active = true;
                                    cmdview_mode = CmdViewMode::OpenFolder;
                                    cmdview_text = format!("{project_root}/");
                                    cmdview_label = "Open Folder:".to_string();
                                    cmdview_suggestions = path_suggest(&cmdview_text, &project_root, true);
                                    cmdview_selected = 0;
                                }
                                "core:open-file" | "core:open-file-from-project" => {
                                    cmdview_active = true;
                                    cmdview_mode = CmdViewMode::OpenFile;
                                    if let Some(doc) = docs.get(active_tab) {
                                        if let Some(pos) = doc.path.rfind('/') {
                                            cmdview_text = format!("{}/", &doc.path[..pos]);
                                        } else {
                                            cmdview_text = format!("{project_root}/");
                                        }
                                    } else {
                                        cmdview_text = format!("{project_root}/");
                                    }
                                    cmdview_label = "Open File:".to_string();
                                    cmdview_suggestions = path_suggest(&cmdview_text, &project_root, false);
                                    cmdview_selected = 0;
                                }
                                "core:open-file-dialog" => {
                                    if let Ok(output) = std::process::Command::new("zenity")
                                        .args(["--file-selection", "--title", "Open File"])
                                        .output()
                                    {
                                        if output.status.success() {
                                            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                                            if !path.is_empty() && open_file_into(&path, &mut docs) {
                                                active_tab = docs.len() - 1;
                                                autoreload.watch(&path);
                                            }
                                        }
                                    }
                                }
                                "core:open-folder-dialog" => {
                                    if let Ok(output) = std::process::Command::new("zenity")
                                        .args(["--file-selection", "--directory", "--title", "Open Project Folder"])
                                        .output()
                                    {
                                        if output.status.success() {
                                            let folder = String::from_utf8_lossy(&output.stdout).trim().to_string();
                                            if !folder.is_empty() && std::path::Path::new(&folder).is_dir() {
                                                if docs.iter().any(|d| doc_is_modified(d)) {
                                                    nag_active = true;
                                                    nag_message = "Unsaved changes. Save all before switching project?  [Y]es  [N]o  [Esc]Cancel".to_string();
                                                    nag_tab_to_close = None;
                                                } else {
                                                    for d in &docs { autoreload.unwatch(&d.path); }
                                                    docs.clear();
                                                    active_tab = 0;
                                                    project_root = folder;
                                                    sidebar_entries = scan_directory(&project_root, 0);
                                                    sidebar_visible = true;
                                                    let abs = std::fs::canonicalize(&project_root).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| project_root.clone());
                                                    recent_projects.retain(|p| p != &abs);
                                                    recent_projects.insert(0, abs);
                                                    if recent_projects.len() > 20 { recent_projects.truncate(20); }
                                                    let _ = crate::editor::storage::save_text(userdir_path, "session", "recent_projects", &serde_json::to_string(&recent_projects).unwrap_or_default());
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {
                                    if let Some(doc) = docs.get_mut(active_tab) {
                                        handle_doc_command(&mut doc.view, &cmd, &style, &doc.indent_type, doc.indent_size);
                                    }
                                    // Adjust inlay hints and mark LSP change only for editing commands.
                                    let is_edit_cmd = matches!(cmd.as_str(),
                                        "doc:newline" | "doc:newline-below" | "doc:newline-above"
                                        | "doc:backspace" | "doc:delete"
                                        | "doc:delete-to-previous-word-start" | "doc:delete-to-next-word-end"
                                        | "doc:indent" | "doc:unindent"
                                        | "doc:toggle-line-comments"
                                        | "doc:move-lines-up" | "doc:move-lines-down"
                                        | "doc:duplicate-lines" | "doc:delete-lines"
                                        | "doc:join-lines"
                                        | "core:sort-lines" | "doc:fold" | "doc:unfold" | "doc:unfold-all"
                                    );
                                    if is_edit_cmd && lsp_state.transport_id.is_some() && lsp_state.initialized {
                                        lsp_state.inlay_hints.clear();
                                        if let Some(doc) = docs.get(active_tab) {
                                            if !doc.path.is_empty() {
                                                lsp_state.last_change = Some(Instant::now());
                                                lsp_state.pending_change_uri = Some(path_to_uri(&doc.path));
                                                lsp_state.pending_change_version += 1;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    redraw = true;
                }
                EditorEvent::TextInput(text) => {
                    cursor_blink_reset = Instant::now();
                    // Forward text to terminal when focused.
                    if terminal.visible && terminal.focused {
                        if let Some(inst) = terminal.active_terminal() {
                            let _ = inst.inner.write(text.as_bytes());
                        }
                        redraw = true;
                        continue;
                    }
                    if cmdview_active {
                        let prev_text = cmdview_text.clone();
                        cmdview_text.push_str(text);
                        let dirs_only = cmdview_mode == CmdViewMode::OpenFolder;
                        if cmdview_text.is_empty() && dirs_only {
                            cmdview_suggestions = recent_projects.clone();
                        } else {
                            cmdview_suggestions = path_suggest(&cmdview_text, &project_root, dirs_only);
                        }
                        cmdview_selected = 0;
                        // Typeahead: auto-fill when exactly 1 suggestion, user is
                        // typing (not deleting), and text doesn't end with '/'.
                        if cmdview_suggestions.len() == 1
                            && cmdview_text.len() > prev_text.len()
                            && !cmdview_text.ends_with('/')
                        {
                            let suggestion = &cmdview_suggestions[0];
                            if suggestion.starts_with(&cmdview_text) {
                                cmdview_text = suggestion.clone();
                            }
                        }
                        redraw = true;
                        continue;
                    }
                    if project_search_active {
                        project_search_query.push_str(text);
                        project_search_results =
                            run_project_search(&project_search_query, &project_root);
                        project_search_selected = 0;
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
                    if nag_active {
                        redraw = true;
                        continue;
                    }
                    if find_active {
                        if find_focus_on_replace {
                            replace_query.push_str(text);
                        } else {
                            find_query.push_str(text);
                        }
                        redraw = true;
                        continue;
                    }
                    if let Some(doc) = docs.get_mut(active_tab) { let dv = &mut doc.view;
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
                                                let end = l.char_indices().nth(ccol).map(|(i,_)| i).unwrap_or(trimmed.len());
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
                                cursor_positions.sort_by(|a, b_pos| {
                                    a.1.cmp(&b_pos.1).then(a.2.cmp(&b_pos.2))
                                });
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
                        // Mark LSP change for debounced didChange (only for LSP-handled files).
                        if lsp_state.transport_id.is_some() && lsp_state.initialized {
                            lsp_state.inlay_hints.clear();
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
                                        let tid =
                                            lsp_state.transport_id.unwrap();
                                        let (cl, cc) = buffer::with_buffer(
                                            buf_id,
                                            |b| {
                                                let l = *b
                                                    .selections
                                                    .get(2)
                                                    .unwrap_or(&1);
                                                let c = *b
                                                    .selections
                                                    .get(3)
                                                    .unwrap_or(&1);
                                                Ok((l, c))
                                            },
                                        )
                                        .unwrap_or((1, 1));
                                        let uri = path_to_uri(&doc.path);
                                        let req_id = lsp_state.next_id();
                                        lsp_state.pending_requests.insert(
                                            req_id,
                                            "textDocument/completion"
                                                .to_string(),
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
                    redraw = true;
                }
                EditorEvent::MousePressed { button, x, y, .. } => {
                    cursor_blink_reset = Instant::now();
                    // Nag bar button click handling.
                    if nag_active && *button == MouseButton::Left {
                        use crate::editor::view::DrawContext as _;
                        let bar_h = style.font_height + style.padding_y * 2.0;
                        if *y < bar_h {
                            let msg_w = draw_ctx.font_width(style.font, &nag_message);
                            let btn_pad = style.padding_x;
                            let btn_h = style.font_height + style.padding_y;
                            let btn_y = style.padding_y * 0.5;
                            let _ = (btn_h, btn_y);
                            let mut bx = style.padding_x + msg_w + btn_pad * 2.0;
                            for (i, label) in ["Save", "Don't Save", "Cancel"].iter().enumerate() {
                                let lw = draw_ctx.font_width(style.font, label) + btn_pad * 2.0;
                                if *x >= bx && *x <= bx + lw {
                                    match i {
                                        0 => {
                                            // Save
                                            if let Some(idx) = nag_tab_to_close {
                                                if let Some(doc) = docs.get(idx) {
                                                    if let Some(buf_id) = doc.view.buffer_id {
                                                        let path = doc.path.clone();
                                                        if !path.is_empty() {
                                                            let _ = buffer::with_buffer(buf_id, |b| {
                                                                buffer::save_file(b, &path, b.crlf)
                                                                    .map_err(|_| buffer::BufferError::UnknownBuffer)
                                                            });
                                                        }
                                                    }
                                                }
                                                if let Some(d) = docs.get(idx) {
                                                    autoreload.unwatch(&d.path);
                                                }
                                                docs.remove(idx);
                                                if active_tab >= docs.len() && !docs.is_empty() {
                                                    active_tab = docs.len() - 1;
                                                }
                                            } else {
                                                for doc in &docs {
                                                    if doc_is_modified(doc) {
                                                        if let Some(buf_id) = doc.view.buffer_id {
                                                            let path = doc.path.clone();
                                                            if !path.is_empty() {
                                                                let _ = buffer::with_buffer(buf_id, |b| {
                                                                    buffer::save_file(b, &path, b.crlf)
                                                                        .map_err(|_| buffer::BufferError::UnknownBuffer)
                                                                });
                                                            }
                                                        }
                                                    }
                                                }
                                                quit = true;
                                            }
                                        }
                                        1 => {
                                            // Don't Save
                                            if let Some(idx) = nag_tab_to_close {
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
                                        _ => {} // Cancel - just dismiss
                                    }
                                    nag_active = false;
                                    nag_tab_to_close = None;
                                    #[allow(unused_assignments)]
                                    { redraw = true; }
                                    continue;
                                }
                                bx += lw + btn_pad;
                            }
                        }
                    }

                    // Context menu: left-click outside dismisses, right-click shows.
                    if context_menu.visible && *button == MouseButton::Left {
                        // Check if click is inside the context menu area.
                        let item_h = style.font_height + style.padding_y;
                        let menu_h = item_h * context_menu.items.len() as f64 + style.padding_y;
                        let menu_x = context_menu.position.x;
                        let menu_y = context_menu.position.y;
                        // Approximate menu width.
                        let menu_w = 200.0;
                        if *x >= menu_x && *x <= menu_x + menu_w
                            && *y >= menu_y && *y <= menu_y + menu_h
                        {
                            let idx = ((*y - menu_y - style.padding_y / 2.0) / item_h)
                                .floor() as usize;
                            if let Some(item) = context_menu.items.get(idx) {
                                if let Some(ref cmd) = item.command {
                                    let cmd = cmd.clone();
                                    context_menu.hide();
                                    if let Some(doc) = docs.get_mut(active_tab) {
                                        handle_doc_command(&mut doc.view, &cmd, &style, &doc.indent_type, doc.indent_size);
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
                        context_menu.show(*x, *y, items);
                        redraw = true;
                        continue;
                    }

                    let sidebar_w = if sidebar_visible { sidebar_width } else { 0.0 };

                    // Sidebar resize drag: click near the right edge.
                    if sidebar_visible && (*x - sidebar_w).abs() < 5.0 && *button == MouseButton::Left {
                        sidebar_dragging = true;
                        redraw = true;
                        continue;
                    }

                    // Sidebar click detection.
                    if sidebar_visible && *x < sidebar_w {
                        use crate::editor::view::DrawContext as _;
                        let ibf = style.icon_big_font;
                        let sidebar_toolbar_h = draw_ctx.font_height(ibf) + style.padding_y * 2.0;

                        // Toolbar button click.
                        if *y < sidebar_toolbar_h {
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
                                match cmd {
                                    "core:new-doc" => {
                                        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
                                        let mut dv = DocView::new();
                                        dv.buffer_id = Some(buf_id);
                                        docs.push(OpenDoc { view: dv, path: String::new(), name: "[new]".to_string(), saved_change_id: 1, saved_signature: buffer::content_signature(&["\n".to_string()]), indent_type: "soft".to_string(), indent_size: 2, git_changes: std::collections::HashMap::new() });
                                        active_tab = docs.len() - 1;
                                    }
                                    "core:open-file" => {
                                        cmdview_active = true;
                                        cmdview_mode = CmdViewMode::OpenFile;
                                        cmdview_text = format!("{project_root}/");
                                        cmdview_label = "Open File:".to_string();
                                        cmdview_suggestions = path_suggest(&cmdview_text, &project_root, false);
                                        cmdview_selected = 0;
                                    }
                                    "doc:save" => {
                                        if let Some(doc) = docs.get_mut(active_tab) {
                                            if let Some(buf_id) = doc.view.buffer_id {
                                                let path = doc.path.clone();
                                                if !path.is_empty() {
                                                    let _ = buffer::with_buffer(buf_id, |b| {
                                                        buffer::save_file(b, &path, b.crlf)
                                                            .map_err(|_| buffer::BufferError::UnknownBuffer)
                                                    });
                                                }
                                            }
                                        }
                                    }
                                    "find-replace:find" => {
                                        find_active = true;
                                        replace_active = false;
                                        find_focus_on_replace = false;
                                    }
                                    "core:find-command" => {
                                        palette_active = true;
                                        palette_query.clear();
                                        palette_results = all_commands.clone();
                                        palette_selected = 0;
                                    }
                                    "core:open-user-settings" => {
                                        let settings_path = format!("{userdir}/config.toml");
                                        if !std::path::Path::new(&settings_path).exists() {
                                            let _ = std::fs::write(&settings_path, NativeConfig::default_toml_template());
                                        }
                                        if open_file_into(&settings_path, &mut docs) {
                                            active_tab = docs.len() - 1;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            redraw = true;
                            continue;
                        }

                        let entry_h = style.font_height + style.padding_y;
                        let sidebar_dir_header_h = style.font_height + style.padding_y;
                        let click_idx =
                            ((*y - sidebar_toolbar_h - sidebar_dir_header_h + sidebar_scroll) / entry_h).floor() as usize;
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
                                    let children = scan_directory(&path, depth + 1);
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
                                    open_file_into(&entry_path, &mut docs);
                                    autoreload.watch(&entry_path);
                                    active_tab = docs.len() - 1;
                                }
                            }
                        }
                        redraw = true;
                        continue;
                    }

                    // Tab bar click detection.
                    let tab_h = if !docs.is_empty() { style.font_height + style.padding_y * 3.0 } else { 0.0 };
                    if *y < tab_h && !docs.is_empty() {
                        use crate::editor::view::DrawContext as _;
                        let close_btn_w = draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                        let mut tx = sidebar_w + style.padding_x;
                        let mut clicked_close = false;
                        for (i, doc) in docs.iter().enumerate() {
                            let tab_label = if doc_is_modified(doc) {
                                format!("*{}", doc.name)
                            } else {
                                doc.name.clone()
                            };
                            let tw = draw_ctx.font_width(style.font, &tab_label)
                                + style.padding_x * 2.0 + close_btn_w
                                + style.divider_size;
                            if *x >= tx && *x < tx + tw {
                                // Check if click is on the close button area.
                                let close_x = tx + tw - close_btn_w - style.divider_size;
                                if *x >= close_x {
                                    // Close this tab (with nag check).
                                    if doc_is_modified(doc) {
                                        nag_active = true;
                                        nag_message = format!("Save changes to {}?", doc.name);
                                        nag_tab_to_close = Some(i);
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
                                }
                                break;
                            }
                            tx += tw;
                        }
                        let _ = clicked_close;
                        redraw = true;
                        continue;
                    }
                    // Terminal click: focus the terminal panel.
                    if terminal.visible {
                        let (ww, wh, _, _) = crate::window::get_window_size();
                        let win_h = wh as f64;
                        let status_h_click =
                            style.font_height + style.padding_y * 2.0;
                        let terminal_h_click =
                            (win_h * 0.3).min(win_h - tab_h - status_h_click - 50.0).max(80.0);
                        let term_y_click = win_h - terminal_h_click - status_h_click;
                        if *y >= term_y_click && *y < win_h - status_h_click {
                            terminal.focused = true;
                            redraw = true;
                            continue;
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
                                    (wh as f64 * 0.3).min(wh as f64 - tab_h - st_h - 50.0).max(80.0)
                                } else {
                                    0.0
                                };
                                wh as f64 - tab_h - terminal_h_click - st_h
                            };
                            if let Some(doc) = docs.get_mut(active_tab) {
                                let dv = &mut doc.view;
                                let total_lines = buffer::with_buffer(
                                    dv.buffer_id.unwrap_or(0),
                                    |b| Ok(b.lines.len()),
                                ).unwrap_or(0);
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
                                    let relative_y = *y - mm_y;
                                    let clicked_line_offset = (relative_y / mlh).floor() as usize;
                                    let target_line = (minimap_start + clicked_line_offset).clamp(1, total_lines);
                                    let new_scroll = ((target_line as f64 - 1.0) * doc_line_h - dv.rect().h / 2.0).max(0.0);
                                    dv.scroll_y = new_scroll;
                                    dv.target_scroll_y = new_scroll;
                                }
                            }
                            redraw = true;
                            continue;
                        }
                    }

                    if let Some(doc) = docs.get_mut(active_tab) { let dv = &mut doc.view;
                        if let Some(buf_id) = dv.buffer_id {
                            let line_h = style.code_font_height * 1.2;
                            let gutter_w = dv.gutter_width;
                            let click_line = ((y - dv.rect().y + dv.scroll_y) / line_h).floor() as usize + 1;
                            let text_x_start = dv.rect().x + gutter_w + style.padding_x - dv.scroll_x;
                            let click_col = if *x > text_x_start {
                                use crate::editor::view::DrawContext as _;
                                buffer::with_buffer(buf_id, |b| {
                                    let line_idx = click_line.min(b.lines.len()).max(1);
                                    let text = b.lines[line_idx - 1].trim_end_matches('\n');
                                    let mut col = 1usize;
                                    let mut cx = text_x_start;
                                    for ch in text.chars() {
                                        let cw = draw_ctx.font_width(style.code_font, &ch.to_string());
                                        if cx + cw / 2.0 > *x {
                                            break;
                                        }
                                        cx += cw;
                                        col += 1;
                                    }
                                    Ok(col)
                                }).unwrap_or(1)
                            } else {
                                1usize
                            };
                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                let line = click_line.min(b.lines.len()).max(1);
                                let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                                let col = click_col.min(max_col);
                                b.selections[0] = line;
                                b.selections[1] = col;
                                b.selections[2] = line;
                                b.selections[3] = col;
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
                    // Tab drag reorder.
                    if let Some(drag_idx) = tab_dragging {
                        let tab_h = style.font_height + style.padding_y * 3.0;
                        if *y < tab_h {
                            use crate::editor::view::DrawContext as _;
                            let sidebar_w = if sidebar_visible { sidebar_width } else { 0.0 };
                            let close_w = draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                            let mut tx = sidebar_w + style.padding_x;
                            for (i, doc) in docs.iter().enumerate() {
                                let label = if doc_is_modified(doc) { format!("*{}", doc.name) } else { doc.name.clone() };
                                let tw = draw_ctx.font_width(style.font, &label) + style.padding_x * 2.0 + close_w + style.divider_size;
                                if *x >= tx && *x < tx + tw && i != drag_idx {
                                    docs.swap(i, drag_idx);
                                    tab_dragging = Some(i);
                                    active_tab = i;
                                    redraw = true;
                                    break;
                                }
                                tx += tw;
                            }
                        }
                        continue;
                    }
                    if sidebar_dragging {
                        sidebar_width = x.clamp(MIN_SIDEBAR_W, MAX_SIDEBAR_W);
                        redraw = true;
                    } else if editor_mouse_down {
                        // Drag selection: update cursor position while keeping anchor.
                        if let Some(doc) = docs.get_mut(active_tab) {
                            let dv = &mut doc.view;
                            if let Some(buf_id) = dv.buffer_id {
                                let line_h = style.code_font_height * 1.2;
                                let gutter_w = dv.gutter_width;
                                let drag_line = ((y - dv.rect().y + dv.scroll_y) / line_h).floor() as usize + 1;
                                let text_x_start = dv.rect().x + gutter_w + style.padding_x - dv.scroll_x;
                                let drag_col = if *x > text_x_start {
                                    use crate::editor::view::DrawContext as _;
                                    buffer::with_buffer(buf_id, |b| {
                                        let li = drag_line.min(b.lines.len()).max(1);
                                        let text = b.lines[li - 1].trim_end_matches('\n');
                                        let mut col = 1usize;
                                        let mut cx = text_x_start;
                                        for ch in text.chars() {
                                            let cw = draw_ctx.font_width(style.code_font, &ch.to_string());
                                            if cx + cw / 2.0 > *x { break; }
                                            cx += cw;
                                            col += 1;
                                        }
                                        Ok(col)
                                    }).unwrap_or(1)
                                } else { 1 };
                                let _ = buffer::with_buffer_mut(buf_id, |b| {
                                    let line = drag_line.min(b.lines.len()).max(1);
                                    let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                                    b.selections[2] = line;
                                    b.selections[3] = drag_col.min(max_col);
                                    Ok(())
                                });
                                redraw = true;
                            }
                        }
                    }
                    let sidebar_w = if sidebar_visible { sidebar_width } else { 0.0 };
                    if sidebar_visible && (*x - sidebar_w).abs() < 5.0 {
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
                            userdir_path, "session", "sidebar_width",
                            &sidebar_width.to_string(),
                        );
                    }
                    editor_mouse_down = false;
                    tab_dragging = None;
                    redraw = true;
                    continue;
                }
                EditorEvent::MouseWheel { y, .. } => {
                    let line_h = style.code_font_height * 1.2;
                    let scroll_amt = y * line_h * 3.0;
                    if let Some(doc) = docs.get_mut(active_tab) {
                        let dv = &mut doc.view;
                        dv.target_scroll_y = (dv.target_scroll_y - scroll_amt).max(0.0);
                    }
                    // Sidebar scrolls only when mouse is over it.
                    if sidebar_visible && mouse_x < sidebar_width {
                        sidebar_scroll = (sidebar_scroll - scroll_amt).max(0.0);
                    }
                    redraw = true;
                }
                _ => {
                    redraw = true;
                }
            }
        }

        // LSP: auto-start for the active file if no transport is running.
        if lsp_state.transport_id.is_none() {
            if let Some(doc) = docs.get(active_tab) {
                if !doc.path.is_empty() {
                    try_start_lsp(&doc.path, &mut lsp_state, &lsp_specs, userdir, config.verbose);
                }
            }
        }

        // LSP: poll transport and handle responses.
        if let Some(tid) = lsp_state.transport_id {
            // Check inlay hint retry timer.
            if let Some(retry_at) = lsp_state.inlay_retry_at {
                if Instant::now() >= retry_at {
                    lsp_state.inlay_retry_at = None;
                    lsp_state.inlay_hints.clear();
                    for doc in &docs {
                        if !doc.path.is_empty() {
                            let uri = path_to_uri(&doc.path);
                            let line_count = doc.view.buffer_id
                                .and_then(|id| buffer::with_buffer(id, |b| Ok(b.lines.len())).ok())
                                .unwrap_or(100);
                            let req_id = lsp_state.next_request_id;
                            lsp_state.next_request_id += 1;
                            lsp_state.pending_requests.insert(req_id, "textDocument/inlayHint".to_string());
                            let _ = lsp::send_message(tid, &lsp_inlay_hint_request(req_id, &uri, 0, line_count));
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
                                if doc.path.is_empty() { continue; }
                                let ext = doc.path.rsplit('.').next().unwrap_or("");
                                let Some(ft) = ext_to_lsp_filetype(ext) else { continue };
                                if ft != lsp_state.filetype { continue; }
                                if let Some(buf_id) = doc.view.buffer_id {
                                    let text = buffer::with_buffer(buf_id, |b| {
                                        Ok(b.lines.join(""))
                                    })
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
                                if ext_to_lsp_filetype(ext).map(|ft| ft == lsp_state.filetype).unwrap_or(false) {
                                    let uri = path_to_uri(&doc.path);
                                    let line_count = doc.view.buffer_id
                                        .and_then(|id| buffer::with_buffer(id, |b| Ok(b.lines.len())).ok())
                                        .unwrap_or(100);
                                    let req_id = lsp_state.next_id();
                                    lsp_state.pending_requests.insert(req_id, "textDocument/inlayHint".to_string());
                                    let _ = lsp::send_message(tid, &lsp_inlay_hint_request(req_id, &uri, 0, line_count));
                                }
                            }
                        }

                        // Handle inlay hint response.
                        if lsp_state.pending_requests.get(&id).map(|s| s.as_str())
                            == Some("textDocument/inlayHint")
                        {
                            lsp_state.pending_requests.remove(&id);
                            if msg.get("error").is_some() {
                                // Silently ignore LSP errors for inlay hints.
                            }
                            if let Some(result) = msg.get("result").and_then(|r| r.as_array()) {
                                lsp_state.inlay_hints.clear();
                                for hint in result {
                                    let line = hint.get("position").and_then(|p| p.get("line")).and_then(|l| l.as_i64()).unwrap_or(0) as usize;
                                    let col = hint.get("position").and_then(|p| p.get("character")).and_then(|c| c.as_i64()).unwrap_or(0) as usize;
                                    let label = if let Some(s) = hint.get("label").and_then(|l| l.as_str()) {
                                        s.to_string()
                                    } else if let Some(parts) = hint.get("label").and_then(|l| l.as_array()) {
                                        parts.iter().filter_map(|p| p.get("value").and_then(|v| v.as_str())).collect::<Vec<_>>().join("")
                                    } else {
                                        continue;
                                    };
                                    let padding_left = hint.get("paddingLeft").and_then(|p| p.as_bool()).unwrap_or(true);
                                    let padding_right = hint.get("paddingRight").and_then(|p| p.as_bool()).unwrap_or(false);
                                    let mut display = label;
                                    if padding_left { display = format!(" {display}"); }
                                    if padding_right { display = format!("{display} "); }
                                    lsp_state.inlay_hints.push(InlayHint { line, col, label: display });
                                }
                                // If 0 hints, server may still be loading. Retry in 3 seconds.
                                if lsp_state.inlay_hints.is_empty() && lsp_state.inlay_retry_count < 5 {
                                    lsp_state.inlay_retry_at = Some(Instant::now() + std::time::Duration::from_secs(3));
                                    lsp_state.inlay_retry_count += 1;
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
                                        r.get("items")
                                            .and_then(|v| v.as_array())
                                            .cloned()
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
                            if !items.is_empty() {
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
                            let contents = msg
                                .get("result")
                                .and_then(|r| r.get("contents"));
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
                                    r.as_array()
                                        .and_then(|a| a.first())
                                } else if r.is_object() {
                                    Some(r)
                                } else {
                                    None
                                }
                            });
                            if let Some(location) = loc {
                                let target_uri = location
                                    .get("uri")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
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
                                        open_file_into(&target_path, &mut docs);
                                        autoreload.watch(&target_path);
                                        docs.len() - 1
                                    };
                                    active_tab = tab_idx;
                                    // Set cursor to target position.
                                    if let Some(doc) = docs.get(active_tab) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            let _ = buffer::with_buffer_mut(
                                                buf_id,
                                                |b| {
                                                    let line =
                                                        target_line.min(b.lines.len()).max(1);
                                                    let max_col = char_count(
                                                        b.lines[line - 1]
                                                            .trim_end_matches('\n'),
                                                    ) + 1;
                                                    let col = target_col.min(max_col);
                                                    b.selections =
                                                        vec![line, col, line, col];
                                                    Ok(())
                                                },
                                            );
                                        }
                                    }
                                }
                            }
                            redraw = true;
                        }

                        // Handle implementation/typeDefinition/references responses.
                        // These return the same Location/Location[] format as definition.
                        let method_str = lsp_state.pending_requests.get(&id).cloned();
                        if matches!(method_str.as_deref(), Some("textDocument/implementation" | "textDocument/typeDefinition" | "textDocument/references")) {
                            lsp_state.pending_requests.remove(&id);
                            let result = msg.get("result");
                            let loc = result.and_then(|r| {
                                if r.is_array() { r.as_array().and_then(|a| a.first()) }
                                else if r.is_object() { Some(r) }
                                else { None }
                            });
                            if let Some(location) = loc {
                                let target_uri = location.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                                let target_line = location.get("range").and_then(|r| r.get("start")).and_then(|s| s.get("line")).and_then(|v| v.as_i64()).unwrap_or(0) as usize + 1;
                                let target_col = location.get("range").and_then(|r| r.get("start")).and_then(|s| s.get("character")).and_then(|v| v.as_i64()).unwrap_or(0) as usize + 1;
                                let target_path = uri_to_path(target_uri);
                                if !target_path.is_empty() {
                                    let existing = docs.iter().position(|d| d.path == target_path);
                                    let tab_idx = if let Some(idx) = existing { idx } else {
                                        open_file_into(&target_path, &mut docs);
                                        autoreload.watch(&target_path);
                                        docs.len() - 1
                                    };
                                    active_tab = tab_idx;
                                    if let Some(doc) = docs.get(active_tab) {
                                        if let Some(buf_id) = doc.view.buffer_id {
                                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                                let line = target_line.min(b.lines.len()).max(1);
                                                let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
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
                                                let start =
                                                    range.and_then(|r| r.get("start"));
                                                let end =
                                                    range.and_then(|r| r.get("end"));
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
                                                    end_line: end
                                                        .and_then(|s| s.get("line"))
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
                                                    message: d
                                                        .get("message")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("")
                                                        .to_string(),
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

        // LSP: flush debounced didChange after 300ms of no changes.
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
                                    let text = buffer::with_buffer(buf_id, |b| {
                                        Ok(b.lines.join(""))
                                    })
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
                                    let line_count = buffer::with_buffer(buf_id, |b| Ok(b.lines.len())).unwrap_or(100);
                                    let req_id = lsp_state.next_id();
                                    lsp_state.pending_requests.insert(req_id, "textDocument/inlayHint".to_string());
                                    let _ = lsp::send_message(tid, &lsp_inlay_hint_request(req_id, &uri, 0, line_count));
                                }
                            }
                        }
                    }
                }
                lsp_state.last_change = None;
            }
        }

        // Terminal: poll output from each pty.
        if terminal.visible {
            let mut dead_indices = Vec::new();
            for (i, inst) in terminal.terminals.iter_mut().enumerate() {
                inst.inner.poll();
                if !inst.inner.running {
                    dead_indices.push(i);
                } else if let Some(data) = inst.inner.read(4096) {
                    if !data.is_empty() {
                        inst.tbuf.process_output(&data);
                        redraw = true;
                    }
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
            let sidebar_w = if sidebar_visible { sidebar_width } else { 0.0 };

            let tab_h = if !docs.is_empty() {
                style.font_height + style.padding_y * 3.0
            } else {
                0.0
            };
            let terminal_h = if terminal.visible {
                (height * 0.3).min(height - tab_h - status_h - 50.0).max(80.0)
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
            if let Some(doc) = docs.get_mut(active_tab) {
                let dv = &mut doc.view;
                dv.set_rect(content_rect);
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
                for doc in &docs {
                    let doc_canon = std::fs::canonicalize(&doc.path)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| doc.path.clone());
                    if doc_canon == canonical {
                        if doc_is_modified(doc) {
                            reload_nag_active = true;
                            reload_nag_path = doc.path.clone();
                        } else if let Some(buf_id) = doc.view.buffer_id {
                            let path = doc.path.clone();
                            let _ = buffer::with_buffer_mut(buf_id, |b| {
                                let mut buf_state = buffer::default_buffer_state();
                                if buffer::load_file(&mut buf_state, &path).is_ok() {
                                    b.lines = buf_state.lines;
                                    b.change_id = buf_state.change_id;
                                }
                                Ok(())
                            });
                        }
                        redraw = true;
                        break;
                    }
                }
            }

            if redraw {
                // Update window title and status bar from active tab.
                let title = docs
                    .get(active_tab)
                    .map(|d| d.name.as_str())
                    .unwrap_or("Lite-Anvil");
                if window_title != title {
                    let display = if title.is_empty() {
                        "Lite-Anvil".to_string()
                    } else {
                        format!("{title} - Lite-Anvil")
                    };
                    crate::window::set_window_title(&display);
                    window_title = title.to_string();
                }
                status_view.left_items.clear();
                status_view.right_items.clear();
                if let Some(doc) = docs.get(active_tab) {
                    // Left: filename (with modified indicator).
                    let modified_label = if doc_is_modified(doc) {
                        format!("*{}", doc.name)
                    } else {
                        doc.name.clone()
                    };
                    status_view.left_items.push(StatusItem {
                        text: modified_label,
                        color: None,
                        command: None,
                    });
                    // Left: cursor position + document %.
                    if let Some(buf_id) = doc.view.buffer_id {
                        let (line, col, total) = buffer::with_buffer(buf_id, |b| {
                            Ok((*b.selections.get(2).unwrap_or(&1), *b.selections.get(3).unwrap_or(&1), b.lines.len()))
                        }).unwrap_or((1, 1, 1));
                        let pct = if total > 0 { (line * 100) / total } else { 100 };
                        status_view.left_items.push(StatusItem {
                            text: format!("  Ln {line}/{total}, Col {col}  ({pct}%)"),
                            color: Some(style.dim.to_array()),
                            command: None,
                        });
                    }
                    // Right side with separators: Lang | UTF-8 | Spaces: N | LF | INS
                    let ext = doc.path.rsplit('.').next().unwrap_or("");
                    let lang = match ext {
                        "rs" => "Rust", "py" => "Python", "js" => "JavaScript",
                        "ts" => "TypeScript", "c" => "C", "h" => "C",
                        "cpp" | "cc" => "C++", "hpp" => "C++", "go" => "Go",
                        "toml" => "TOML", "json" => "JSON", "yaml" | "yml" => "YAML",
                        "md" => "Markdown", "sh" | "bash" => "Shell", "html" => "HTML",
                        "css" => "CSS", "xml" => "XML", "java" => "Java",
                        "rb" => "Ruby", "php" => "PHP", "sql" => "SQL",
                        _ => if ext.is_empty() { "Plain Text" } else { ext },
                    };
                    let indent_label = if doc.indent_type == "hard" {
                        "Tabs".to_string()
                    } else {
                        format!("Spaces: {}", doc.indent_size)
                    };
                    let crlf = doc.view.buffer_id.and_then(|id| {
                        buffer::with_buffer(id, |b| Ok(b.crlf)).ok()
                    }).unwrap_or(false);
                    let le = if crlf { "CRLF" } else { "LF" };
                    let mode = if overwrite_mode { "OVR" } else { "INS" };
                    let sep = " | ";
                    let mut right_parts = vec![lang.to_string(), "UTF-8".to_string(), indent_label, le.to_string()];
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
                        text: "Lite-Anvil".to_string(),
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
                            let errors = diags
                                .iter()
                                .filter(|d| d.severity == 1)
                                .count();
                            let warnings = diags
                                .iter()
                                .filter(|d| d.severity == 2)
                                .count();
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

                // Smooth scroll interpolation.
                if let Some(doc) = docs.get_mut(active_tab) {
                    let dv = &mut doc.view;
                    let diff = dv.target_scroll_y - dv.scroll_y;
                    #[allow(unused_assignments)]
                    if diff.abs() > 0.5 {
                        dv.scroll_y += diff * 0.35;
                        redraw = true;
                    } else if diff.abs() > 0.01 {
                        dv.scroll_y = dv.target_scroll_y;
                    }
                }

                crate::renderer::native_begin_frame();
                crate::editor::app_state::clip_init(width, height);

                // Draw tab bar.
                let _tab_bar_h = if !docs.is_empty() {
                    let tbh = style.font_height + style.padding_y * 3.0;
                    let accent_h = 2.0;
                    use crate::editor::view::DrawContext as _;
                    draw_ctx.draw_rect(sidebar_w, 0.0, width - sidebar_w, tbh, style.background2.to_array());
                    let mut tx = sidebar_w + style.padding_x;
                    for (i, doc) in docs.iter().enumerate() {
                        let tab_label = if doc_is_modified(doc) {
                            format!("*{}", doc.name)
                        } else {
                            doc.name.clone()
                        };
                        let close_w = draw_ctx.font_width(style.icon_font, "C") + style.padding_x;
                        let tw = draw_ctx.font_width(style.font, &tab_label)
                            + style.padding_x * 2.0 + close_w;
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
                        draw_ctx.draw_rect(tx, accent_h, tw, tbh - accent_h, bg);
                        // Accent line on top of active tab.
                        if i == active_tab {
                            draw_ctx.draw_rect(tx, 0.0, tw, accent_h, style.accent.to_array());
                        }
                        let text_y_tab = accent_h + (tbh - accent_h - style.font_height) / 2.0;
                        draw_ctx.draw_text(
                            style.font,
                            &tab_label,
                            tx + style.padding_x,
                            text_y_tab,
                            fg,
                        );
                        // Close button with hover highlight.
                        let close_x = tx + tw - close_w;
                        let close_hovered = mouse_y < tbh && mouse_x >= close_x && mouse_x < close_x + close_w;
                        if close_hovered {
                            draw_ctx.draw_rect(close_x, accent_h, close_w, tbh - accent_h, style.line_highlight.to_array());
                        }
                        let close_color = if close_hovered { style.text.to_array() } else { style.dim.to_array() };
                        draw_ctx.draw_text(
                            style.icon_font,
                            "C",
                            close_x + style.padding_x * 0.5,
                            accent_h + (tbh - accent_h - draw_ctx.font_height(style.icon_font)) / 2.0,
                            close_color,
                        );
                        draw_ctx.draw_rect(
                            tx + tw,
                            style.padding_y * 0.5,
                            style.divider_size,
                            tbh - style.padding_y,
                            style.dim.to_array(),
                        );
                        tx += tw + style.divider_size;
                    }
                    draw_ctx.draw_rect(
                        sidebar_w,
                        tbh - style.divider_size,
                        width - sidebar_w,
                        style.divider_size,
                        style.divider.to_array(),
                    );
                    crate::editor::app_state::clip_init(width, height);
                    tbh
                } else {
                    0.0
                };

                // Draw breadcrumb bar above the document area.
                if let Some(doc) = docs.get(active_tab) {
                    use crate::editor::view::DrawContext as _;
                    let bc_y = tab_h;
                    draw_ctx.draw_rect(
                        sidebar_w,
                        bc_y,
                        width - sidebar_w - minimap_w,
                        breadcrumb_h,
                        style.background3.to_array(),
                    );
                    let segments: Vec<&str> = doc.path.split('/').filter(|s| !s.is_empty()).collect();
                    let mut bx = sidebar_w + style.padding_x;
                    let by = bc_y + style.padding_y * 0.25;
                    for (i, seg) in segments.iter().enumerate() {
                        let is_last = i == segments.len() - 1;
                        let color = if is_last {
                            style.text.to_array()
                        } else {
                            style.dim.to_array()
                        };
                        draw_ctx.draw_text(style.font, seg, bx, by, color);
                        bx += draw_ctx.font_width(style.font, seg);
                        if !is_last {
                            let arrow = " > ";
                            draw_ctx.draw_text(style.font, arrow, bx, by, style.dim.to_array());
                            bx += draw_ctx.font_width(style.font, arrow);
                        }
                    }
                }

                // Draw sidebar.
                if sidebar_visible {
                    use crate::editor::view::DrawContext as _;
                    draw_ctx.draw_rect(0.0, 0.0, sidebar_w, height, style.background2.to_array());

                    // Mini toolbar at top of sidebar (big icon font, matching 1.5.5).
                    let ibf = style.icon_big_font;
                    let icon_h = draw_ctx.font_height(ibf);
                    let toolbar_h = icon_h + style.padding_y * 2.0;
                    draw_ctx.draw_rect(0.0, 0.0, sidebar_w, toolbar_h, style.background3.to_array());
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
                        if bx + iw + icon_spacing > sidebar_w { break; }
                        draw_ctx.draw_text(ibf, icon, bx, btn_y, style.dim.to_array());
                        bx += iw + icon_spacing;
                    }
                    draw_ctx.draw_rect(0.0, toolbar_h - style.divider_size, sidebar_w, style.divider_size, style.divider.to_array());

                    // Project directory name header.
                    let dir_header_h = style.font_height + style.padding_y;
                    let resolved_root = std::fs::canonicalize(&project_root)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| project_root.clone());
                    let dir_name = resolved_root.rsplit('/').find(|s| !s.is_empty()).unwrap_or(&resolved_root);
                    let dir_label = dir_name.to_string();
                    draw_ctx.draw_rect(0.0, toolbar_h, sidebar_w, dir_header_h, style.background2.to_array());
                    draw_ctx.draw_text(style.font, &dir_label, style.padding_x, toolbar_h + (dir_header_h - style.font_height) / 2.0, style.accent.to_array());
                    draw_ctx.draw_rect(0.0, toolbar_h + dir_header_h - style.divider_size, sidebar_w, style.divider_size, style.divider.to_array());

                    // File tree entries.
                    let entry_h = style.font_height + style.padding_y;
                    let icon_font_h = draw_ctx.font_height(style.icon_font);
                    let icon_w = draw_ctx.font_width(style.icon_font, "D") + style.padding_x * 0.5;
                    let active_path = docs.get(active_tab).map(|d| d.path.as_str()).unwrap_or("");
                    let mut ey = toolbar_h + dir_header_h - sidebar_scroll;
                    let sidebar_content_top = toolbar_h + dir_header_h;
                    for entry in &sidebar_entries {
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
                            let icon = if entry.is_dir {
                                if entry.expanded { "D" } else { "d" }
                            } else {
                                "f"
                            };
                            let icon_color = if entry.is_dir {
                                style.accent.to_array()
                            } else {
                                style.dim.to_array()
                            };
                            let icon_y = ey + (entry_h - icon_font_h) / 2.0;
                            draw_ctx.draw_text(style.icon_font, icon, x, icon_y, icon_color);

                            // Name (vertically centered, same baseline alignment).
                            let name_x = x + icon_w;
                            let name_color = if entry.is_dir {
                                style.accent.to_array()
                            } else if is_active {
                                style.text.to_array()
                            } else {
                                style.text.to_array()
                            };
                            draw_ctx.draw_text(style.font, &entry.name, name_x, text_y, name_color);
                        }
                        ey += entry_h;
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
                        // Compile and cache syntax for this extension.
                        let compiled_opt = compiled_syntax_cache
                            .entry(ext.to_string())
                            .or_insert_with(|| {
                                let filename = doc
                                    .path
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(&doc.path);
                                let matched = match_syntax_for_file(filename, &syntax_defs);
                                matched.and_then(|def| {
                                    match tokenizer::compile_from_definition(def) {
                                        Ok(cs) => Some(cs),
                                        Err(e) => {
                                            log_to_file(userdir, &format!("Syntax compile error: {e:?}"));
                                            None
                                        }
                                    }
                                })
                            });
                        let wrap_w = if line_wrapping {
                            Some(dv.rect().w - dv.gutter_width - style.padding_x * 2.0)
                        } else {
                            None
                        };
                        let is_lsp_file = ext_to_lsp_filetype(ext)
                            .map(|ft| ft == lsp_state.filetype)
                            .unwrap_or(false);
                        let empty_hints = Vec::new();
                        let hints = if is_lsp_file { &lsp_state.inlay_hints } else { &empty_hints };
                        let render_lines = build_render_lines(
                            buf_id,
                            dv,
                            &style,
                            ext,
                            compiled_opt.as_ref(),
                            wrap_w,
                            hints,
                        );
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
                        // Draw bracket match underlines at cursor position.
                        if let Some(buf_id) = dv.buffer_id {
                            let bracket = buffer::with_buffer(buf_id, |b| {
                                Ok(crate::editor::picker::bracket_pair(
                                    &b.lines, cursor_line, cursor_col,
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
                                let y1 = doc_y + (l1 as f64 - 1.0) * line_h + line_h - 2.0
                                    - dv.scroll_y;
                                let x1 = doc_x + (c1 as f64 - 1.0) * char_w - dv.scroll_x;
                                if y1 >= doc_y && y1 <= doc_y + dv.rect().h {
                                    draw_ctx.draw_rect(x1, y1, char_w, 2.0, caret_color);
                                }
                                // Underline at second bracket.
                                let y2 = doc_y + (l2 as f64 - 1.0) * line_h + line_h - 2.0
                                    - dv.scroll_y;
                                let x2 = doc_x + (c2 as f64 - 1.0) * char_w - dv.scroll_x;
                                if y2 >= doc_y && y2 <= doc_y + dv.rect().h {
                                    draw_ctx.draw_rect(x2, y2, char_w, 2.0, caret_color);
                                }
                            }
                        }
                        // Draw diagnostic underlines from LSP (only for LSP-handled files).
                        if is_lsp_file {
                        if let Some(diags) = lsp_state.diagnostics.get(&doc.path) {
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
                                let y_pos = doc_y
                                    + (diag.start_line as f64) * line_h
                                    + line_h
                                    - 2.0
                                    - dv.scroll_y;
                                if y_pos < doc_y || y_pos > doc_y + dv.rect().h {
                                    continue;
                                }
                                use crate::editor::view::DrawContext as _;
                                let char_w = draw_ctx
                                    .font_width(style.code_font, "m");
                                let x1 = doc_x
                                    + diag.start_col as f64 * char_w
                                    - dv.scroll_x;
                                let x2 = doc_x
                                    + end_col as f64 * char_w
                                    - dv.scroll_x;
                                let w = (x2 - x1).max(char_w);
                                draw_ctx.draw_rect(x1, y_pos, w, 2.0, color);
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

                        let total_lines = buffer::with_buffer(
                            dv.buffer_id.unwrap_or(0),
                            |b| Ok(b.lines.len()),
                        )
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
                                    let y_pos = mm_y + (line_idx - minimap_start) as f64 * mlh + block_y_pad;
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
                                                    let trimmed = t.text.trim_start_matches([' ', '\t']);
                                                    let leading = text_len - trimmed.len();
                                                    let trimmed_draw = draw_len.saturating_sub(leading);
                                                    if trimmed_draw > 0 {
                                                        let seg_x = (x_off + leading as f64 * fixed_char_w).min(usable_w);
                                                        let seg_w = (trimmed_draw as f64 * fixed_char_w).min(usable_w - seg_x + text_padding);
                                                        if seg_w > 0.2 {
                                                            let mut color = syntax_color(&t.token_type, &style);
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
                                        let draw_len = trimmed.len().min((usable_w / fixed_char_w) as usize);
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

                // Draw terminal panel.
                if terminal.visible {
                    use crate::editor::view::DrawContext as _;
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
                    // Background.
                    draw_ctx.draw_rect(
                        term_x,
                        term_y + style.divider_size,
                        term_w,
                        terminal_h - style.divider_size,
                        [30, 30, 30, 255],
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
                    let char_h_resize = style.code_font_height * 1.2;
                    let char_w_resize = draw_ctx.font_width(style.code_font, "m");
                    if char_w_resize > 0.0 && char_h_resize > 0.0 {
                        let new_cols = ((term_w - style.padding_x * 2.0) / char_w_resize).max(1.0) as usize;
                        let new_rows = ((terminal_h - style.divider_size - style.padding_y) / char_h_resize).max(1.0) as usize;
                        if let Some(inst) = terminal.terminals.get_mut(terminal.active) {
                            inst.tbuf.resize(new_cols, new_rows);
                        }
                    }
                    // Draw terminal tab bar when multiple terminals exist.
                    let tab_bar_h = if terminal.terminals.len() > 1 {
                        let tbh = style.font_height + style.padding_y;
                        let tby = term_y + style.divider_size;
                        draw_ctx.draw_rect(term_x, tby, term_w, tbh, [40, 40, 40, 255]);
                        let mut tx = term_x + style.padding_x;
                        for (i, inst) in terminal.terminals.iter().enumerate() {
                            let label = &inst.title;
                            let lw = draw_ctx.font_width(style.font, label)
                                + style.padding_x * 2.0;
                            let color = if i == terminal.active {
                                style.accent.to_array()
                            } else {
                                [80, 80, 80, 255]
                            };
                            draw_ctx.draw_rect(tx, tby, lw, tbh, color);
                            draw_ctx.draw_text(
                                style.font,
                                label,
                                tx + style.padding_x,
                                tby + style.padding_y * 0.5,
                                style.text.to_array(),
                            );
                            tx += lw + 2.0;
                        }
                        tbh
                    } else {
                        0.0
                    };
                    // Draw active terminal buffer text using TerminalBufferInner cell grid.
                    if let Some(inst) = terminal.terminals.get(terminal.active) {
                        let char_h = style.code_font_height * 1.2;
                        let char_w = draw_ctx.font_width(style.code_font, "m");
                        let ty_start = term_y + style.divider_size + tab_bar_h + 2.0;

                        for (row_idx, row) in inst.tbuf.screen().iter().enumerate() {
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

                                // Draw bg if non-zero.
                                if let Some(bg_color) = bg {
                                    if bg_color[3] > 0 && bg_color != [0, 0, 0, 255] {
                                        draw_ctx.draw_rect(cx, ry, char_w, char_h, bg_color);
                                    }
                                }

                                // Batch text runs with same fg color.
                                if fg != run_fg && !run_text.is_empty() {
                                    draw_ctx.draw_text(style.code_font, &run_text, run_x, ry, run_fg);
                                    run_text.clear();
                                    run_x = cx;
                                    run_fg = fg;
                                }
                                if run_text.is_empty() {
                                    run_x = cx;
                                    run_fg = fg;
                                }
                                run_text.push(ch);

                                // Draw cursor block.
                                if terminal.focused
                                    && row_idx == inst.tbuf.cursor_row().saturating_sub(1)
                                    && col_idx == inst.tbuf.cursor_col().saturating_sub(1)
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
                    }
                }

                status_view.draw_native(&mut draw_ctx, &style);

                // Draw find bar (and optionally replace bar).
                if find_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let bar_h = style.font_height + style.padding_y * 2.0;
                    let total_bars = if replace_active { 2.0 } else { 1.0 };
                    let bar_y =
                        height - bar_h * total_bars - (style.font_height + style.padding_y * 2.0);

                    draw_ctx.draw_rect(0.0, bar_y, width, bar_h * total_bars, style.background3.to_array());
                    draw_ctx.draw_rect(0.0, bar_y, width, style.divider_size, style.divider.to_array());

                    let find_cursor = if !find_focus_on_replace { "_" } else { "" };
                    let find_label = format!("Find: {find_query}{find_cursor}");
                    draw_ctx.draw_text(
                        style.font,
                        &find_label,
                        style.padding_x,
                        bar_y + style.padding_y,
                        style.text.to_array(),
                    );

                    if replace_active {
                        let replace_y = bar_y + bar_h;
                        draw_ctx.draw_rect(
                            0.0,
                            replace_y,
                            width,
                            style.divider_size,
                            style.divider.to_array(),
                        );
                        let repl_cursor = if find_focus_on_replace { "_" } else { "" };
                        let repl_label =
                            format!("Replace: {replace_query}{repl_cursor}  (Ctrl+Enter to replace)");
                        draw_ctx.draw_text(
                            style.font,
                            &repl_label,
                            style.padding_x,
                            replace_y + style.padding_y,
                            style.text.to_array(),
                        );
                    }
                }

                // Draw nag bar if active.
                if nag_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    // Semi-transparent overlay dims the entire editor.
                    draw_ctx.draw_rect(0.0, 0.0, width, height, [0, 0, 0, 120]);
                    let bar_h = style.font_height + style.padding_y * 2.0;
                    draw_ctx.draw_rect(0.0, 0.0, width, bar_h, style.nagbar.to_array());
                    draw_ctx.draw_text(
                        style.font,
                        &nag_message,
                        style.padding_x,
                        style.padding_y,
                        style.nagbar_text.to_array(),
                    );
                    // Draw option buttons.
                    let msg_w = draw_ctx.font_width(style.font, &nag_message);
                    let btn_y = style.padding_y * 0.5;
                    let btn_h = style.font_height + style.padding_y;
                    let btn_pad = style.padding_x;
                    let mut bx = style.padding_x + msg_w + btn_pad * 2.0;
                    for label in &["Save", "Don't Save", "Cancel"] {
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
                        draw_ctx.draw_text(style.font, msg, style.padding_x, ty, [255, 255, 255, 255]);
                    }
                }

                // Draw reload nag bar if active.
                if reload_nag_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let bar_h = style.font_height + style.padding_y * 2.0;
                    draw_ctx.draw_rect(0.0, 0.0, width, bar_h, style.nagbar.to_array());
                    let msg = format!(
                        "File changed on disk: {}. Reload?  [Y]es  [N]o",
                        reload_nag_path
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

                    for (i, (_, display)) in
                        palette_results.iter().enumerate().take(max_visible)
                    {
                        let ry =
                            input_y + line_h + style.divider_size + i as f64 * line_h;
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
                if project_search_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let ps_w = (width * 0.6).max(500.0).min(width - 20.0);
                    let ps_x = (width - ps_w) / 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_visible = 15usize;
                    let visible_count = project_search_results.len().min(max_visible);
                    let ps_h = line_h * (visible_count as f64 + 1.0) + style.padding_y * 2.0;
                    let ps_y = style.padding_y * 2.0;

                    // Border + background.
                    draw_ctx.draw_rect(
                        ps_x - 1.0, ps_y - 1.0,
                        ps_w + 2.0, ps_h + 2.0,
                        style.divider.to_array(),
                    );
                    draw_ctx.draw_rect(ps_x, ps_y, ps_w, ps_h, style.background3.to_array());

                    // Input line.
                    let input_y = ps_y + style.padding_y;
                    let label = "Search: ";
                    let label_w = draw_ctx.font_width(style.font, label);
                    draw_ctx.draw_text(
                        style.font, label,
                        ps_x + style.padding_x, input_y,
                        style.accent.to_array(),
                    );
                    draw_ctx.draw_text(
                        style.font,
                        &format!("{}_", &project_search_query),
                        ps_x + style.padding_x + label_w + style.padding_x, input_y,
                        style.text.to_array(),
                    );

                    // Divider below input.
                    draw_ctx.draw_rect(
                        ps_x, input_y + line_h, ps_w, style.divider_size,
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
                        let ry = input_y + line_h + style.divider_size
                            + display_idx as f64 * line_h;
                        if i == project_search_selected {
                            draw_ctx.draw_rect(
                                ps_x, ry, ps_w, line_h,
                                style.selection.to_array(),
                            );
                        }
                        // Show path:line then the matched text.
                        let location = format!("{path}:{line_num}");
                        let loc_color = if i == project_search_selected {
                            style.accent.to_array()
                        } else {
                            style.dim.to_array()
                        };
                        draw_ctx.draw_text(
                            style.font, &location,
                            ps_x + style.padding_x,
                            ry + style.padding_y / 2.0,
                            loc_color,
                        );
                        let loc_w = draw_ctx.font_width(style.font, &location);
                        let text_color = style.text.to_array();
                        let max_text_w = ps_w - style.padding_x * 3.0 - loc_w;
                        let truncated: String = if max_text_w > 0.0 {
                            let char_w = draw_ctx.font_width(style.font, "m");
                            let max_chars =
                                (max_text_w / char_w).floor() as usize;
                            text.chars().take(max_chars).collect()
                        } else {
                            String::new()
                        };
                        draw_ctx.draw_text(
                            style.font, &format!("  {truncated}"),
                            ps_x + style.padding_x + loc_w,
                            ry + style.padding_y / 2.0,
                            text_color,
                        );
                    }
                }

                // Draw git status overlay.
                if git_status_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let gs_w = (width * 0.5).max(400.0).min(width - 20.0);
                    let gs_x = (width - gs_w) / 2.0;
                    let gs_y = style.padding_y * 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_vis = 20usize;
                    let vis = git_status_entries.len().min(max_vis);
                    let gs_h = line_h * (vis as f64 + 1.0) + style.padding_y * 2.0;
                    draw_ctx.draw_rect(gs_x - 1.0, gs_y - 1.0, gs_w + 2.0, gs_h + 2.0, style.divider.to_array());
                    draw_ctx.draw_rect(gs_x, gs_y, gs_w, gs_h, style.background3.to_array());
                    let input_y = gs_y + style.padding_y;
                    let title = format!("Git Status  ({} changed)  [R] refresh  [Enter] open  [Esc] close", git_status_entries.len());
                    draw_ctx.draw_text(style.font, &title, gs_x + style.padding_x, input_y, style.accent.to_array());
                    draw_ctx.draw_rect(gs_x, input_y + line_h, gs_w, style.divider_size, style.divider.to_array());
                    let scroll_off = if git_status_selected >= max_vis { git_status_selected - max_vis + 1 } else { 0 };
                    for (i, (code, _path, display)) in git_status_entries.iter().enumerate().skip(scroll_off).take(max_vis) {
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
                        draw_ctx.draw_text(style.font, display, gs_x + style.padding_x, ry + style.padding_y / 2.0, color);
                    }
                }

                // Draw command view (file/folder open with autocomplete) at top.
                if cmdview_active {
                    crate::editor::app_state::clip_init(width, height);
                    use crate::editor::view::DrawContext as _;
                    let cv_w = (width * 0.5).max(400.0).min(width - 20.0);
                    let cv_x = (width - cv_w) / 2.0;
                    let line_h = style.font_height + style.padding_y;
                    let max_visible = 15usize;
                    let visible_count = cmdview_suggestions.len().min(max_visible);
                    let cv_h = line_h * (visible_count as f64 + 1.0) + style.padding_y * 2.0;
                    let cv_y = style.padding_y * 2.0;

                    // Border + background.
                    draw_ctx.draw_rect(cv_x - 1.0, cv_y - 1.0, cv_w + 2.0, cv_h + 2.0, style.divider.to_array());
                    draw_ctx.draw_rect(cv_x, cv_y, cv_w, cv_h, style.background3.to_array());

                    // Input line.
                    let input_y = cv_y + style.padding_y;
                    let label = &cmdview_label;
                    let label_w = draw_ctx.font_width(style.font, label);
                    draw_ctx.draw_text(style.font, label, cv_x + style.padding_x, input_y, style.accent.to_array());
                    draw_ctx.draw_text(style.font, &format!("{}_", &cmdview_text), cv_x + style.padding_x + label_w + style.padding_x, input_y, style.text.to_array());

                    // Divider below input.
                    draw_ctx.draw_rect(cv_x, input_y + line_h, cv_w, style.divider_size, style.divider.to_array());

                    // Scroll offset so selected item is visible.
                    let scroll_off = if cmdview_selected >= max_visible {
                        cmdview_selected - max_visible + 1
                    } else { 0 };

                    // Suggestions list.
                    for (i, suggestion) in cmdview_suggestions.iter().enumerate().skip(scroll_off).take(max_visible) {
                        let display_idx = i - scroll_off;
                        let ry = input_y + line_h + style.divider_size + display_idx as f64 * line_h;
                        if i == cmdview_selected {
                            draw_ctx.draw_rect(cv_x, ry, cv_w, line_h, style.selection.to_array());
                        }
                        let is_dir = suggestion.ends_with('/');
                        let color = if i == cmdview_selected || is_dir {
                            style.accent.to_array()
                        } else {
                            style.text.to_array()
                        };
                        draw_ctx.draw_text(style.font, suggestion, cv_x + style.padding_x, ry + style.padding_y / 2.0, color);
                    }
                }

                // Draw LSP completion popup.
                if completion.visible && !completion.items.is_empty() {
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
                        let popup_y = dv.rect().y
                            + completion.line as f64 * line_h_comp
                            - dv.scroll_y;
                        let item_h = style.font_height + style.padding_y;
                        let popup_h =
                            item_h * completion.items.len() as f64 + style.padding_y;
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
                                let label_w =
                                    draw_ctx.font_width(style.font, label);
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
                if hover.visible && !hover.text.is_empty() {
                    if let Some(doc) = docs.get(active_tab) {
                        let dv = &doc.view;
                        crate::editor::app_state::clip_init(width, height);
                        use crate::editor::view::DrawContext as _;
                        let line_h_hover = style.code_font_height * 1.2;
                        let gutter_w = dv.gutter_width;
                        let hover_x = dv.rect().x
                            + gutter_w
                            + style.padding_x
                            + (hover.col as f64 - 1.0)
                                * draw_ctx.font_width(style.code_font, "m")
                            - dv.scroll_x;
                        let hover_y = dv.rect().y
                            + (hover.line as f64 - 1.0) * line_h_hover
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
                                        .map(|chunk| {
                                            std::str::from_utf8(chunk).unwrap_or("")
                                        })
                                        .collect()
                                }
                            })
                            .take(15)
                            .collect();
                        let line_count_h = hover_lines.len();
                        let tooltip_line_h = style.font_height + 2.0;
                        let tooltip_h = tooltip_line_h * line_count_h as f64
                            + style.padding_y * 2.0;
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

    // Session save: persist open files, active tab, and project root via storage.
    let open_files: Vec<String> = docs
        .iter()
        .filter(|d| !d.path.is_empty())
        .map(|d| d.path.clone())
        .collect();
    if !open_files.is_empty() {
        let session = SessionData {
            files: open_files,
            active: active_tab,
            active_project: project_root.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&session) {
            if let Err(e) = storage::save_text(userdir_path, "session", "files", &json) {
                eprintln!("[native] Failed to save session: {e}");
            }
        }
    } else if let Err(e) = storage::clear(userdir_path, "session", Some("files")) {
        eprintln!("[native] Failed to clear session: {e}");
    }

    // Save window size and position.
    let (pw, ph, wx, wy) = crate::window::get_window_size();
    let win_json = serde_json::json!({ "w": pw, "h": ph, "x": wx, "y": wy });
    if let Err(e) = storage::save_text(
        userdir_path,
        "session",
        "window",
        &win_json.to_string(),
    ) {
        eprintln!("[native] Failed to save window size: {e}");
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

/// Filter command list using fuzzy matching from the picker module.
#[cfg(feature = "sdl")]
fn fuzzy_filter_commands(
    query: &str,
    all_commands: &[(String, String)],
) -> Vec<(String, String)> {
    if query.is_empty() {
        return all_commands.to_vec();
    }
    let display_strings: Vec<String> = all_commands.iter().map(|(_, d)| d.clone()).collect();
    let ranked = picker::rank_strings(display_strings, query, false, &[], None);
    ranked
        .into_iter()
        .filter_map(|display| {
            all_commands
                .iter()
                .find(|(_, d)| *d == display)
                .cloned()
        })
        .collect()
}

/// Find the next occurrence of query in the document, starting from cursor.
#[cfg(feature = "sdl")]
fn find_next_in_doc(dv: &mut DocView, query: &str) {
    if query.is_empty() { return; }
    let Some(buf_id) = dv.buffer_id else { return };
    let _ = buffer::with_buffer_mut(buf_id, |b| {
        let start_line = *b.selections.get(2).unwrap_or(&1);
        let start_col = *b.selections.get(3).unwrap_or(&1);
        let line_count = b.lines.len();

        // Search from cursor position forward, wrapping around.
        for offset in 0..line_count {
            let ln = ((start_line - 1 + offset) % line_count) + 1;
            let text = b.lines[ln - 1].trim_end_matches('\n');
            let search_from = if ln == start_line {
                
                text.char_indices()
                    .nth(start_col.saturating_sub(1))
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            } else {
                0
            };
            if let Some(byte_pos) = text[search_from..].find(query) {
                let abs_byte = search_from + byte_pos;
                let col = text[..abs_byte].chars().count() + 1;
                let end_col = col + query.chars().count();
                b.selections = vec![ln, col, ln, end_col];
                return Ok(());
            }
        }
        Ok(())
    });
    // Auto-scroll to found position.
    let _ = buffer::with_buffer(dv.buffer_id.unwrap(), |b| {
        let cursor_line = *b.selections.get(2).unwrap_or(&1);
        let line_h = 20.0; // approximate
        let cursor_y = (cursor_line as f64 - 1.0) * line_h;
        let view_h = dv.rect().h;
        if cursor_y < dv.scroll_y || cursor_y + line_h > dv.scroll_y + view_h {
            let new_scroll = (cursor_y - view_h / 2.0).max(0.0);
            dv.scroll_y = new_scroll;
            dv.target_scroll_y = new_scroll;
        }
        Ok(())
    });
}

/// Replace the current selection (match) with replacement text.
#[cfg(feature = "sdl")]
fn replace_current_match(dv: &mut DocView, find_query: &str, replacement: &str) {
    if find_query.is_empty() {
        return;
    }
    let Some(buf_id) = dv.buffer_id else { return };
    let _ = buffer::with_buffer_mut(buf_id, |b| {
        let selected = buffer::get_selected_text(b);
        if selected != find_query {
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
#[cfg(feature = "sdl")]
fn handle_doc_command(
    dv: &mut DocView,
    cmd: &str,
    style: &StyleContext,
    indent_type: &str,
    indent_size: usize,
) {
    let Some(buf_id) = dv.buffer_id else { return };
    let line_h = style.code_font_height * 1.2;

    let _ = buffer::with_buffer_mut(buf_id, |b| {
        let anchor_line = *b.selections.first().unwrap_or(&1);
        let anchor_col = *b.selections.get(1).unwrap_or(&1);
        let cursor_line = *b.selections.get(2).unwrap_or(&anchor_line);
        let cursor_col = *b.selections.get(3).unwrap_or(&anchor_col);
        let line_count = b.lines.len();

        // Selection: shift variants move cursor but keep anchor.
        let is_select = cmd.starts_with("doc:select-to-");

        // Movement always operates on the cursor position.
        let mut line = cursor_line;
        let mut col = cursor_col;

        match cmd {
            "doc:select-none" => {
                if buffer::cursor_count(b) > 1 {
                    buffer::remove_extra_cursors(b);
                    return Ok(());
                }
                // Collapse selection to cursor.
            }
            "doc:create-cursor-previous-line" => {
                let last_idx = b.selections.len() - 4;
                let last_line = b.selections[last_idx + 2];
                let last_col = b.selections[last_idx + 3];
                if last_line > 1 {
                    let new_line = last_line - 1;
                    let max_col =
                        char_count(b.lines[new_line - 1].trim_end_matches('\n')) + 1;
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
                    let max_col =
                        char_count(b.lines[new_line - 1].trim_end_matches('\n')) + 1;
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
            "doc:move-to-previous-line" | "doc:select-to-previous-line" => {
                if line > 1 {
                    line -= 1;
                    let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                    col = col.min(max_col);
                }
            }
            "doc:move-to-next-line" | "doc:select-to-next-line" => {
                if line < line_count {
                    line += 1;
                    let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                    col = col.min(max_col);
                }
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
                    while i > 0 && chars[i].is_whitespace() { i -= 1; }
                    // Skip word chars backwards.
                    while i > 0 && !chars[i - 1].is_whitespace() && chars[i - 1].is_alphanumeric() || chars.get(i.wrapping_sub(1)).is_some_and(|c| *c == '_') {
                        if i == 0 { break; }
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
                    while i < max && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                    // Skip whitespace forward.
                    while i < max && chars[i].is_whitespace() { i += 1; }
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
                while i > 0 && chars[i].is_whitespace() { i -= 1; }
                while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') { i -= 1; }
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
                while i < max && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                while i < max && chars[i].is_whitespace() { i += 1; }
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
                    if line > b.lines.len() { line = b.lines.len(); }
                    let max_col = char_count(b.lines[line - 1].trim_end_matches('\n')) + 1;
                    col = col.min(max_col);
                } else {
                    b.lines[0] = "\n".to_string();
                    col = 1;
                }
            }
            "doc:move-lines-up" => {
                if line > 1 {
                    buffer::push_undo(b);
                    b.lines.swap(line - 1, line - 2);
                    line -= 1;
                }
            }
            "doc:move-lines-down" => {
                if line < line_count {
                    buffer::push_undo(b);
                    b.lines.swap(line - 1, line);
                    line += 1;
                }
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
                            let prev_len =
                                char_count(b.lines[new_line - 1].trim_end_matches('\n'));
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
                        let max_c =
                            char_count(b.lines[cline - 1].trim_end_matches('\n')) + 1;
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
                let (start, end) = if anchor_line != cursor_line
                    || anchor_col != cursor_col
                {
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
            "doc:upper-case" | "doc:lower-case" => {
                if anchor_line != cursor_line || anchor_col != cursor_col {
                    buffer::push_undo(b);
                    let (s_line, s_col, e_line, e_col) = if anchor_line < cursor_line || (anchor_line == cursor_line && anchor_col <= cursor_col) {
                        (anchor_line, anchor_col, cursor_line, cursor_col)
                    } else {
                        (cursor_line, cursor_col, anchor_line, anchor_col)
                    };
                    let is_upper = cmd == "doc:upper-case";
                    if s_line == e_line {
                        let l = &mut b.lines[s_line - 1];
                        let start_byte = l.char_indices().nth(s_col - 1).map(|(i,_)| i).unwrap_or(0);
                        let end_byte = l.char_indices().nth(e_col - 1).map(|(i,_)| i).unwrap_or(l.len());
                        let fragment = &l[start_byte..end_byte];
                        let converted = if is_upper { fragment.to_uppercase() } else { fragment.to_lowercase() };
                        l.replace_range(start_byte..end_byte, &converted);
                    } else {
                        for li in s_line..=e_line {
                            let l = &mut b.lines[li - 1];
                            let start = if li == s_line { l.char_indices().nth(s_col - 1).map(|(i,_)| i).unwrap_or(0) } else { 0 };
                            let end = if li == e_line { l.char_indices().nth(e_col - 1).map(|(i,_)| i).unwrap_or(l.len()) } else { l.trim_end_matches('\n').len() };
                            let fragment = &l[start..end];
                            let converted = if is_upper { fragment.to_uppercase() } else { fragment.to_lowercase() };
                            l.replace_range(start..end, &converted);
                        }
                    }
                }
            }
            "doc:toggle-line-comments" => {
                buffer::push_undo(b);
                let comment = match std::path::Path::new(indent_type).extension().and_then(|e| e.to_str()) {
                    _ => "// " // default; indent_type is not ext, use file ext from caller context
                };
                // Determine comment prefix from common languages.
                // (indent_type field is reused here; the actual ext-based prefix is set below)
                let (start, end) = if anchor_line != cursor_line {
                    let s = anchor_line.min(cursor_line);
                    let e = anchor_line.max(cursor_line);
                    (s, e)
                } else {
                    (line, line)
                };
                // Check if all lines are already commented.
                let all_commented = (start..=end).all(|i| {
                    b.lines.get(i - 1).map(|l| l.trim_start().starts_with(comment.trim())).unwrap_or(false)
                });
                let _ = comment;
                // Use "//" for most languages, "#" for python/shell/toml/yaml, "--" for lua/sql
                // We don't have access to file ext here directly, so use a simple heuristic.
                // The caller should ideally pass the ext. For now use "//" as default.
                if all_commented {
                    for i in start..=end {
                        if let Some(l) = b.lines.get_mut(i - 1) {
                            if let Some(pos) = l.find("// ") {
                                l.replace_range(pos..pos + 3, "");
                            } else if let Some(pos) = l.find("//") {
                                l.replace_range(pos..pos + 2, "");
                            }
                        }
                    }
                } else {
                    for i in start..=end {
                        if let Some(l) = b.lines.get_mut(i - 1) {
                            let indent_len = l.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                            let byte = l.char_indices().nth(indent_len).map(|(i,_)| i).unwrap_or(0);
                            l.insert_str(byte, "// ");
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
                            if l.starts_with('\t') { l.remove(0); }
                        } else {
                            let remove = l.chars().take(indent_size).take_while(|c| *c == ' ').count();
                            if remove > 0 { l.replace_range(..remove, ""); }
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
                    if l.ends_with('\n') { l.pop(); }
                    if !l.ends_with(' ') && !trimmed.is_empty() { l.push(' '); }
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

    // Auto-scroll to keep cursor visible.
    let _ = buffer::with_buffer(buf_id, |b| {
        let cursor_line = *b.selections.get(2).unwrap_or(&1);
        let cursor_y = (cursor_line as f64 - 1.0) * line_h;
        let view_h = dv.rect().h;
        if cursor_y < dv.scroll_y {
            dv.scroll_y = cursor_y;
            dv.target_scroll_y = cursor_y;
        } else if cursor_y + line_h > dv.scroll_y + view_h {
            dv.scroll_y = cursor_y + line_h - view_h;
            dv.target_scroll_y = dv.scroll_y;
        }
        Ok(())
    });

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
                dv.folds.retain(|(s, e)| !(cursor_line >= *s && cursor_line <= *e));
                Ok(())
            });
        }
        "doc:unfold-all" => {
            dv.folds.clear();
        }
        _ => {}
    }
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
            let a = if hex.len() == 8 { u8::from_str_radix(&hex[6..8], 16).ok()? } else { 255 };
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

thread_local! {
    static SYNTAX_COLORS: std::cell::RefCell<std::collections::HashMap<String, [u8; 4]>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Get syntax color from loaded theme, with fallback.
fn syntax_color(token_type: &str, style: &StyleContext) -> [u8; 4] {
    SYNTAX_COLORS.with(|s| {
        let colors = s.borrow();
        if let Some(c) = colors.get(token_type) {
            return *c;
        }
        // Markdown emphasis types fall back to keyword2 (matching 1.5.5 behavior).
        if token_type.starts_with("markdown_") {
            if let Some(c) = colors.get("keyword2") {
                return *c;
            }
        }
        // Fallback: check "normal" key for symbol/operator.
        if let Some(c) = colors.get("normal") {
            if token_type == "symbol" || token_type == "operator" {
                return *c;
            }
        }
        style.text.to_array()
    })
}

/// Classify a word as a syntax token type based on common keywords.
fn classify_word(word: &str, ext: &str) -> &'static str {
    match ext {
        "rs" => match word {
            "fn" | "let" | "mut" | "pub" | "use" | "mod" | "struct" | "enum" | "impl" | "trait"
            | "for" | "while" | "loop" | "if" | "else" | "match" | "return" | "break" | "continue"
            | "where" | "type" | "const" | "static" | "ref" | "self" | "Self" | "super" | "crate"
            | "as" | "in" | "move" | "async" | "await" | "unsafe" | "extern" | "dyn" | "true" | "false" => "keyword",
            "bool" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
            | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
            | "f32" | "f64" | "str" | "String" | "Option" | "Result" | "Vec" | "Box" | "Arc"
            | "Mutex" | "HashMap" | "Ok" | "Err" | "Some" | "None" => "keyword2",
            _ => "normal",
        },
        "lua" => match word {
            "local" | "function" | "end" | "if" | "then" | "else" | "elseif" | "for" | "while"
            | "do" | "repeat" | "until" | "return" | "break" | "in" | "and" | "or" | "not"
            | "true" | "false" | "nil" => "keyword",
            _ => "normal",
        },
        "py" => match word {
            "def" | "class" | "if" | "elif" | "else" | "for" | "while" | "return" | "import"
            | "from" | "as" | "try" | "except" | "finally" | "with" | "yield" | "lambda"
            | "and" | "or" | "not" | "in" | "is" | "True" | "False" | "None" | "pass" | "break"
            | "continue" | "raise" | "global" | "nonlocal" | "async" | "await" => "keyword",
            _ => "normal",
        },
        "js" | "ts" | "jsx" | "tsx" => match word {
            "function" | "var" | "let" | "const" | "if" | "else" | "for" | "while" | "do"
            | "switch" | "case" | "break" | "continue" | "return" | "new" | "delete" | "typeof"
            | "instanceof" | "class" | "extends" | "import" | "export" | "default" | "from"
            | "try" | "catch" | "finally" | "throw" | "async" | "await" | "yield"
            | "true" | "false" | "null" | "undefined" | "this" | "super" => "keyword",
            _ => "normal",
        },
        "c" | "h" | "cpp" | "hpp" | "cc" => match word {
            "if" | "else" | "for" | "while" | "do" | "switch" | "case" | "break" | "continue"
            | "return" | "struct" | "enum" | "union" | "typedef" | "static" | "const" | "extern"
            | "void" | "int" | "char" | "float" | "double" | "long" | "short" | "unsigned"
            | "signed" | "sizeof" | "NULL" | "true" | "false" | "class" | "public" | "private"
            | "protected" | "virtual" | "override" | "template" | "typename" | "namespace"
            | "using" | "new" | "delete" | "throw" | "try" | "catch" | "#include" | "#define" => "keyword",
            _ => "normal",
        },
        "toml" => match word {
            "true" | "false" => "keyword",
            _ => "normal",
        },
        _ => "normal",
    }
}

/// Tokenize a line into colored tokens using simple keyword + string/comment detection.
fn simple_tokenize(line: &str, ext: &str, style: &StyleContext) -> Vec<RenderToken> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    let mut current = String::new();
    let mut in_string: Option<char> = None;
    let mut in_line_comment = false;

    while let Some(&ch) = chars.peek() {
        if in_line_comment {
            current.push(ch);
            chars.next();
            continue;
        }

        if let Some(quote) = in_string {
            current.push(ch);
            chars.next();
            if ch == quote {
                tokens.push(RenderToken { text: current.clone(), color: syntax_color("string", style) });
                current.clear();
                in_string = None;
            }
            continue;
        }

        // Check for line comments.
        if ch == '/' {
            let mut peek = chars.clone();
            peek.next();
            if peek.peek() == Some(&'/') {
                if !current.is_empty() {
                    let tt = classify_word(&current, ext);
                    tokens.push(RenderToken { text: current.clone(), color: syntax_color(tt, style) });
                    current.clear();
                }
                in_line_comment = true;
                current.push(ch);
                chars.next();
                continue;
            }
        }
        if ch == '#' && (ext == "py" || ext == "toml" || ext == "sh" || ext == "yml" || ext == "yaml") {
            if !current.is_empty() {
                let tt = classify_word(&current, ext);
                tokens.push(RenderToken { text: current.clone(), color: syntax_color(tt, style) });
                current.clear();
            }
            in_line_comment = true;
            current.push(ch);
            chars.next();
            continue;
        }
        if ch == '-' && ext == "lua" {
            let mut peek = chars.clone();
            peek.next();
            if peek.peek() == Some(&'-') {
                if !current.is_empty() {
                    let tt = classify_word(&current, ext);
                    tokens.push(RenderToken { text: current.clone(), color: syntax_color(tt, style) });
                    current.clear();
                }
                in_line_comment = true;
                current.push(ch);
                chars.next();
                continue;
            }
        }

        // Strings.
        if ch == '"' || ch == '\'' {
            if !current.is_empty() {
                let tt = classify_word(&current, ext);
                tokens.push(RenderToken { text: current.clone(), color: syntax_color(tt, style) });
                current.clear();
            }
            in_string = Some(ch);
            current.push(ch);
            chars.next();
            continue;
        }

        // Numbers.
        if ch.is_ascii_digit() && current.is_empty() {
            let mut num = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == 'x' || c == 'b' {
                    num.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(RenderToken { text: num, color: syntax_color("number", style) });
            continue;
        }

        // Word boundary.
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
            chars.next();
        } else {
            if !current.is_empty() {
                let tt = classify_word(&current, ext);
                tokens.push(RenderToken { text: current.clone(), color: syntax_color(tt, style) });
                current.clear();
            }
            tokens.push(RenderToken { text: ch.to_string(), color: syntax_color("symbol", style) });
            chars.next();
        }
    }

    // Flush remaining.
    if !current.is_empty() {
        let color = if in_line_comment {
            syntax_color("comment", style)
        } else if in_string.is_some() {
            syntax_color("string", style)
        } else {
            let tt = classify_word(&current, ext);
            syntax_color(tt, style)
        };
        tokens.push(RenderToken { text: current, color });
    }

    tokens
}

/// Build render lines from buffer for the visible range, with syntax highlighting.
#[cfg(feature = "sdl")]
fn build_render_lines(
    buf_id: u64,
    dv: &DocView,
    style: &StyleContext,
    file_ext: &str,
    compiled: Option<&CompiledSyntax>,
    wrap_width: Option<f64>,
    inlay_hints: &[InlayHint],
) -> Vec<RenderLine> {
    let line_h = style.code_font_height * 1.2;
    let visible_lines = ((dv.rect().h / line_h).ceil() as usize).max(1);
    let hint_color = SYNTAX_COLORS.with(|s| {
        s.borrow().get("comment").copied().unwrap_or(style.dim.to_array())
    });

    buffer::with_buffer(buf_id, |b| {
        let first = ((dv.scroll_y / line_h).floor() as usize).max(0) + 1;
        let last = (first + visible_lines + 1).min(b.lines.len());
        let mut render = Vec::new();
        let mut i = first;
        while i <= last && i <= b.lines.len() {
            // Skip folded lines.
            let mut folded = false;
            for (fs, fe) in &dv.folds {
                if i > *fs && i <= *fe {
                    folded = true;
                    break;
                }
            }
            if folded {
                i += 1;
                continue;
            }
            let raw_line = &b.lines[i - 1];
            let text = raw_line.trim_end_matches('\n');
            let mut tokens: Vec<RenderToken> = if let Some(syntax) = compiled {
                let toks = tokenizer::tokenize_line(syntax, raw_line);
                toks.iter()
                    .map(|t| {
                        let trimmed = t.text.trim_end_matches('\n').to_string();
                        // Rust attributes (#[...]) should render as normal/white, not keyword blue.
                        let tt = if t.token_type == "keyword" && trimmed.starts_with("#[") {
                            "attribute"
                        } else {
                            &t.token_type
                        };
                        RenderToken { text: trimmed, color: syntax_color(tt, style) }
                    })
                    .collect()
            } else {
                simple_tokenize(text, file_ext, style)
            };

            // Bracket pair colorization: color ()[]{}  by nesting depth.
            let bracket_colors: [Option<[u8; 4]>; 3] = SYNTAX_COLORS.with(|s| {
                let c = s.borrow();
                [c.get("bracket1").copied(), c.get("bracket2").copied(), c.get("bracket3").copied()]
            });
            if bracket_colors[0].is_some() {
                let mut depth: usize = 0;
                let mut new_tokens = Vec::with_capacity(tokens.len());
                for tok in tokens {
                    let has_bracket = tok.text.contains(|c: char| matches!(c, '('|')'|'['|']'|'{'|'}'));
                    if !has_bracket {
                        new_tokens.push(tok);
                        continue;
                    }
                    let base = tok.color;
                    let mut run = String::new();
                    for ch in tok.text.chars() {
                        if matches!(ch, '(' | '[' | '{') {
                            if !run.is_empty() { new_tokens.push(RenderToken { text: std::mem::take(&mut run), color: base }); }
                            let bc = bracket_colors[depth % 3].unwrap_or(base);
                            new_tokens.push(RenderToken { text: ch.to_string(), color: bc });
                            depth += 1;
                        } else if matches!(ch, ')' | ']' | '}') {
                            if !run.is_empty() { new_tokens.push(RenderToken { text: std::mem::take(&mut run), color: base }); }
                            depth = depth.saturating_sub(1);
                            let bc = bracket_colors[depth % 3].unwrap_or(base);
                            new_tokens.push(RenderToken { text: ch.to_string(), color: bc });
                        } else {
                            run.push(ch);
                        }
                    }
                    if !run.is_empty() { new_tokens.push(RenderToken { text: run, color: base }); }
                }
                tokens = new_tokens;
            }

            // Inject inlay hints inline between tokens.
            // Hints use byte_col (0-based byte offset in the line text).
            // This matches the 1.5.5 approach: split tokens at hint byte positions.
            let mut line_hints: Vec<(usize, &str)> = inlay_hints.iter()
                .filter(|h| h.line == i - 1)
                .map(|h| {
                    // Convert 0-based char col to 1-based byte col (matching legacy).
                    let byte_col = text.char_indices()
                        .nth(h.col)
                        .map(|(bi, _)| bi + 1)
                        .unwrap_or(text.len() + 1);
                    (byte_col, h.label.as_str())
                })
                .collect();
            line_hints.sort_by_key(|h| h.0);

            if !line_hints.is_empty() {
                let mut new_tokens = Vec::new();
                let mut byte_col = 1usize; // 1-based
                let mut hint_idx = 0;
                for tok in &tokens {
                    let tok_bytes = tok.text.len();
                    let token_end = byte_col + tok_bytes;
                    if hint_idx < line_hints.len() && line_hints[hint_idx].0 < token_end {
                        let mut remaining = tok.text.as_str();
                        let mut cur_col = byte_col;
                        while hint_idx < line_hints.len() && line_hints[hint_idx].0 < token_end {
                            let (hcol, display) = line_hints[hint_idx];
                            if hcol > cur_col && !remaining.is_empty() {
                                let split_at = (hcol - cur_col).min(remaining.len());
                                let (before, after) = remaining.split_at(split_at);
                                new_tokens.push(RenderToken { text: before.to_string(), color: tok.color });
                                cur_col += split_at;
                                remaining = after;
                            }
                            new_tokens.push(RenderToken { text: display.to_string(), color: hint_color });
                            hint_idx += 1;
                        }
                        if !remaining.is_empty() {
                            new_tokens.push(RenderToken { text: remaining.to_string(), color: tok.color });
                        }
                    } else {
                        new_tokens.push(tok.clone());
                    }
                    byte_col = token_end;
                }
                while hint_idx < line_hints.len() {
                    new_tokens.push(RenderToken { text: line_hints[hint_idx].1.to_string(), color: hint_color });
                    hint_idx += 1;
                }
                tokens = new_tokens;
            }

            // If wrapping enabled, split tokens across multiple render lines.
            if let Some(max_w) = wrap_width {
                let char_w = style.code_font_height * 0.6; // approximate
                let max_chars = (max_w / char_w).floor() as usize;
                if max_chars > 0 && text.chars().count() > max_chars {
                    // Simple character-level wrapping.
                    let full_text: String = tokens.iter().map(|t| t.text.as_str()).collect();
                    let chars: Vec<char> = full_text.chars().collect();
                    let mut offset = 0;
                    let mut first_wrap = true;
                    while offset < chars.len() {
                        let end = (offset + max_chars).min(chars.len());
                        let chunk: String = chars[offset..end].iter().collect();
                        render.push(RenderLine {
                            line_number: if first_wrap { i } else { 0 }, // 0 = continuation
                            tokens: vec![RenderToken {
                                text: chunk,
                                color: style.text.to_array(),
                            }],
                        });
                        first_wrap = false;
                        offset = end;
                    }
                } else {
                    render.push(RenderLine { line_number: i, tokens });
                }
            } else {
                render.push(RenderLine { line_number: i, tokens });
            }
            i += 1;
        }
        Ok(render)
    })
    .unwrap_or_default()
}

/// Load fonts from NativeConfig into a draw context.
#[cfg(feature = "sdl")]
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
            refs.push(std::sync::Arc::new(parking_lot::Mutex::new(inner)));
        }
        Ok(ctx.add_font(refs))
    };

    let ui = load(&config.fonts.ui, &mut ctx)?;
    let code = load(&config.fonts.code, &mut ctx)?;
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

    FONT_SLOTS.with(|s| *s.borrow_mut() = Some((ui, code, icon, big, icon_big)));

    Ok(ctx)
}

use std::cell::RefCell;
thread_local! {
    static FONT_SLOTS: RefCell<Option<(u64, u64, u64, u64, u64)>> = const { RefCell::new(None) };
}

/// Build a StyleContext from NativeConfig and loaded fonts.
#[cfg(feature = "sdl")]
fn build_style(
    config: &NativeConfig,
    ctx: &crate::editor::draw_context::NativeDrawContext,
) -> StyleContext {
    use crate::editor::types::Color;
    use crate::editor::view::DrawContext as _;

    let (ui, code, icon, big, icon_big) = FONT_SLOTS.with(|s| s.borrow().unwrap_or((0, 0, 0, 0, 0)));

    StyleContext {
        font: ui,
        code_font: code,
        icon_font: icon,
        icon_big_font: icon_big,
        big_font: big,
        font_height: ctx.font_height(ui),
        code_font_height: ctx.font_height(code),
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

#[cfg(not(feature = "sdl"))]
fn build_style(_config: &NativeConfig, _ctx: &()) -> StyleContext {
    StyleContext::default()
}

#[cfg(not(feature = "sdl"))]
fn load_fonts(_config: &NativeConfig) -> Result<(), String> {
    Ok(())
}
