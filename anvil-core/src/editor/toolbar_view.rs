use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::Rect;
use crate::editor::view::{DrawContext, UpdateContext, View};

/// A toolbar button.
#[derive(Debug, Clone)]
pub struct ToolbarItem {
    pub icon: String,
    pub command: String,
    pub tooltip: String,
}

/// Native toolbar view state.
#[derive(Debug)]
pub struct ToolbarView {
    rect: Rect,
    pub visible: bool,
    pub items: Vec<ToolbarItem>,
    pub active_index: Option<usize>,
}

impl ToolbarView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            visible: true,
            items: Vec::new(),
            active_index: None,
        }
    }
}

impl Default for ToolbarView {
    fn default() -> Self {
        Self::new()
    }
}

/// Draw toolbar items from the provided symbols + bounding rects.
pub fn draw_toolbar(
    ctx: &mut dyn DrawContext,
    style: &crate::editor::style_ctx::StyleContext,
    item_rects: &[(f64, f64, f64, f64)],
    symbols: &[String],
    hovered_valid: &[bool],
) {
    let icon_h = ctx.font_height(style.big_font);
    for (i, (x, y, _w, h)) in item_rects.iter().enumerate() {
        let symbol = match symbols.get(i) {
            Some(s) => s.as_str(),
            None => continue,
        };
        let color = if hovered_valid.get(i).copied().unwrap_or(false) {
            style.text.to_array()
        } else {
            style.dim.to_array()
        };
        ctx.draw_text(style.big_font, symbol, *x, y + (h - icon_h) / 2.0, color);
    }
}

impl View for ToolbarView {
    fn name(&self) -> &str {
        "Toolbar"
    }
    fn update(&mut self, _ctx: &UpdateContext) {}
    fn draw(&self, _ctx: &mut dyn DrawContext) {}
    fn on_event(&mut self, _event: &EditorEvent) -> EventResult {
        EventResult::Ignored
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_view_defaults() {
        let view = ToolbarView::new();
        assert_eq!(view.name(), "Toolbar");
        assert!(view.visible);
    }
}
