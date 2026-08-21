use parking_lot::Mutex;
use std::sync::LazyLock;

use crate::editor::contrast;
use crate::editor::types::Color;

/// Global style context, synced once per frame.
static STYLE: LazyLock<Mutex<StyleContext>> = LazyLock::new(|| Mutex::new(StyleContext::default()));

/// Get a copy of the current style context.
pub fn current_style() -> StyleContext {
    STYLE.lock().clone()
}

/// Update the global style context (called from core.step before draw).
pub fn set_current_style(style: StyleContext) {
    *STYLE.lock() = style;
}

/// Resolved style values for native view drawing.
#[derive(Debug, Clone, Default)]
pub struct StyleContext {
    // Colors
    pub background: Color,
    pub background2: Color,
    pub background3: Color,
    pub text: Color,
    pub caret: Color,
    pub accent: Color,
    pub dim: Color,
    pub divider: Color,
    pub selection: Color,
    pub line_number: Color,
    pub line_number2: Color,
    pub line_highlight: Color,
    pub scrollbar: Color,
    pub scrollbar2: Color,
    pub good: Color,
    pub warn: Color,
    pub error: Color,
    pub nagbar: Color,
    pub nagbar_text: Color,
    pub nagbar_dim: Color,
    pub scrollbar_track: Color,

    // Dimensions (already scaled)
    pub padding_x: f64,
    pub padding_y: f64,
    pub divider_size: f64,
    pub scrollbar_size: f64,
    pub caret_width: f64,
    pub tab_width: f64,

    // Font slot IDs (into NativeDrawContext)
    pub font: u64,
    pub code_font: u64,
    pub icon_font: u64,
    pub icon_big_font: u64,
    pub big_font: u64,
    pub seti_font: u64,
    /// Scaled UI font used for markdown h1 headings.
    pub h1_font: u64,
    /// Scaled UI font used for markdown h2 headings.
    pub h2_font: u64,
    /// Scaled UI font used for markdown h3 headings.
    pub h3_font: u64,

    // Metrics
    pub font_height: f64,
    pub code_font_height: f64,
    pub h1_font_height: f64,
    pub h2_font_height: f64,
    pub h3_font_height: f64,

    // Window
    pub scale: f64,
}

impl StyleContext {
    /// Color for indent guide lines (uses selection color with reduced alpha).
    pub fn guide_color(&self) -> [u8; 4] {
        let c = self.selection.to_array();
        [c[0], c[1], c[2], (c[3] as u16 * 2 / 3).min(255) as u8]
    }

    /// `fg` adjusted to read clearly as body text on `bg`, keeping the
    /// theme's hue.
    pub fn text_on(&self, fg: Color, bg: Color) -> [u8; 4] {
        contrast::readable(fg, bg, contrast::MIN_TEXT).to_array()
    }

    /// `fg` adjusted to stay visible on `bg` while remaining visibly muted
    /// next to [`Self::text_on`]. Used for hints, counts, and paths, and for
    /// borders and other non-text marks.
    pub fn muted_on(&self, fg: Color, bg: Color) -> [u8; 4] {
        contrast::readable(fg, bg, contrast::MIN_UI).to_array()
    }

    /// `fill` adjusted to stay distinguishable from the `bg` it is painted
    /// on, so a highlighted row is identifiable at a glance.
    pub fn row_fill_on(&self, fill: Color, bg: Color) -> Color {
        contrast::readable(contrast::flatten(fill, bg), bg, contrast::MIN_UI)
    }

    /// Panel fill for a floating overlay: the command palette, the file and
    /// command pickers, the search overlays, and the popups.
    pub fn overlay_bg(&self) -> [u8; 4] {
        self.background3.to_array()
    }

    /// Border around an overlay panel, kept distinguishable from the panel
    /// fill so the overlay reads as a surface of its own.
    pub fn overlay_border(&self) -> [u8; 4] {
        self.muted_on(self.divider, self.background3)
    }

    /// Body text on an overlay panel.
    pub fn overlay_text(&self) -> [u8; 4] {
        self.on_overlay(self.text)
    }

    /// Labels, titles, and directory entries on an overlay panel.
    pub fn overlay_accent(&self) -> [u8; 4] {
        self.on_overlay(self.accent)
    }

