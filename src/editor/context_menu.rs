use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::{Point, Rect};
use crate::editor::view::{DrawContext, UpdateContext, View};

/// A single item in a context menu.
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub text: String,
    pub info: Option<String>,
    pub command: Option<String>,
    pub separator: bool,
}

/// Native context menu state.
#[derive(Debug)]
pub struct ContextMenu {
    rect: Rect,
    pub visible: bool,
    pub items: Vec<MenuItem>,
    pub selected: Option<usize>,
    pub position: Point,
}

impl ContextMenu {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            visible: false,
            items: Vec::new(),
            selected: None,
            position: Point::default(),
        }
    }

    /// Show at the given position with the given items.
    pub fn show(&mut self, x: f64, y: f64, items: Vec<MenuItem>) {
        self.position = Point { x, y };
        self.items = items;
        self.selected = None;
        self.visible = true;
    }

    /// Hide the menu.
    pub fn hide(&mut self) {
        self.visible = false;
        self.items.clear();
        self.selected = None;
    }
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextMenu {
    /// Draw the context menu natively.
    pub fn draw_native(
        &self,
        ctx: &mut dyn DrawContext,
        style: &crate::editor::style_ctx::StyleContext,
    ) {
        if !self.visible || self.items.is_empty() {
            return;
        }
        let item_h = style.font_height + style.padding_y;
        let total_h = item_h * self.items.len() as f64 + style.padding_y;
        let mut max_w = 0.0f64;
        for item in &self.items {
            let w = ctx.font_width(style.font, &item.text);
            if let Some(ref info) = item.info {
                let iw = ctx.font_width(style.font, info);
                max_w = max_w.max(w + iw + style.padding_x * 3.0);
            } else {
                max_w = max_w.max(w + style.padding_x * 2.0);
            }
        }
        let x = self.position.x;
        let y = self.position.y;

        // Background + border
        ctx.draw_rect(
            x - 1.0,
            y - 1.0,
            max_w + 2.0,
            total_h + 2.0,
            style.divider.to_array(),
        );
        ctx.draw_rect(x, y, max_w, total_h, style.background.to_array());

        let mut iy = y + style.padding_y / 2.0;
        for (i, item) in self.items.iter().enumerate() {
            if item.separator {
                let sep_y = iy + item_h / 2.0;
                ctx.draw_rect(
                    x + style.padding_x,
                    sep_y,
                    max_w - style.padding_x * 2.0,
                    1.0,
                    style.divider.to_array(),
                );
                iy += item_h;
                continue;
            }
            let is_selected = self.selected == Some(i);
            if is_selected {
                ctx.draw_rect(x, iy, max_w, item_h, style.selection.to_array());
            }
            let color = if is_selected {
                style.accent.to_array()
            } else {
                style.text.to_array()
            };
            ctx.draw_text(
                style.font,
                &item.text,
                x + style.padding_x,
                iy + (item_h - style.font_height) / 2.0,
                color,
            );
            if let Some(ref info) = item.info {
                let info_w = ctx.font_width(style.font, info);
                ctx.draw_text(
                    style.font,
                    info,
                    x + max_w - info_w - style.padding_x,
                    iy + (item_h - style.font_height) / 2.0,
                    style.dim.to_array(),
                );
            }
            iy += item_h;
        }
    }
}

impl View for ContextMenu {
    fn name(&self) -> &str {
        "Context Menu"
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
    fn context_menu_show_hide() {
        let mut menu = ContextMenu::new();
        assert!(!menu.visible);
        menu.show(
            100.0,
            200.0,
            vec![
                MenuItem {
                    text: "Cut".into(),
                    info: None,
                    command: Some("doc:cut".into()),
                    separator: false,
                },
                MenuItem {
                    text: "Copy".into(),
                    info: None,
                    command: Some("doc:copy".into()),
                    separator: false,
                },
            ],
        );
        assert!(menu.visible);
        assert_eq!(menu.items.len(), 2);
        assert_eq!(menu.position.x, 100.0);
        menu.hide();
        assert!(!menu.visible);
        assert!(menu.items.is_empty());
    }
}
