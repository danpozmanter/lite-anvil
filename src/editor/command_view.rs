use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::Rect;
use crate::editor::view::{DrawContext, UpdateContext, View};

/// A suggestion in the command palette.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub text: String,
    pub info: Option<String>,
}

/// Native command palette state.
#[derive(Debug)]
pub struct CommandView {
    rect: Rect,
    pub input_text: String,
    pub suggestions: Vec<Suggestion>,
    pub suggestion_idx: usize,
    pub visible: bool,
    pub label: String,
}

impl CommandView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            input_text: String::new(),
            suggestions: Vec::new(),
            suggestion_idx: 0,
            visible: false,
            label: String::new(),
        }
    }
}

impl Default for CommandView {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandView {
    /// Draw the command palette natively.
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
            style.background.to_array(),
        );

        let x = self.rect.x + style.padding_x;
        let y = self.rect.y + style.padding_y;
        let item_h = style.font_height + style.padding_y;

        // Label
        if !self.label.is_empty() {
            ctx.draw_text(style.font, &self.label, x, y, style.dim.to_array());
        }

        // Input text
        let input_y = y + if self.label.is_empty() { 0.0 } else { item_h };
        ctx.draw_text(
            style.font,
            &self.input_text,
            x,
            input_y,
            style.text.to_array(),
        );

        // Suggestions
        let list_y = input_y + item_h + style.padding_y;
        for (i, suggestion) in self.suggestions.iter().enumerate() {
            let sy = list_y + item_h * i as f64;
            let is_selected = i == self.suggestion_idx;
            if is_selected {
                ctx.draw_rect(
                    self.rect.x,
                    sy,
                    self.rect.w,
                    item_h,
                    style.selection.to_array(),
                );
            }
            let color = if is_selected {
                style.accent.to_array()
            } else {
                style.text.to_array()
            };
            ctx.draw_text(
                style.font,
                &suggestion.text,
                x,
                sy + (item_h - style.font_height) / 2.0,
                color,
            );
            if let Some(ref info) = suggestion.info {
                let iw = ctx.font_width(style.font, info);
                ctx.draw_text(
                    style.font,
                    info,
                    self.rect.x + self.rect.w - iw - style.padding_x,
                    sy + (item_h - style.font_height) / 2.0,
                    style.dim.to_array(),
                );
            }
        }
    }
}

impl View for CommandView {
    fn name(&self) -> &str {
        "Command"
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
    fn focusable(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_view_defaults() {
        let v = CommandView::new();
        assert_eq!(v.name(), "Command");
        assert!(v.focusable());
        assert!(v.suggestions.is_empty());
    }
}
