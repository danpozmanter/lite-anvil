use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::Rect;
use crate::editor::view::{DrawContext, UpdateContext, View};

/// A single log entry.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: String,
    pub text: String,
    pub time: f64,
}

/// Native log viewer state.
#[derive(Debug)]
pub struct LogView {
    rect: Rect,
    pub entries: Vec<LogEntry>,
    pub scroll_y: f64,
    pub last_item_count: usize,
}

impl LogView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            entries: Vec::new(),
            scroll_y: 0.0,
            last_item_count: 0,
        }
    }

    /// Replace the log entries shown in the view.
    pub fn sync_entries(&mut self, entries: Vec<LogEntry>) {
        self.last_item_count = entries.len();
        self.entries = entries;
    }
}

impl Default for LogView {
    fn default() -> Self {
        Self::new()
    }
}

impl LogView {
    /// Draw log entries natively.
    pub fn draw_native(
        &self,
        ctx: &mut dyn DrawContext,
        style: &crate::editor::style_ctx::StyleContext,
    ) {
        // Background
        ctx.draw_rect(
            self.rect.x,
            self.rect.y,
            self.rect.w,
            self.rect.h,
            style.background.to_array(),
        );

        let item_h = style.font_height + style.padding_y;
        let visible_start = self.scroll_y;
        let visible_end = self.scroll_y + self.rect.h;

        for (i, entry) in self.entries.iter().enumerate() {
            let y = self.rect.y + (i as f64 * item_h) - self.scroll_y;
            if y + item_h < self.rect.y || y > self.rect.y + self.rect.h {
                continue;
            }
            let mut x = self.rect.x + style.padding_x;
            let text_y = y + (item_h - style.font_height) / 2.0;

            // Level icon
            let icon = match entry.level.as_str() {
                "ERROR" => "!",
                "WARN" => "!",
                _ => "i",
            };
            let color = match entry.level.as_str() {
                "ERROR" => style.error.to_array(),
                "WARN" => style.warn.to_array(),
                _ => style.text.to_array(),
            };
            let adv = ctx.draw_text(style.icon_font, icon, x, text_y, color);
            x = adv + style.padding_x;

            // Text
            ctx.draw_text(style.font, &entry.text, x, text_y, style.text.to_array());
        }

        let _ = (visible_start, visible_end);
    }
}

impl View for LogView {
    fn name(&self) -> &str {
        "Log"
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
    fn log_view_defaults() {
        let view = LogView::new();
        assert_eq!(view.name(), "Log");
        assert!(view.entries.is_empty());
    }

    #[test]
    fn log_view_sync() {
        let mut view = LogView::new();
        view.sync_entries(vec![LogEntry {
            level: "INFO".into(),
            text: "test message".into(),
            time: 1.0,
        }]);
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.last_item_count, 1);
    }
}
