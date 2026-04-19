use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::{Color, Rect};
use crate::editor::view::{DrawContext, UpdateContext, View};

/// Window title bar button for native rendering.
#[derive(Debug, Clone)]
pub struct TitleButton {
    pub symbol: String,
    pub command: String,
}

/// Native title bar view state.
#[derive(Debug)]
pub struct TitleView {
    rect: Rect,
    pub visible: bool,
    pub title: String,
    pub buttons: Vec<TitleButton>,
    /// Index of the currently hovered button (-1 = none).
    pub hovered_index: i32,
    /// Icon logo symbols and their colors, drawn left of the title.
    pub icon_items: Vec<(String, Color)>,
    /// Separator inset from edges.
    pub separator_inset: f64,
}

impl TitleView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            visible: true,
            title: "Lite Anvil".into(),
            buttons: default_buttons(),
            hovered_index: -1,
            icon_items: default_icon_items(),
            separator_inset: 12.0,
        }
    }
}

impl Default for TitleView {
    fn default() -> Self {
        Self::new()
    }
}

fn default_buttons() -> Vec<TitleButton> {
    vec![
        TitleButton {
            symbol: "_".into(),
            command: "core:minimize".into(),
        },
        TitleButton {
            symbol: "W".into(),
            command: "core:toggle-maximize".into(),
        },
        TitleButton {
            symbol: "X".into(),
            command: "core:quit".into(),
        },
    ]
}

fn default_icon_items() -> Vec<(String, Color)> {
    vec![
        ("5".into(), Color::new(0x2e, 0x2e, 0x32, 0xff)),
        ("6".into(), Color::new(0xe1, 0xe1, 0xe6, 0xff)),
        ("7".into(), Color::new(0xff, 0xa9, 0x4d, 0xff)),
        ("8".into(), Color::new(0x93, 0xdd, 0xfa, 0xff)),
        ("9 ".into(), Color::new(0xf7, 0xc9, 0x5c, 0xff)),
    ]
}

impl TitleView {
    /// Draw the title bar natively.
    pub fn draw_native(
        &self,
        ctx: &mut dyn DrawContext,
        style: &crate::editor::style_ctx::StyleContext,
    ) {
        if !self.visible {
            return;
        }

        // Background.
        ctx.draw_rect(
            self.rect.x,
            self.rect.y,
            self.rect.w,
            self.rect.h,
            style.background2.to_array(),
        );

        // Compute control metrics.
        let icon_w = ctx.font_width(style.icon_font, "_");
        let spacing = (style.padding_x * 0.75).max((icon_w * 0.7).floor());
        let hit_width = (icon_w + style.padding_x).max(style.font_height);
        let n_buttons = self.buttons.len() as f64;
        let controls_width = hit_width * n_buttons + spacing;

        // Icon logo on the left.
        let inset = self.separator_inset;
        let mut x = self.rect.x + inset;
        let y = self.rect.y + style.padding_y;

        for (symbol, color) in &self.icon_items {
            let new_x = ctx.draw_text(style.icon_font, symbol, x, y, color.to_array());
            // Only advance x for the last item (the "9 " with trailing space).
            if symbol.ends_with(' ') {
                x = new_x;
            }
        }

        // Title text, clipped to available width.
        let title_max_w = (self.rect.w - controls_width - (x - self.rect.x) - inset).max(0.0);
        // Clip rect for title text.
        ctx.set_clip_rect(x, self.rect.y, title_max_w, self.rect.h);
        ctx.draw_text(style.font, &self.title, x, y, style.text.to_array());
        // Restore full clip.
        ctx.set_clip_rect(self.rect.x, self.rect.y, self.rect.w, self.rect.h);

        // Control buttons on the right.
        let btn_base_x = self.rect.x + self.rect.w - inset;
        let icon_h = ctx.font_height(style.icon_font);
        for (i, button) in self.buttons.iter().enumerate().rev() {
            let bx = btn_base_x - hit_width * (self.buttons.len() - i) as f64;
            let is_hovered = self.hovered_index == i as i32;

            if is_hovered {
                let mut hover_color = style.line_highlight.to_array();
                hover_color[3] = 140;
                ctx.draw_rect(bx, self.rect.y, hit_width, self.rect.h, hover_color);
            }

            let color = if is_hovered {
                style.text.to_array()
            } else {
                style.dim.to_array()
            };
            // Center the symbol in the button area.
            let sym_w = ctx.font_width(style.icon_font, &button.symbol);
            let sym_x = bx + (hit_width - sym_w) / 2.0;
            let sym_y = self.rect.y + (self.rect.h - icon_h) / 2.0;
            ctx.draw_text(style.icon_font, &button.symbol, sym_x, sym_y, color);
        }

        // Bottom divider.
        let div_y = self.rect.y + self.rect.h - style.divider_size;
        let div_w = (self.rect.w - inset * 2.0).max(0.0);
        ctx.draw_rect(
            self.rect.x + inset,
            div_y,
            div_w,
            style.divider_size,
            style.divider.to_array(),
        );
    }
}

impl View for TitleView {
    fn name(&self) -> &str {
        "Title"
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
    fn title_view_defaults() {
        let view = TitleView::new();
        assert_eq!(view.name(), "Title");
        assert!(view.visible);
        assert_eq!(view.buttons.len(), 3);
    }

    #[test]
    fn default_icon_items_has_five_entries() {
        let items = default_icon_items();
        assert_eq!(items.len(), 5);
    }
}
