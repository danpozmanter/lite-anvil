use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::editor::types::Color;

/// Global style context, synced once per frame.
static STYLE: Lazy<Mutex<StyleContext>> = Lazy::new(|| Mutex::new(StyleContext::default()));

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
}
