use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Typed editor configuration. Deserialised from the user's `config.toml`
/// with sensible defaults for every field.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NativeConfig {
    #[serde(skip)]
    pub verbose: bool,
    /// Why the user's `config.toml` could not be read, when it exists but
    /// failed to load. Carried so the editor can tell the user their
    /// settings are not in effect instead of quietly running on defaults.
    #[serde(skip)]
    pub load_error: Option<String>,
    pub fps: u32,
    pub max_log_items: u32,
    pub message_timeout: u32,
    pub mouse_wheel_scroll: f64,
    pub animate_drag_scroll: bool,
    pub scroll_past_end: bool,
    pub force_scrollbar_status: bool,
    pub file_size_limit: u32,
    pub large_file: LargeFileConfig,
    pub project_scan: ProjectScanConfig,
    pub ignore_files: Vec<String>,
    pub symbol_pattern: String,
    pub non_word_chars: String,
    pub undo_merge_timeout: f64,
    pub max_undos: u32,
    pub max_tabs: u32,
    pub max_visible_commands: u32,
    pub always_show_tabs: bool,
    pub highlight_current_line: bool,
    pub line_height: f64,
    pub indent_size: u32,
    pub tab_type: TabType,
    pub keep_newline_whitespace: bool,
    pub line_endings: LineEndings,
    pub line_limit: u32,
    pub theme: String,
    pub gitignore: GitignoreConfig,
    pub lsp: LspConfig,
    pub native_tokenizer: NativeTokenizerConfig,
    pub terminal: TerminalConfig,
    pub ui: UiConfig,
    pub fonts: FontsConfig,
    pub files: FilesConfig,
    pub long_line_indicator: bool,
    pub long_line_indicator_width: u32,
    pub transitions: bool,
    pub disabled_transitions: DisabledTransitions,
    pub animation_rate: f64,
    pub blink_period: f64,
    pub disable_blink: bool,
    pub draw_whitespace: bool,
    pub borderless: bool,
    pub tab_close_button: bool,
    pub max_clicks: u32,
    pub skip_plugins_version: bool,
    pub stonks: bool,
    pub use_system_file_picker: bool,
    /// macOS only: when true, the Command key triggers the same bindings as
    /// Control (so Cmd+S acts like Ctrl+S, matching Mac conventions). Default
    /// true on macOS so shortcuts feel native. Set to false to use Ctrl
    /// uniformly across platforms. No-op on non-Mac.
    pub mac_command_as_ctrl: bool,
    /// When true, pasted text has its leading whitespace converted to match the
    /// document's indent style (tabs vs spaces) and size. Default true.
    pub format_on_paste: bool,
    /// Color overrides (key -> "#rrggbb" or "#rrggbbaa").
    #[serde(default)]
    pub colors: ColorsConfig,
    /// Custom keybindings: stroke -> command name or array of command names.
    #[serde(default)]
    pub keybindings: HashMap<String, toml::Value>,
    /// Plugin enable/disable and per-plugin settings.
    #[serde(default)]
    pub plugins: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LargeFileConfig {
    pub soft_limit_mb: u32,
    pub hard_limit_mb: u32,
    /// Lines longer than this (in bytes) trigger plain-text fallback
    /// regardless of total file size. Prevents a single pathological line
    /// from defeating the incremental tokenizer.
    pub long_line_limit_kb: u32,
    pub read_only: bool,
    pub plain_text: bool,
    pub disable_lsp: bool,
    pub disable_autocomplete: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProjectScanConfig {
    pub max_files: u32,
    pub exclude_dirs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabType {
    Soft,
    Hard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEndings {
    Lf,
    Crlf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GitignoreConfig {
    pub enabled: bool,
    pub additional_patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LspConfig {
    pub load_on_startup: bool,
    pub semantic_highlighting: bool,
    pub inline_diagnostics: bool,
    pub format_on_save: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NativeTokenizerConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    pub placement: String,
    pub reuse_mode: String,
}

/// File-handling options that cut across the save / load pipeline.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FilesConfig {
    /// When `true`, saves go through a temp file and a rename, so a crash
    /// mid-write leaves the original file intact. The rename installs a
    /// new inode, so mode bits, ownership, ACLs, xattrs, and hardlinks
    /// are copied across explicitly. When `false` (the default), saves
    /// write the new content directly over the existing inode with
    /// truncate-then-write, preserving all file metadata for free at the
    /// cost of crash-safety.
    pub atomic_save: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub divider_size: u32,
    pub scrollbar_size: u32,
    pub expanded_scrollbar_size: u32,
    pub minimum_thumb_size: u32,
    pub contracted_scrollbar_margin: u32,
    pub expanded_scrollbar_margin: u32,
    pub caret_width: u32,
    pub tab_width: u32,
    pub padding_x: u32,
    pub padding_y: u32,
}

/// Color overrides for the style system.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ColorsConfig {
    /// Top-level color overrides (background, text, caret, etc.).
    #[serde(flatten)]
    pub style: HashMap<String, String>,
    /// Syntax token color overrides (keyword, string, comment, etc.).
    #[serde(default)]
    pub syntax: HashMap<String, String>,
    /// Log level color overrides (INFO, WARN, ERROR).
    #[serde(default)]
    pub log: HashMap<String, LogColorEntry>,
    /// Lint color overrides (error, warning, info).
    #[serde(default)]
    pub lint: HashMap<String, String>,
}

/// Log level color entry with icon and color.
#[derive(Debug, Clone, Deserialize)]
pub struct LogColorEntry {
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

/// Font rendering options.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FontOptions {
    pub antialiasing: Option<String>,
    pub hinting: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub smoothing: Option<bool>,
    pub strikethrough: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FontSpec {
    pub path: Option<String>,
    /// Multiple font paths for fallback groups.
    pub paths: Option<Vec<String>>,
    pub size: u32,
    #[serde(default)]
    pub options: FontOptions,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FontsConfig {
    pub ui: FontSpec,
    pub code: FontSpec,
    pub big: FontSpec,
    pub icon: FontSpec,
    pub icon_big: FontSpec,
    /// Per-syntax-token font overrides (e.g. italic comments).
    #[serde(default)]
    pub syntax: HashMap<String, FontSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DisabledTransitions {
    pub scroll: bool,
    pub commandview: bool,
    pub contextmenu: bool,
    pub logview: bool,
    pub nagbar: bool,
    pub tabs: bool,
    pub tab_drag: bool,
    pub statusbar: bool,
}

// ── Default implementations ──────────────────────────────────────────────────

// The literal's base is `Self::with_defaults`, whose field values (fps 60,
// theme "dark_default", ...) are not the field types' own defaults, so
// clippy::derivable_impls misreads this as a derivable impl.
#[allow(clippy::derivable_impls)]
impl Default for NativeConfig {
    fn default() -> Self {
        // Serde fills a parsed config's missing fields from this value, and
        // `load_or_default` resolves font paths against the real data
        // directory afterwards. Font paths must therefore stay unset here:
        // a baked-in directory is non-None, so it survives that resolution
        // and leaves a relative path that only loads when the process runs
        // from the install root.
        Self {
            fonts: FontsConfig::default(),
            ..Self::with_defaults(1.0, "Linux", "data")
        }
    }
}

/// Keys the typed config accepts, per section (`""` = top level). Sections
/// and nested tables not listed here are unchecked: their shape is free-form
/// (colors, fonts.syntax, plugin settings), so an unfamiliar key there is
/// not a mistake. serde finds the first unknown key but does not name where
/// it sat, so the raw text is scanned for the line it appears on.
fn known_keys(section: &str) -> Option<&'static [&'static str]> {
    match section {
        "" => Some(&[
            "fps", "max_log_items", "message_timeout", "mouse_wheel_scroll",
            "animate_drag_scroll", "scroll_past_end", "force_scrollbar_status",
            "file_size_limit", "large_file", "project_scan", "ignore_files",
            "symbol_pattern", "non_word_chars", "undo_merge_timeout", "max_undos",
            "max_tabs", "max_visible_commands", "always_show_tabs",
            "highlight_current_line", "line_height", "indent_size", "tab_type",
            "keep_newline_whitespace", "line_endings", "line_limit", "theme",
            "gitignore", "lsp", "native_tokenizer", "terminal", "ui", "fonts",
            "files", "long_line_indicator", "long_line_indicator_width",
            "transitions", "disabled_transitions", "animation_rate",
            "blink_period", "disable_blink", "draw_whitespace", "borderless",
            "tab_close_button", "max_clicks", "skip_plugins_version", "stonks",
            "use_system_file_picker", "mac_command_as_ctrl", "format_on_paste",
            "colors", "keybindings", "plugins",
        ]),
        "large_file" => Some(&[
            "soft_limit_mb", "hard_limit_mb", "long_line_limit_kb", "read_only",
            "plain_text", "disable_lsp", "disable_autocomplete",
        ]),
        "project_scan" => Some(&["max_files", "exclude_dirs"]),
        "gitignore" => Some(&["enabled", "additional_patterns"]),
        "lsp" => Some(&[
            "load_on_startup", "semantic_highlighting", "inline_diagnostics",
            "format_on_save",
        ]),
        "native_tokenizer" => Some(&["enabled"]),
        "terminal" => Some(&["placement", "reuse_mode"]),
        "ui" => Some(&[
            "divider_size", "scrollbar_size", "expanded_scrollbar_size",
            "minimum_thumb_size", "contracted_scrollbar_margin",
            "expanded_scrollbar_margin", "caret_width", "tab_width",
            "padding_x", "padding_y",
        ]),
        "fonts" => Some(&["ui", "code", "big", "icon", "icon_big", "syntax"]),
        "files" => Some(&["atomic_save"]),
        "disabled_transitions" => Some(&[
            "scroll", "commandview", "contextmenu", "logview", "nagbar",
            "tabs", "tab_drag", "statusbar",
        ]),
        _ => None,
    }
}

/// Every unknown key with its 1-based line number. A file with unknown keys
/// still parses — serde fills the known fields and drops the rest — so this
/// is the only place "my theme didn't load" gets named.
fn unknown_key_errors(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut section = String::new();
    for (n, raw) in content.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().trim_matches('"').to_string();
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let known = known_keys(&section)
            .map(|fields| fields.contains(&key))
            // A nested table (fonts.code, colors.syntax) is its own section:
            // re-anchor to its parent so its known keys are still checked.
            .unwrap_or_else(|| {
                let parent = section.rsplit_once('.').map(|(p, _)| p).unwrap_or("");
                known_keys(parent).map(|f| f.contains(&key)).unwrap_or(true)
            });
        if !known {
            out.push(format!(
                "config.toml line {}: unknown key `{key}` - the rest of the file still loaded",
                n + 1
            ));
        }
    }
    out
}

impl NativeConfig {
    /// Build a config using the given scale factor and platform name.
    pub fn with_defaults(scale: f64, platform: &str, datadir: &str) -> Self {
        let line_endings = if platform == "Windows" {
            LineEndings::Crlf
        } else {
            LineEndings::Lf
        };
        Self {
            verbose: false,
            load_error: None,
            fps: 60,
            max_log_items: 800,
            message_timeout: 5,
            mouse_wheel_scroll: 50.0 * scale,
            animate_drag_scroll: false,
            scroll_past_end: true,
            force_scrollbar_status: false,
            file_size_limit: 10,
            large_file: LargeFileConfig::default(),
            project_scan: ProjectScanConfig::default(),
            ignore_files: default_ignore_files(),
            symbol_pattern: "[%a_][%w_]*".into(),
            non_word_chars: " \t\n/\\()\"':,.;<>~!@#$%^&*|+=[]{}`?-".into(),
            undo_merge_timeout: 0.3,
            max_undos: 10000,
            max_tabs: 8,
            max_visible_commands: 10,
            always_show_tabs: true,
            highlight_current_line: true,
            line_height: 1.2,
            indent_size: 2,
            tab_type: TabType::Soft,
            keep_newline_whitespace: false,
            line_endings,
            line_limit: 80,
            theme: "dark_default".into(),
            gitignore: GitignoreConfig::default(),
            lsp: LspConfig::default(),
            native_tokenizer: NativeTokenizerConfig::default(),
            terminal: TerminalConfig::default(),
            ui: UiConfig::default(),
            fonts: FontsConfig::with_datadir(datadir),
            files: FilesConfig::default(),
            long_line_indicator: false,
            long_line_indicator_width: 1,
            transitions: true,
            disabled_transitions: DisabledTransitions::default(),
            animation_rate: 1.0,
            blink_period: 0.8,
            disable_blink: false,
            draw_whitespace: false,
            borderless: false,
            tab_close_button: true,
            max_clicks: 3,
            skip_plugins_version: false,
            stonks: true,
            use_system_file_picker: false,
            mac_command_as_ctrl: cfg!(target_os = "macos"),
            format_on_paste: true,
            colors: ColorsConfig::default(),
            keybindings: HashMap::new(),
            plugins: HashMap::new(),
        }
    }

    /// Load config from a TOML file, falling back to defaults for missing fields.
    pub fn load_toml(path: &Path) -> Result<Self, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read config: {e}"))?;
        Self::load_toml_str(&content)
    }

    /// Parse config TOML already in memory. A syntax error names the line and
    /// reason in the error; an unknown key parses with the rest of the file
    /// but is reported through `load_error` so the status bar can name it.
    pub fn load_toml_str(content: &str) -> Result<Self, String> {
        match toml::from_str::<NativeConfig>(content) {
            Ok(mut config) => {
                if let Some(msg) = unknown_key_errors(content).into_iter().next() {
                    config.load_error = Some(msg);
                }
                Ok(config)
            }
            Err(e) => Err(format!("invalid config TOML: {e}")),
        }
    }

    /// Load config from TOML if it exists, otherwise return defaults.
    pub fn load_or_default(userdir: &str, scale: f64, platform: &str, datadir: &str) -> Self {
        let toml_path = Path::new(userdir).join("config.toml");
        if toml_path.exists() {
            match Self::load_toml(&toml_path) {
                Ok(mut config) => {
                    // Apply runtime-only values that can't come from TOML.
                    if config.mouse_wheel_scroll == 50.0 {
                        config.mouse_wheel_scroll = 50.0 * scale;
                    }
                    config.resolve_font_paths(datadir);
                    config
                }
                Err(e) => {
                    log::error!("{e}");
                    let mut config = Self::with_defaults(scale, platform, datadir);
                    config.load_error = Some(e);
                    config
                }
            }
        } else {
            Self::with_defaults(scale, platform, datadir)
        }
    }

    /// Default TOML config template for new users (all settings commented out).
    pub fn default_toml_template() -> &'static str {
        include_str!("config_template.toml")
    }

    /// Serialize the config to TOML.
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        out.push_str("# Lite Anvil configuration\n\n");
        out.push_str(&format!("fps = {}\n", self.fps));
        out.push_str(&format!("theme = \"{}\"\n", self.theme));
        out.push_str(&format!("indent_size = {}\n", self.indent_size));
        out.push_str(&format!(
            "tab_type = \"{}\"\n",
            match self.tab_type {
                TabType::Soft => "soft",
                TabType::Hard => "hard",
            }
        ));
        out.push_str(&format!(
            "line_endings = \"{}\"\n",
            match self.line_endings {
                LineEndings::Lf => "lf",
                LineEndings::Crlf => "crlf",
            }
        ));
        out.push_str(&format!("line_limit = {}\n", self.line_limit));
        out.push_str(&format!("line_height = {}\n", self.line_height));
        out.push_str(&format!(
            "highlight_current_line = {}\n",
            self.highlight_current_line
        ));
        out.push_str(&format!("max_tabs = {}\n", self.max_tabs));
        out.push_str(&format!("always_show_tabs = {}\n", self.always_show_tabs));
        out.push_str(&format!("blink_period = {}\n", self.blink_period));
        out.push_str(&format!("disable_blink = {}\n", self.disable_blink));
        out.push_str(&format!("draw_whitespace = {}\n", self.draw_whitespace));
        out.push_str(&format!("borderless = {}\n", self.borderless));
        out.push_str(&format!("tab_close_button = {}\n", self.tab_close_button));
        out.push_str(&format!("transitions = {}\n", self.transitions));
        out.push_str(&format!("animation_rate = {}\n", self.animation_rate));
        out.push_str(&format!("scroll_past_end = {}\n", self.scroll_past_end));
        out.push_str(&format!("max_undos = {}\n", self.max_undos));
        out.push_str(&format!(
            "undo_merge_timeout = {}\n",
            self.undo_merge_timeout
        ));

        out.push_str("\n[lsp]\n");
        out.push_str(&format!("load_on_startup = {}\n", self.lsp.load_on_startup));
        out.push_str(&format!(
            "semantic_highlighting = {}\n",
            self.lsp.semantic_highlighting
        ));
        out.push_str(&format!(
            "inline_diagnostics = {}\n",
            self.lsp.inline_diagnostics
        ));
        out.push_str(&format!("format_on_save = {}\n", self.lsp.format_on_save));

        out.push_str("\n[terminal]\n");
        out.push_str(&format!("placement = \"{}\"\n", self.terminal.placement));
        out.push_str(&format!("reuse_mode = \"{}\"\n", self.terminal.reuse_mode));

        out.push_str("\n[ui]\n");
        out.push_str(&format!("padding_x = {}\n", self.ui.padding_x));
        out.push_str(&format!("padding_y = {}\n", self.ui.padding_y));
        out.push_str(&format!("caret_width = {}\n", self.ui.caret_width));
        out.push_str(&format!("scrollbar_size = {}\n", self.ui.scrollbar_size));
        out.push_str(&format!("tab_width = {}\n", self.ui.tab_width));

        out.push_str("\n[fonts.ui]\n");
        if let Some(ref p) = self.fonts.ui.path {
            out.push_str(&format!("path = \"{p}\"\n"));
        }
        out.push_str(&format!("size = {}\n", self.fonts.ui.size));

        out.push_str("\n[fonts.code]\n");
        if let Some(ref p) = self.fonts.code.path {
            out.push_str(&format!("path = \"{p}\"\n"));
        }
        out.push_str(&format!("size = {}\n", self.fonts.code.size));

        out
    }

    /// Fill in default font paths for fonts that have no path set.
    pub fn resolve_font_paths(&mut self, datadir: &str) {
        if self.fonts.ui.path.is_none() {
            self.fonts.ui.path = Some(join_font_path(datadir, "Lilex-Regular.ttf"));
        }
        if self.fonts.code.path.is_none() {
            self.fonts.code.path = Some(join_font_path(datadir, "Lilex-Medium.ttf"));
        }
        if self.fonts.icon.path.is_none() {
            self.fonts.icon.path = Some(join_font_path(datadir, "icons.ttf"));
        }
    }
}

impl Default for LargeFileConfig {
    fn default() -> Self {
        Self {
            soft_limit_mb: 20,
            // 4 GB hard cap. Files above this are refused.
            // Peak memory during load is ~2.5x the file size due to line splitting.
            hard_limit_mb: 4096,
            // Lines above 64 KB fall back to plain text. Prevents a single
            // pathological line from defeating the incremental tokenizer.
            long_line_limit_kb: 64,
            read_only: true,
            plain_text: true,
            disable_lsp: true,
            disable_autocomplete: true,
        }
    }
}

impl Default for ProjectScanConfig {
    fn default() -> Self {
        Self {
            max_files: 50000,
            exclude_dirs: vec!["__pycache__".into()],
        }
    }
}

impl Default for GitignoreConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            additional_patterns: Vec::new(),
        }
    }
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            load_on_startup: true,
            semantic_highlighting: true,
            inline_diagnostics: true,
            format_on_save: true,
        }
    }
}

