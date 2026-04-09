use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::Rect;
use crate::editor::view::{DrawContext, UpdateContext, View};

/// Native file tree sidebar state.
#[derive(Debug)]
pub struct TreeView {
    rect: Rect,
    pub visible: bool,
    pub scroll_y: f64,
    pub target_scroll_y: f64,
    pub selected_path: Option<String>,
    pub hovered_index: Option<usize>,
    pub target_width: f64,
    pub current_width: f64,
}

impl TreeView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            visible: true,
            scroll_y: 0.0,
            target_scroll_y: 0.0,
            selected_path: None,
            hovered_index: None,
            target_width: 200.0,
            current_width: 200.0,
        }
    }
}

impl Default for TreeView {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolved item for native tree drawing.
#[derive(Debug, Clone)]
pub struct NativeTreeItem {
    pub name: String,
    pub depth: f64,
    pub is_dir: bool,
    pub expanded: bool,
    pub is_active: bool,
    pub is_hovered: bool,
    pub is_ignored: bool,
    pub icon_char: String,
    /// Y position (absolute).
    pub y: f64,
    /// Item height.
    pub h: f64,
    /// Content offset x.
    pub ox: f64,
    /// Chevron width.
    pub chevron_w: f64,
    /// Icon width + text spacing.
    pub icon_offset: f64,
}

/// Draw all visible tree items natively.
pub fn draw_tree_items(
    ctx: &mut dyn DrawContext,
    style: &crate::editor::style_ctx::StyleContext,
    view_x: f64,
    view_w: f64,
    items: &[NativeTreeItem],
    icon_vertical_nudge: f64,
) {
    for item in items {
        let y = item.y;
        let h = item.h;
        let base_x = item.ox + item.depth * style.padding_x + style.padding_x;

        // Background for active/hovered.
        if item.is_active {
            let mut c = style.line_highlight.to_array();
            c[3] = c[3].max(210);
            ctx.draw_rect(view_x, y, view_w, h, c);
        } else if item.is_hovered {
            let mut c = style.line_highlight.to_array();
            c[3] = 110;
            ctx.draw_rect(view_x, y, view_w, h, c);
        }

        let mut draw_x = base_x;

        // Chevron for directories.
        if item.is_dir {
            let chevron = if item.expanded { "-" } else { "+" };
            let chevron_color = if item.is_hovered {
                style.accent.to_array()
            } else {
                style.text.to_array()
            };
            let text_top = y + ((h - style.font_height) / 2.0).round();
            let icon_h = ctx.font_height(style.icon_font);
            let iy = text_top + ((style.font_height - icon_h) / 2.0).round() - icon_vertical_nudge;
            ctx.draw_text(style.icon_font, chevron, draw_x, iy, chevron_color);
        }
        draw_x += item.chevron_w;

        // Icon.
        let icon_color = if item.is_active || item.is_hovered {
            style.accent.to_array()
        } else if item.is_ignored {
            style.dim.to_array()
        } else {
            style.text.to_array()
        };
        let text_top = y + ((h - style.font_height) / 2.0).round();
        let icon_h = ctx.font_height(style.icon_font);
        let iy = text_top + ((style.font_height - icon_h) / 2.0).round() - icon_vertical_nudge;
        ctx.draw_text(style.icon_font, &item.icon_char, draw_x, iy, icon_color);
        draw_x += item.icon_offset;

        // Text.
        let text_color = if item.is_active || item.is_hovered {
            style.accent.to_array()
        } else if item.is_ignored {
            style.dim.to_array()
        } else {
            style.text.to_array()
        };
        ctx.draw_text(
            style.font,
            &item.name,
            draw_x,
            y + (h - style.font_height) / 2.0,
            text_color,
        );
    }

}

impl View for TreeView {
    fn name(&self) -> &str {
        "File Tree"
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
    fn tree_view_defaults() {
        let v = TreeView::new();
        assert_eq!(v.name(), "File Tree");
        assert!(v.visible);
    }
}