    /// Secondary text - hints, counts, paths - on an overlay panel.
    pub fn overlay_dim(&self) -> [u8; 4] {
        self.muted_on(self.dim, self.background3)
    }

    /// `color` adjusted to read clearly on an overlay panel.
    pub fn on_overlay(&self, color: Color) -> [u8; 4] {
        self.text_on(color, self.background3)
    }

    /// Fill behind the highlighted row of an overlay list.
    pub fn overlay_row_bg(&self) -> [u8; 4] {
        self.overlay_row_surface().to_array()
    }

    /// Text on the highlighted row of an overlay list.
    pub fn overlay_row_text(&self) -> [u8; 4] {
        self.on_overlay_row(self.text)
    }

    /// Accent-tinted text on the highlighted row of an overlay list.
    pub fn overlay_row_accent(&self) -> [u8; 4] {
        self.on_overlay_row(self.accent)
    }

    /// Secondary text on the highlighted row of an overlay list.
    pub fn overlay_row_dim(&self) -> [u8; 4] {
        self.muted_on(self.dim, self.overlay_row_surface())
    }

    /// `color` adjusted to read clearly on the highlighted row.
    pub fn on_overlay_row(&self, color: Color) -> [u8; 4] {
        self.text_on(color, self.overlay_row_surface())
    }

    /// The surface an overlay panel's own text is drawn on.
    pub fn overlay_surface(&self) -> Color {
        self.background3
    }

    /// The surface the highlighted row of an overlay list is drawn on.
    pub fn overlay_row_surface(&self) -> Color {
        self.row_fill_on(self.selection, self.background3)
    }

    /// Text on the nag bar, kept legible against the bar's own fill.
    pub fn nag_text(&self) -> [u8; 4] {
        self.nag_text_surface().to_array()
    }

    /// The nag bar's text color as a surface, for the inverted buttons that
    /// fill with it and label themselves in the bar color.
    pub fn nag_text_surface(&self) -> Color {
        contrast::readable(self.nagbar_text, self.nagbar, contrast::MIN_TEXT)
    }

    /// The built-in palette, used before a theme loads and for any color a
    /// theme leaves unset.
    pub fn apply_default_colors(&mut self) {
        self.background = Color::new(40, 42, 54, 255);
        self.background2 = Color::new(34, 36, 46, 255);
        self.background3 = Color::new(48, 50, 62, 255);
        self.text = Color::new(215, 218, 224, 255);
        self.caret = Color::new(147, 161, 255, 255);
        self.accent = Color::new(97, 175, 239, 255);
        self.dim = Color::new(114, 120, 138, 255);
        self.divider = Color::new(24, 26, 34, 255);
        self.selection = Color::new(72, 79, 100, 255);
        self.line_number = Color::new(82, 88, 106, 255);
        self.line_number2 = Color::new(147, 161, 255, 255);
        self.line_highlight = Color::new(44, 47, 59, 255);
        self.scrollbar = Color::new(72, 79, 100, 255);
        self.scrollbar2 = Color::new(97, 175, 239, 255);
        self.good = Color::new(80, 200, 120, 255);
        self.warn = Color::new(255, 212, 121, 255);
        self.error = Color::new(255, 95, 86, 255);
        self.nagbar = Color::new(64, 64, 64, 255);
        self.nagbar_text = Color::new(255, 255, 255, 255);
        self.nagbar_dim = Color::new(0, 0, 0, 115);
    }

    /// Overwrite every color field this palette names, leaving the rest and
    /// all metrics untouched.
    pub fn apply_palette(&mut self, palette: &crate::editor::style::ThemePalette) {
        let set = |field: &mut Color, key: &str| {
            if let Some(c) = palette
                .colors
                .get(key)
                .and_then(|s| crate::editor::style::parse_color(s))
            {
                *field = c;
            }
        };
        set(&mut self.background, "background");
        set(&mut self.background2, "background2");
        set(&mut self.background3, "background3");
        set(&mut self.text, "text");
        set(&mut self.caret, "caret");
        set(&mut self.accent, "accent");
        set(&mut self.dim, "dim");
        set(&mut self.divider, "divider");
        set(&mut self.selection, "selection");
        set(&mut self.line_number, "line_number");
        set(&mut self.line_number2, "line_number2");
        set(&mut self.line_highlight, "line_highlight");
        set(&mut self.scrollbar, "scrollbar");
        set(&mut self.scrollbar2, "scrollbar2");
        set(&mut self.scrollbar_track, "scrollbar_track");
        set(&mut self.nagbar, "nagbar");
        set(&mut self.nagbar_text, "nagbar_text");
        set(&mut self.nagbar_dim, "nagbar_dim");
        set(&mut self.good, "good");
        set(&mut self.warn, "warn");
        set(&mut self.error, "error");
    }
}

