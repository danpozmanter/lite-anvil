#[cfg(not(any(unix, windows)))]
mod dummy {
    use crate::editor::terminal_buffer::Cell;

    /// Stub terminal inner for unsupported platforms.
    pub struct DummyInner {
        pub running: bool,
    }

    impl DummyInner {
        pub fn write(&mut self, _data: &[u8]) -> Result<(), ()> {
            Ok(())
        }
        pub fn poll(&mut self) {}
        pub fn read(&mut self, _max: usize) -> Option<Vec<u8>> {
            None
        }
        pub fn cleanup(&mut self) {}
    }

    /// Stub terminal buffer (matches TerminalBufferInner API surface).
    pub struct DummyBuf {
        empty_screen: Vec<Vec<Cell>>,
    }

    impl DummyBuf {
        pub fn new() -> Self {
            Self {
                empty_screen: Vec::new(),
            }
        }
        pub fn process_output(&mut self, _bytes: &[u8]) {}
        pub fn resize(&mut self, _cols: usize, _rows: usize) {}
        pub fn screen(&self) -> &Vec<Vec<Cell>> {
            &self.empty_screen
        }
        pub fn cursor_row(&self) -> usize {
            0
        }
        pub fn cursor_col(&self) -> usize {
            0
        }
    }

    /// Stub terminal instance for non-Unix platforms.
    pub struct TerminalInstance {
        pub inner: DummyInner,
        pub tbuf: DummyBuf,
        pub title: String,
    }

    /// Stub terminal panel for unsupported platforms.
    pub struct TerminalPanel {
        pub terminals: Vec<TerminalInstance>,
        pub active: usize,
        pub visible: bool,
        pub focused: bool,
    }

    impl TerminalPanel {
        pub fn new() -> Self {
            Self {
                terminals: vec![],
                active: 0,
                visible: false,
                focused: false,
            }
        }

        pub fn spawn(&mut self, _root: &str) -> bool {
            false
        }

        pub fn set_palette(&mut self, _palette: [[u8; 4]; 16], _default_fg: [u8; 4]) {}

        pub fn close_active(&mut self) -> bool {
            false
        }

        pub fn active_terminal(&mut self) -> Option<&mut TerminalInstance> {
            None
        }

        pub fn next_tab(&mut self) {}