impl Default for NativeTokenizerConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            placement: "bottom".into(),
            reuse_mode: "pane".into(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            divider_size: 1,
            scrollbar_size: 16,
            expanded_scrollbar_size: 20,
            minimum_thumb_size: 32,
            contracted_scrollbar_margin: 8,
            expanded_scrollbar_margin: 12,
            caret_width: 2,
            tab_width: 170,
            padding_x: 14,
            padding_y: 7,
        }
    }
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            path: None,
            paths: None,
            size: 15,
            options: FontOptions::default(),
        }
    }
}

/// Join a data directory with a font filename using the platform's native
/// path separator, avoiding mixed forward/backward slashes on Windows.
pub fn join_font_path(datadir: &str, filename: &str) -> String {
    Path::new(datadir)
        .join("fonts")
        .join(filename)
        .to_string_lossy()
        .into_owned()
}

impl FontsConfig {
    fn with_datadir(datadir: &str) -> Self {
        Self {
            ui: FontSpec {
                path: Some(join_font_path(datadir, "Lilex-Regular.ttf")),
                size: 15,
                ..Default::default()
            },
            code: FontSpec {
                path: Some(join_font_path(datadir, "Lilex-Medium.ttf")),
                size: 15,
                ..Default::default()
            },
            big: FontSpec {
                size: 46,
                ..Default::default()
            },
            icon: FontSpec {
                path: Some(join_font_path(datadir, "icons.ttf")),
                size: 16,
                options: FontOptions {
                    antialiasing: Some("grayscale".into()),
                    hinting: Some("full".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            icon_big: FontSpec {
                size: 23,
                ..Default::default()
            },
            syntax: HashMap::new(),
        }
    }
}

impl Default for FontsConfig {
    fn default() -> Self {
        // Paths left as None so `resolve_font_paths(datadir)` fills them
        // with an absolute `{datadir}/fonts/...` later. Pre-filling paths
        // here (the old behaviour baked "data/fonts/..." relative strings
        // in) defeats `resolve_font_paths`, which only overrides None.
        Self {
            ui: FontSpec::default(),
            code: FontSpec::default(),
            big: FontSpec {
                size: 46,
                ..Default::default()
            },
            icon: FontSpec {
                size: 16,
                options: FontOptions {
                    antialiasing: Some("grayscale".into()),
                    hinting: Some("full".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            icon_big: FontSpec {
                size: 23,
                ..Default::default()
            },
            syntax: HashMap::new(),
        }
    }
}

fn default_ignore_files() -> Vec<String> {
    vec![
        "^%.svn/".into(),
        "^%.git/".into(),
        "^%.hg/".into(),
        "^CVS/".into(),
        "^%.Trash/".into(),
        "^%.Trash%-.*/".into(),
        "^node_modules/".into(),
        "^%.cache/".into(),
        "^__pycache__/".into(),
        "%.pyc$".into(),
        "%.pyo$".into(),
        "%.exe$".into(),
        "%.dll$".into(),
        "%.obj$".into(),
        "%.o$".into(),
        "%.a$".into(),
        "%.lib$".into(),
        "%.so$".into(),
        "%.dylib$".into(),
        "%.ncb$".into(),
        "%.sdf$".into(),
        "%.suo$".into(),
        "%.pdb$".into(),
        "%.idb$".into(),
        "%.class$".into(),
        "%.psd$".into(),
        "%.db$".into(),
        "^desktop%.ini$".into(),
        "^%.DS_Store$".into(),
        "^%.directory$".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sane_values() {
        let config = NativeConfig::default();
        assert_eq!(config.fps, 60);
        assert_eq!(config.theme, "dark_default");
        assert_eq!(config.indent_size, 2);
        assert_eq!(config.tab_type, TabType::Soft);
    }

    #[test]
    fn load_toml_minimal() {
        let toml = r#"
            theme = "summer"
            indent_size = 4
            tab_type = "hard"
        "#;
        let config: NativeConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.theme, "summer");
        assert_eq!(config.indent_size, 4);
        assert_eq!(config.tab_type, TabType::Hard);
        // Defaults for unspecified fields
        assert_eq!(config.fps, 60);
        assert_eq!(config.line_height, 1.2);
    }

    #[test]
    fn load_toml_nested() {
        let toml = r#"
            [lsp]
            format_on_save = false

            [ui]
            padding_x = 20

            [fonts.code]
            path = "/usr/share/fonts/mono.ttf"
            size = 14
        "#;
        let config: NativeConfig = toml::from_str(toml).unwrap();
        assert!(!config.lsp.format_on_save);
        assert_eq!(config.ui.padding_x, 20);
        assert_eq!(
            config.fonts.code.path,
            Some("/usr/share/fonts/mono.ttf".into())
        );
        assert_eq!(config.fonts.code.size, 14);
    }

    #[test]
    fn load_toml_line_endings() {
        let toml = r#"line_endings = "crlf""#;
        let config: NativeConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.line_endings, LineEndings::Crlf);
    }

    #[test]
    fn to_toml_round_trips_key_fields() {
        let config = NativeConfig::with_defaults(1.0, "Linux", "data");
        let toml_str = config.to_toml();
        assert!(toml_str.contains("theme = \"dark_default\""));
        assert!(toml_str.contains("indent_size = 2"));
        assert!(toml_str.contains("tab_type = \"soft\""));
    }

    #[test]
    fn load_or_default_reports_a_config_it_could_not_parse() {
        let dir = std::env::temp_dir().join("anvil_bad_config_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "theme = { name = \"summer\" }\n").unwrap();
        let config = NativeConfig::load_or_default(dir.to_str().unwrap(), 1.0, "Linux", "data");
        assert!(
            config.load_error.is_some(),
            "a config that fails to parse must say so"
        );
        assert_eq!(config.theme, "dark_default");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_parsed_config_resolves_fonts_under_the_real_data_dir() {
        let dir = std::env::temp_dir().join("anvil_font_path_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "theme = \"summer\"\n").unwrap();
        // An absolute path spelled the host's way, so the assertion below holds
        // on Windows as well as on Unix.
        let datadir = std::env::temp_dir().join("anvil_font_path_test_data");
        let config = NativeConfig::load_or_default(
            dir.to_str().unwrap(),
            1.0,
            "Linux",
            datadir.to_str().unwrap(),
        );
        assert_eq!(config.theme, "summer");
        for (slot, spec) in [
            ("ui", &config.fonts.ui),
            ("code", &config.fonts.code),
            ("icon", &config.fonts.icon),
        ] {
            let path = spec.path.as_deref().unwrap_or_default();
            assert!(
                Path::new(path).is_absolute() && Path::new(path).starts_with(&datadir),
                "{slot} font resolved to {path:?}, which does not sit under {datadir:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_or_default_returns_default_when_no_file() {
        let config = NativeConfig::load_or_default("/nonexistent", 1.0, "Linux", "data");
        assert_eq!(config.fps, 60);
    }

    #[test]
    fn load_toml_colors() {
        let toml = r##"
            [colors]
            background = "#1f2128"
            text = "#d7dae0"

            [colors.syntax]
            keyword = "#ff7a90"
            string = "#ffd479"

            [colors.lint]
            error = "#ff5f56"
        "##;
        let config: NativeConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.colors.style.get("background").unwrap(), "#1f2128");
        assert_eq!(config.colors.syntax.get("keyword").unwrap(), "#ff7a90");
        assert_eq!(config.colors.lint.get("error").unwrap(), "#ff5f56");
    }

    #[test]
    fn load_toml_keybindings() {
        let toml = r#"
            [keybindings]
            "ctrl+escape" = "core:quit"
            "ctrl+shift+p" = "core:find-command"
        "#;
        let config: NativeConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.keybindings.len(), 2);
    }

    #[test]
    fn load_toml_plugins() {
        let toml = r#"
            [plugins]
            detectindent = false
            minimap = { enabled = true, width = 120 }
        "#;
        let config: NativeConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.plugins.len(), 2);
    }

    #[test]
    fn load_toml_font_options() {
        let toml = r#"
            [fonts.code]
            path = "/usr/share/fonts/mono.ttf"
            size = 14

            [fonts.code.options]
            antialiasing = "grayscale"
            hinting = "slight"
            italic = true
        "#;
        let config: NativeConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.fonts.code.options.antialiasing,
            Some("grayscale".into())
        );
        assert_eq!(config.fonts.code.options.italic, Some(true));
    }

    #[test]
    fn load_toml_syntax_fonts() {
        let toml = r#"
            [fonts.syntax.comment]
            path = "/usr/share/fonts/italic.ttf"
            size = 15

            [fonts.syntax.comment.options]
            italic = true
        "#;
        let config: NativeConfig = toml::from_str(toml).unwrap();
        let comment = config.fonts.syntax.get("comment").unwrap();
        assert_eq!(comment.options.italic, Some(true));
    }

    #[test]
    fn a_syntax_error_names_the_line_and_reason() {
        let err = NativeConfig::load_toml_str("theme = \nindent_size = 2\n").unwrap_err();
        assert!(err.contains("line 1"), "error must name the line: {err}");
        assert!(!err.contains("unknown key"), "{err}");
    }

    #[test]
    fn an_unknown_key_names_the_line_and_the_key_and_loads_the_rest() {
        let config = NativeConfig::load_toml_str("theme = \"summer\"\nthme = \"typo\"\nindent_size = 4\n").unwrap();
        // The rest of the file still loaded...
        assert_eq!(config.theme, "summer");
        assert_eq!(config.indent_size, 4);
        // ...and the unknown key is named with its line.
        assert_eq!(
            config.load_error.as_deref(),
            Some("config.toml line 2: unknown key `thme` - the rest of the file still loaded")
        );
    }

    #[test]
    fn an_unknown_key_inside_a_table_names_its_line() {
        let config = NativeConfig::load_toml_str(
            "[lsp]\nload_on_startup = true\nsave_on_load = true\n",
        )
        .unwrap();
        assert!(
            config
                .load_error
                .as_deref()
                .unwrap_or_default()
                .starts_with("config.toml line 3: unknown key `save_on_load`"),
            "{:?}",
            config.load_error
        );
        assert!(config.lsp.load_on_startup);
    }
}