impl Color {
    /// Convert to a [u8; 4] array for DrawContext calls.
    pub fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_style_context_is_zero() {
        let ctx = StyleContext::default();
        assert_eq!(ctx.font_height, 0.0);
        assert_eq!(ctx.padding_x, 0.0);
    }

    #[test]
    fn color_to_array() {
        let c = Color::new(255, 128, 64, 200);
        assert_eq!(c.to_array(), [255, 128, 64, 200]);
    }

    fn themed(name: &str) -> Option<StyleContext> {
        for dir in ["data", "../data"] {
            let path = format!("{dir}/assets/themes/{name}.json");
            if let Ok(palette) = crate::editor::style::load_theme_palette(&path) {
                let mut style = StyleContext::default();
                style.apply_default_colors();
                style.apply_palette(&palette);
                return Some(style);
            }
        }
        None
    }

    fn as_color(c: [u8; 4]) -> Color {
        Color::new(c[0], c[1], c[2], c[3])
    }

    #[test]
    fn every_builtin_theme_reads_on_an_overlay_panel() {
        let mut checked = 0;
        for name in crate::editor::style::builtin_theme_names() {
            let Some(style) = themed(name) else {
                continue;
            };
            checked += 1;
            let panel = as_color(style.overlay_bg());
            let row = as_color(style.overlay_row_bg());
            for (label, fg, bg, min) in [
                ("text", style.overlay_text(), panel, contrast::MIN_TEXT),
                ("accent", style.overlay_accent(), panel, contrast::MIN_TEXT),
                ("dim", style.overlay_dim(), panel, contrast::MIN_UI),
                ("border", style.overlay_border(), panel, contrast::MIN_UI),
                ("row fill", style.overlay_row_bg(), panel, contrast::MIN_UI),
                (
                    "row text",
                    style.overlay_row_text(),
                    row,
                    contrast::MIN_TEXT,
                ),
                (
                    "row accent",
                    style.overlay_row_accent(),
                    row,
                    contrast::MIN_TEXT,
                ),
                ("row dim", style.overlay_row_dim(), row, contrast::MIN_UI),
            ] {
                let got = contrast::ratio(as_color(fg), bg);
                assert!(
                    got >= min - 0.05,
                    "{name}: overlay {label} reaches only {got:.2}, needs {min}"
                );
            }

            // The context menu and the tab overlays sit on `background`.
            let page = style.background;
            let menu_row = style.row_fill_on(style.selection, page);
            for (label, fg, bg, min) in [
                (
                    "text",
                    style.text_on(style.text, page),
                    page,
                    contrast::MIN_TEXT,
                ),
                (
                    "hint",
                    style.muted_on(style.dim, page),
                    page,
                    contrast::MIN_UI,
                ),
                (
                    "border",
                    style.muted_on(style.divider, page),
                    page,
                    contrast::MIN_UI,
                ),
                ("row fill", menu_row.to_array(), page, contrast::MIN_UI),
                (
                    "row text",
                    style.text_on(style.accent, menu_row),
                    menu_row,
                    contrast::MIN_TEXT,
                ),
            ] {
                let got = contrast::ratio(as_color(fg), bg);
                assert!(
                    got >= min - 0.05,
                    "{name}: menu {label} reaches only {got:.2}, needs {min}"
                );
            }

            // The nag bar and its inverted buttons.
            let nag_fg = style.nag_text_surface();
            for (label, fg, bg) in [
                ("text", nag_fg.to_array(), style.nagbar),
                ("button label", style.text_on(style.nagbar, nag_fg), nag_fg),
            ] {
                let got = contrast::ratio(as_color(fg), bg);
                assert!(
                    got >= contrast::MIN_TEXT - 0.05,
                    "{name}: nag {label} reaches only {got:.2}"
                );
            }
        }
        assert!(checked > 0, "no bundled themes were found to check");
    }
}
