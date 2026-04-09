use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::Rect;
use crate::editor::view::{DrawContext, UpdateContext, View};

/// A status bar item segment.
#[derive(Debug, Clone)]
pub struct StatusItem {
    pub text: String,
    pub color: Option<[u8; 4]>,
    pub command: Option<String>,
}

/// Native status bar view state.
#[derive(Debug)]
pub struct StatusView {
    rect: Rect,
    pub visible: bool,
    pub left_items: Vec<StatusItem>,
    pub right_items: Vec<StatusItem>,
    pub message: Option<String>,
    pub message_timeout: f64,
    pub tooltip: Option<String>,
    pub left_panel_offset: f64,
    pub right_panel_offset: f64,
}

impl StatusView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            visible: true,
            left_items: Vec::new(),
            right_items: Vec::new(),
            message: None,
            message_timeout: 0.0,
            tooltip: None,
            left_panel_offset: 0.0,
            right_panel_offset: 0.0,
        }
    }
}

impl Default for StatusView {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusView {
    /// Draw the status bar natively.
    pub fn draw_native(
        &self,
        ctx: &mut dyn DrawContext,
        style: &crate::editor::style_ctx::StyleContext,
    ) {
        if !self.visible {
            return;
        }
        // Background
        ctx.draw_rect(
            self.rect.x,
            self.rect.y,
            self.rect.w,
            self.rect.h,
            style.background2.to_array(),
        );

        let y = self.rect.y;
        let h = self.rect.h;
        let text_y = y + (h - style.font_height) / 2.0;

        // Left items
        let mut x = self.rect.x + style.padding_x;
        for item in &self.left_items {
            let color = item.color.unwrap_or(style.text.to_array());
            let adv = ctx.draw_text(style.font, &item.text, x, text_y, color);
            x = adv + style.padding_x / 2.0;
        }

        // Right items (drawn right-to-left)
        let mut rx = self.rect.x + self.rect.w - style.padding_x;
        for item in self.right_items.iter().rev() {
            let w = ctx.font_width(style.font, &item.text);
            rx -= w;
            let color = item.color.unwrap_or(style.text.to_array());
            ctx.draw_text(style.font, &item.text, rx, text_y, color);
            rx -= style.padding_x / 2.0;
        }

        // Message overlay
        if let Some(ref msg) = self.message {
            let msg_w = ctx.font_width(style.font, msg);
            let msg_x = self.rect.x + (self.rect.w - msg_w) / 2.0;
            ctx.draw_rect(
                msg_x - style.padding_x,
                y,
                msg_w + style.padding_x * 2.0,
                h,
                style.background2.to_array(),
            );
            ctx.draw_text(style.font, msg, msg_x, text_y, style.accent.to_array());
        }
    }
}

impl View for StatusView {
    fn name(&self) -> &str {
        "Status"
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
    fn status_view_defaults() {
        let v = StatusView::new();
        assert_eq!(v.name(), "Status");
        assert!(v.left_items.is_empty());
    }
}