        pub fn prev_tab(&mut self) {}
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) use dummy::*;

/// Resolve the directory a new terminal should spawn in: project root,
/// else active doc's parent, else process cwd, else `$HOME`.
#[cfg(any(unix, windows))]
pub(crate) fn resolve_terminal_cwd(active_doc_path: &str, project_root: &str) -> String {
    use std::path::{Path, PathBuf};

    let is_dir = |p: &Path| p.is_dir();

    if !project_root.is_empty() && is_dir(Path::new(project_root)) {
        return project_root.to_string();
    }
    if !active_doc_path.is_empty() {
        let p = Path::new(active_doc_path);
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() && is_dir(parent) {
                return parent.to_string_lossy().into_owned();
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if is_dir(&cwd) {
            return cwd.to_string_lossy().into_owned();
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let h = PathBuf::from(home);
        if is_dir(&h) {
            return h.to_string_lossy().into_owned();
        }
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let h = PathBuf::from(profile);
        if is_dir(&h) {
            return h.to_string_lossy().into_owned();
        }
    }
    ".".to_string()
}

/// Shell-quote a path for a POSIX shell with single quotes, so it can
/// be pasted into a `cd` command verbatim even if it contains spaces,
/// `$`, `"` or other metacharacters. Single quotes inside the path are
/// escaped by closing the quoted string, inserting a literal quote, and
/// reopening the quoted string: `it's` -> `'it'\''s'`.
#[cfg(any(unix, windows))]
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Build the `cd` payload to write into a freshly spawned terminal.
/// Sent as the first shell input so the prompt lands in `cwd` even
/// when the shell's rc files run their own `cd`.
#[cfg(any(unix, windows))]
pub(crate) fn terminal_cd_payload(cwd: &str) -> String {
    format!(" cd {} && clear\n", shell_single_quote(cwd))
}

/// Build a 16-slot ANSI palette + default foreground from the active
/// editor theme.
///
/// | ANSI | Role    | Source         | Fallback |
/// |-----:|---------|----------------|----------|
/// |   0  | black   | `background`   | nord0    |
/// |   1  | red     | `error`        | nord11   |
/// |   2  | green   | `good`         | nord14   |
/// |   3  | yellow  | `warn`         | nord13   |
/// |   4  | blue    | `accent`       | nord9    |
/// |   5  | magenta | `line_number2` | nord15   |
/// |   6  | cyan    | `caret`        | nord8    |
/// |   7  | white   | `text`         | nord4    |
#[cfg(any(unix, windows))]
pub(crate) fn theme_terminal_palette(
    style: &crate::editor::style_ctx::StyleContext,
) -> ([[u8; 4]; 16], [u8; 4]) {
    const DEFAULT_RED: [u8; 4] = [191, 97, 106, 255];
    const DEFAULT_GREEN: [u8; 4] = [163, 190, 140, 255];
    const DEFAULT_YELLOW: [u8; 4] = [235, 203, 139, 255];
    const DEFAULT_BLUE: [u8; 4] = [129, 161, 193, 255];
    const DEFAULT_MAGENTA: [u8; 4] = [180, 142, 173, 255];
    const DEFAULT_CYAN: [u8; 4] = [136, 192, 208, 255];
    const DEFAULT_WHITE: [u8; 4] = [216, 222, 233, 255];
    const DEFAULT_BLACK: [u8; 4] = [46, 52, 64, 255];
    const DEFAULT_BRIGHT_BLACK: [u8; 4] = [76, 86, 106, 255];

    // Zero-alpha means the theme left this slot unset; fall back.
    let or = |c: [u8; 4], fallback: [u8; 4]| if c[3] == 0 { fallback } else { c };

    let bg = or(style.background.to_array(), DEFAULT_BLACK);
    let fg = or(style.text.to_array(), DEFAULT_WHITE);
    let dim = or(style.dim.to_array(), DEFAULT_BRIGHT_BLACK);

    let red = or(style.error.to_array(), DEFAULT_RED);
    let green = or(style.good.to_array(), DEFAULT_GREEN);
    let yellow = or(style.warn.to_array(), DEFAULT_YELLOW);
    let blue = or(style.accent.to_array(), DEFAULT_BLUE);
    let magenta = or(style.line_number2.to_array(), DEFAULT_MAGENTA);
    let cyan = or(style.caret.to_array(), DEFAULT_CYAN);

    let palette = [
        bg, red, green, yellow, blue, magenta, cyan, fg, // 0..7  normal
        dim, red, green, yellow, blue, magenta, cyan, fg, // 8..15 bright
    ];
    (palette, fg)
}

/// Normalize a mouse-drag selection into an (anchor, cursor) pair where
/// anchor <= cursor in reading order. Returns `None` if the selection
/// is empty / a single point.
#[cfg(any(unix, windows))]
pub(crate) fn normalized_selection(
    start: (usize, usize),
    end: (usize, usize),
) -> Option<((usize, usize), (usize, usize))> {
    if start == end {
        return None;
    }
    let (a, b) = if start.0 < end.0 || (start.0 == end.0 && start.1 <= end.1) {
        (start, end)
    } else {
        (end, start)
    };
    Some((a, b))
}

/// Build the plain-text payload for a mouse-drag selection over the
/// given visible rows. `a` and `b` are `(row, col)` with `a <= b` in
/// reading order (use [`normalized_selection`] to normalise first).
/// Trailing spaces on each row are trimmed so copied output is
/// paste-friendly instead of carrying the blank rest-of-row padding.
#[cfg(any(unix, windows))]
pub(crate) fn extract_selection_text(
    visible_rows: &[std::borrow::Cow<'_, [crate::editor::terminal_buffer::Cell]>],
    a: (usize, usize),
    b: (usize, usize),
) -> String {
    let mut out = String::new();
    for row in a.0..=b.0 {
        let Some(cells) = visible_rows.get(row) else {
            continue;
        };
        let start_col = if row == a.0 { a.1 } else { 0 };
        let end_col = if row == b.0 { b.1 } else { cells.len() };
        let slice_end = end_col.min(cells.len());
        let mut line = String::new();
        for c in &cells[start_col.min(cells.len())..slice_end] {
            let ch = char::from_u32(c.ch).unwrap_or(' ');
            line.push(ch);
        }
        out.push_str(line.trim_end());
        if row != b.0 {
            out.push('\n');
        }
    }
    out
}

/// Build a tab title for a freshly spawned terminal, showing the index
/// and the basename of its working directory. Matches 1.5.5 TerminalView
/// labeling ("Terminal: <dir>").
#[cfg(any(unix, windows))]
pub(crate) fn terminal_title(index: usize, cwd: &str) -> String {
    let name = std::path::Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if name.is_empty() {
        format!("Terminal {index}")
    } else {
        format!("Terminal {index}: {name}")
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    #[test]
    fn resolve_cwd_prefers_project_root_over_doc_dir() {
        let tmp = std::env::temp_dir();
        let project = tmp.to_string_lossy().into_owned();
        // Use a file path inside a DIFFERENT existing dir so the doc-dir
        // branch would return something distinguishable from project_root.
        let doc_parent = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| tmp.clone());
        let fake_file = doc_parent.join("some_file.rs");
        let got = resolve_terminal_cwd(fake_file.to_string_lossy().as_ref(), &project);
        assert_eq!(got, project);
    }

    #[test]
    fn resolve_cwd_falls_back_to_doc_dir_when_no_project() {
        let doc_parent = std::env::temp_dir();
        let fake_file = doc_parent.join("some_file.rs");
        let got = resolve_terminal_cwd(fake_file.to_string_lossy().as_ref(), "");
        // Compare by components so trailing-separator differences
        // between platforms (Windows and macOS return `env::temp_dir()`
        // with one, Linux without) don't break the equality check.
        assert_eq!(
            std::path::Path::new(&got).components().collect::<Vec<_>>(),
            doc_parent.components().collect::<Vec<_>>(),
        );
    }

    #[test]
    fn resolve_cwd_never_returns_empty() {
        let got = resolve_terminal_cwd("", "");
        assert!(!got.is_empty());
    }

    #[test]
    fn terminal_title_uses_cwd_basename() {
        assert_eq!(
            terminal_title(1, "/home/user/myproject"),
            "Terminal 1: myproject"
        );
        assert_eq!(terminal_title(3, ""), "Terminal 3");
    }

    #[test]
    fn shell_single_quote_wraps_plain_path() {
        assert_eq!(
            shell_single_quote("/home/user/project"),
            "'/home/user/project'"
        );
    }

    #[test]
    fn shell_single_quote_escapes_embedded_quote() {
        // `it's` -> `'it'\''s'`  — closing the quote, inserting a literal
        // `'`, and reopening is the only POSIX-portable way to embed a
        // single quote inside a single-quoted string.
        assert_eq!(shell_single_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn shell_single_quote_leaves_metachars_inert() {
        // Dollar, backtick, double-quote, space must all stay literal
        // inside the single-quoted payload.
        assert_eq!(
            shell_single_quote("/tmp/a b $HOME `x` \"y\""),
            "'/tmp/a b $HOME `x` \"y\"'"
        );
    }

    #[test]
    fn theme_palette_blue_slot_is_readable() {
        let style = crate::editor::style_ctx::StyleContext::default();
        let (palette, _fg) = theme_terminal_palette(&style);
        let [r, g, b, _] = palette[4];
        let luma = r as u32 + g as u32 + b as u32;
        assert!(luma > 60, "blue slot luma {luma} too dark");
    }

    #[test]
    fn theme_palette_has_16_distinct_role_slots() {
        // Sanity check that the mapping actually differentiates roles
        // rather than returning the same color for every slot when the
        // SYNTAX_COLORS map is empty (default-theme boot path).
        let style = crate::editor::style_ctx::StyleContext::default();
        let (palette, _fg) = theme_terminal_palette(&style);
        assert_eq!(palette.len(), 16);
        // At least background (0) and text (7) should differ.
        assert_ne!(
            palette[0], palette[7],
            "ANSI black and white collapsed to the same color"
        );
    }

    #[test]
    fn normalized_selection_orders_reading_order() {
        assert_eq!(normalized_selection((0, 0), (0, 0)), None);
        assert_eq!(normalized_selection((2, 1), (0, 5)), Some(((0, 5), (2, 1))));
        assert_eq!(normalized_selection((1, 3), (1, 1)), Some(((1, 1), (1, 3))));
    }

    #[test]
    fn extract_selection_text_single_row_partial() {
        use crate::editor::terminal_buffer::Cell;
        let default_fg = [0, 0, 0, 255];
        let row: Vec<Cell> = "hello world"
            .chars()
            .map(|c| Cell {
                ch: c as u32,
                fg: 0,
                bg: 0,
            })
            .collect();
        let _ = default_fg;
        let rows: Vec<std::borrow::Cow<'_, [Cell]>> = vec![std::borrow::Cow::Owned(row)];
        let got = extract_selection_text(&rows, (0, 6), (0, 11));
        assert_eq!(got, "world");
    }

    #[test]
    fn extract_selection_text_multi_row_trims_trailing_spaces() {
        use crate::editor::terminal_buffer::Cell;
        let mk = |s: &str| -> Vec<Cell> {
            let mut padded = s.to_string();
            while padded.len() < 10 {
                padded.push(' ');
            }
            padded
                .chars()
                .map(|c| Cell {
                    ch: c as u32,
                    fg: 0,
                    bg: 0,
                })
                .collect()
        };
        let rows: Vec<std::borrow::Cow<'_, [Cell]>> = vec![
            std::borrow::Cow::Owned(mk("foo")),
            std::borrow::Cow::Owned(mk("bar")),
        ];
        let got = extract_selection_text(&rows, (0, 0), (1, 3));
        assert_eq!(got, "foo\nbar");
    }

    #[test]
    fn cd_payload_has_leading_space_and_clear() {
        // Leading space lets shells with `HISTCONTROL=ignorespace` /
        // `setopt histignorespace` skip this line in history; `clear`
        // wipes the cd artifact from the visible viewport.
        let payload = terminal_cd_payload("/tmp/foo");
        assert!(payload.starts_with(' '));
        assert!(payload.contains("cd '/tmp/foo'"));
        assert!(payload.contains("&& clear"));
        assert!(payload.ends_with('\n'));
    }
}

#[cfg(unix)]
mod unix_impl {
    use std::ffi::CString;

    use crate::editor::terminal::{
        TerminalInner, TerminalSpawnOptions, ensure_terminal_env, spawn_terminal,
    };
    use crate::editor::terminal_buffer::{DEFAULT_SCROLLBACK, TerminalBufferInner};

    pub(crate) const MAX_TERMINALS: usize = 10;

    /// Single terminal instance within the panel.
    pub(crate) struct TerminalInstance {
        pub(crate) inner: TerminalInner,
        pub(crate) tbuf: TerminalBufferInner,
        pub(crate) title: String,
        /// Viewport offset into scrollback in rows (0 = live bottom).
        /// f64 so the wheel handler can set fractional targets that
        /// the per-frame lerp eases toward.
        pub(crate) scrollback: f64,
        /// Target scrollback the current value is easing toward.
        pub(crate) scrollback_target: f64,
        /// Mouse-drag selection anchor (visible_row, col) at mouse-down.
        pub(crate) sel_start: Option<(usize, usize)>,
        /// Mouse-drag selection end (visible_row, col); updated on move.
        pub(crate) sel_end: Option<(usize, usize)>,
        /// True while the left button is held over the terminal.
        pub(crate) sel_dragging: bool,
    }

    /// Standard 16-color ANSI palette.
    pub(crate) fn default_16_color_palette() -> [[u8; 4]; 16] {
        [
            [0, 0, 0, 255],       // black
            [170, 0, 0, 255],     // red
            [0, 170, 0, 255],     // green
            [170, 85, 0, 255],    // yellow/brown
            [0, 0, 170, 255],     // blue
            [170, 0, 170, 255],   // magenta
            [0, 170, 170, 255],   // cyan
            [170, 170, 170, 255], // white
            [85, 85, 85, 255],    // bright black
            [255, 85, 85, 255],   // bright red
            [85, 255, 85, 255],   // bright green
            [255, 255, 85, 255],  // bright yellow
            [85, 85, 255, 255],   // bright blue
            [255, 85, 255, 255],  // bright magenta
            [85, 255, 255, 255],  // bright cyan
            [255, 255, 255, 255], // bright white
        ]
    }

    /// Multi-terminal panel managing several terminal instances.
    pub(crate) struct TerminalPanel {
        pub(crate) terminals: Vec<TerminalInstance>,
        pub(crate) active: usize,
        pub(crate) visible: bool,
        pub(crate) focused: bool,
        /// Last palette pushed in via `set_palette`, reused as the
        /// initial palette for terminals spawned after.
        pub(crate) pending_palette: Option<[[u8; 4]; 16]>,
        /// Last default-foreground pushed in via `set_palette`.
        pub(crate) pending_default_fg: Option<[u8; 4]>,
    }

    impl TerminalPanel {
        pub(crate) fn new() -> Self {
            Self {
                terminals: Vec::new(),
                active: 0,
                visible: false,
                focused: false,
                pending_palette: None,
                pending_default_fg: None,
            }
        }

        /// Apply an ANSI palette to every terminal instance and store it
        /// as the default for future spawns.
        pub(crate) fn set_palette(&mut self, palette: [[u8; 4]; 16], default_fg: [u8; 4]) {
            for inst in self.terminals.iter_mut() {
                inst.tbuf.set_palette(palette, default_fg);
            }
            self.pending_palette = Some(palette);
            self.pending_default_fg = Some(default_fg);
        }

        /// Spawn a new terminal instance. Returns false if at the limit.
        pub(crate) fn spawn(&mut self, project_root: &str) -> bool {
            if self.terminals.len() >= MAX_TERMINALS {
                return false;
            }
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            let cmd = vec![CString::new(shell).unwrap_or_default()];
            let mut env = Vec::new();
            let _ = ensure_terminal_env(&mut env);
            let opts = TerminalSpawnOptions {
                cwd: Some(CString::new(project_root).unwrap_or_default()),
                env,
                cols: 80,
                rows: 24,
            };
            match spawn_terminal(&cmd, &opts) {
                Ok(inner) => {
                    let idx = self.terminals.len();
                    let title = format!("Terminal {}", idx + 1);
                    let palette = self
                        .pending_palette
                        .unwrap_or_else(default_16_color_palette);
                    let default_fg = self.pending_default_fg.unwrap_or([200, 200, 200, 255]);
                    let tbuf =
                        TerminalBufferInner::new(80, 24, DEFAULT_SCROLLBACK, palette, default_fg);
                    let inst = TerminalInstance {
                        inner,
                        tbuf,
                        title,
                        scrollback: 0.0,
                        scrollback_target: 0.0,
                        sel_start: None,
                        sel_end: None,
                        sel_dragging: false,
                    };
                    self.terminals.push(inst);
                    self.active = idx;
                    self.visible = true;
                    self.focused = true;
                    true
                }
                Err(e) => {
                    eprintln!("[terminal] Spawn failed: {e}");
                    false
                }
            }
        }

        /// Close the active terminal. Returns true if panel should stay visible.
        pub(crate) fn close_active(&mut self) -> bool {
            if self.terminals.is_empty() {
                return false;
            }
            self.terminals[self.active].inner.cleanup();
            self.terminals.remove(self.active);
            if self.terminals.is_empty() {
                self.active = 0;
                self.visible = false;
                self.focused = false;
                return false;
            }
            if self.active >= self.terminals.len() {
                self.active = self.terminals.len() - 1;
            }
            true
        }

        /// Get the active terminal instance, if any.
        pub(crate) fn active_terminal(&mut self) -> Option<&mut TerminalInstance> {
            self.terminals.get_mut(self.active)
        }

        /// Switch to next terminal tab.
        pub(crate) fn next_tab(&mut self) {
            if !self.terminals.is_empty() {
                self.active = (self.active + 1) % self.terminals.len();
            }
        }

        /// Switch to previous terminal tab.
        pub(crate) fn prev_tab(&mut self) {
            if !self.terminals.is_empty() {
                self.active = if self.active == 0 {
                    self.terminals.len() - 1
                } else {
                    self.active - 1
                };
            }
        }
    }
}

#[cfg(unix)]
pub(crate) use unix_impl::*;

#[cfg(windows)]
mod windows_impl {
    use crate::editor::terminal_buffer::{DEFAULT_SCROLLBACK, TerminalBufferInner};
    use crate::editor::terminal_windows::{
        TerminalInner, TerminalSpawnOptions, ensure_terminal_env, spawn_terminal,
    };

    pub(crate) const MAX_TERMINALS: usize = 10;

    /// Single terminal instance within the panel.
    pub(crate) struct TerminalInstance {
        pub(crate) inner: TerminalInner,
        pub(crate) tbuf: TerminalBufferInner,
        pub(crate) title: String,
        /// Viewport offset into scrollback in rows (0 = live bottom).
        /// f64 so the wheel handler can set fractional targets that
        /// the per-frame lerp eases toward.
        pub(crate) scrollback: f64,
        /// Target scrollback the current value is easing toward.
        pub(crate) scrollback_target: f64,
        /// Mouse-drag selection anchor (visible_row, col) at mouse-down.
        pub(crate) sel_start: Option<(usize, usize)>,
        /// Mouse-drag selection end (visible_row, col); updated on move.
        pub(crate) sel_end: Option<(usize, usize)>,
        /// True while the left button is held over the terminal.
        pub(crate) sel_dragging: bool,
    }

    /// Standard 16-color ANSI palette.
    pub(crate) fn default_16_color_palette() -> [[u8; 4]; 16] {
        [
            [0, 0, 0, 255],       // black
            [170, 0, 0, 255],     // red
            [0, 170, 0, 255],     // green
            [170, 85, 0, 255],    // yellow/brown
            [0, 0, 170, 255],     // blue
            [170, 0, 170, 255],   // magenta
            [0, 170, 170, 255],   // cyan
            [170, 170, 170, 255], // white
            [85, 85, 85, 255],    // bright black
            [255, 85, 85, 255],   // bright red
            [85, 255, 85, 255],   // bright green
            [255, 255, 85, 255],  // bright yellow
            [85, 85, 255, 255],   // bright blue
            [255, 85, 255, 255],  // bright magenta
            [85, 255, 255, 255],  // bright cyan
            [255, 255, 255, 255], // bright white
        ]
    }

    /// Multi-terminal panel managing several terminal instances.
    pub(crate) struct TerminalPanel {
        pub(crate) terminals: Vec<TerminalInstance>,
        pub(crate) active: usize,
        pub(crate) visible: bool,
        pub(crate) focused: bool,
        pub(crate) pending_palette: Option<[[u8; 4]; 16]>,
        pub(crate) pending_default_fg: Option<[u8; 4]>,
    }

    impl TerminalPanel {
        pub(crate) fn new() -> Self {
            Self {
                terminals: Vec::new(),
                active: 0,
                visible: false,
                focused: false,
                pending_palette: None,
                pending_default_fg: None,
            }
        }

        /// Apply an ANSI palette to every terminal instance and store it
        /// as the default for future spawns.
        pub(crate) fn set_palette(&mut self, palette: [[u8; 4]; 16], default_fg: [u8; 4]) {
            for inst in self.terminals.iter_mut() {
                inst.tbuf.set_palette(palette, default_fg);
            }
            self.pending_palette = Some(palette);
            self.pending_default_fg = Some(default_fg);
        }

        /// Spawn a new terminal instance. Returns false if at the limit.
        pub(crate) fn spawn(&mut self, project_root: &str) -> bool {
            if self.terminals.len() >= MAX_TERMINALS {
                return false;
            }
            let mut env = Vec::new();
            let _ = ensure_terminal_env(&mut env);
            let opts = TerminalSpawnOptions {
                cwd: Some(project_root.to_string()),
                env,
                cols: 80,
                rows: 24,
            };
            match spawn_terminal(&opts) {
                Ok(inner) => {
                    let idx = self.terminals.len();
                    let title = format!("Terminal {}", idx + 1);
                    let palette = self
                        .pending_palette
                        .unwrap_or_else(default_16_color_palette);
                    let default_fg = self.pending_default_fg.unwrap_or([200, 200, 200, 255]);
                    let tbuf =
                        TerminalBufferInner::new(80, 24, DEFAULT_SCROLLBACK, palette, default_fg);
                    let inst = TerminalInstance {
                        inner,
                        tbuf,
                        title,
                        scrollback: 0.0,
                        scrollback_target: 0.0,
                        sel_start: None,
                        sel_end: None,
                        sel_dragging: false,
                    };
                    self.terminals.push(inst);
                    self.active = idx;
                    self.visible = true;
                    self.focused = true;
                    true
                }
                Err(e) => {
                    eprintln!("[terminal] Spawn failed: {e}");
                    false
                }
            }
        }

        /// Close the active terminal. Returns true if panel should stay visible.
        pub(crate) fn close_active(&mut self) -> bool {
            if self.terminals.is_empty() {
                return false;
            }
            self.terminals[self.active].inner.cleanup();
            self.terminals.remove(self.active);
            if self.terminals.is_empty() {
                self.active = 0;
                self.visible = false;
                self.focused = false;
                return false;
            }
            if self.active >= self.terminals.len() {
                self.active = self.terminals.len() - 1;
            }
            true
        }

        /// Get the active terminal instance, if any.
        pub(crate) fn active_terminal(&mut self) -> Option<&mut TerminalInstance> {
            self.terminals.get_mut(self.active)
        }

        /// Switch to next terminal tab.
        pub(crate) fn next_tab(&mut self) {
            if !self.terminals.is_empty() {
                self.active = (self.active + 1) % self.terminals.len();
            }
        }

        /// Switch to previous terminal tab.
        pub(crate) fn prev_tab(&mut self) {
            if !self.terminals.is_empty() {
                self.active = if self.active == 0 {
                    self.terminals.len() - 1
                } else {
                    self.active - 1
                };
            }
        }
    }
}

#[cfg(windows)]
pub(crate) use windows_impl::*;
