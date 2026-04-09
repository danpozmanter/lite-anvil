use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::Rect;
use crate::editor::view::{DrawContext, UpdateContext, View};

/// A button option in the nag bar.
#[derive(Debug, Clone)]
pub struct NagOption {
    pub text: String,
    pub default_yes: bool,
    pub default_no: bool,
}

/// Resolved button layout for native rendering.
#[derive(Debug, Clone)]
pub struct NagButton {
    pub index: i64,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Native notification/confirmation bar state.
#[derive(Debug)]
pub struct NagView {
    rect: Rect,
    pub visible: bool,
    pub title: String,
    pub message: String,
    pub options: Vec<NagOption>,
    pub selected: usize,
    pub show_height: f64,
    pub target_height: f64,
    pub hovered_item: i64,
    pub underline_progress: f64,
    pub dim_alpha: f64,
    pub queue_count: i64,
    /// Pre-computed button positions from Lua each_option.
    pub buttons: Vec<NagButton>,
    /// Root view size for dimming overlay.
    pub root_w: f64,
    pub root_h: f64,
}

impl NagView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            visible: false,
            title: String::new(),
            message: String::new(),
            options: Vec::new(),
            selected: 0,
            show_height: 0.0,
            target_height: 0.0,
            hovered_item: -1,
            underline_progress: 0.0,
            dim_alpha: 0.0,
            queue_count: 0,
            buttons: Vec::new(),
            root_w: 0.0,
            root_h: 0.0,
        }
    }

    /// Draw the nag bar natively.
    pub fn draw_native(
        &self,
        ctx: &mut dyn DrawContext,
        style: &crate::editor::style_ctx::StyleContext,
        scale: f64,
        line_height: f64,
    ) {
        if !self.visible && self.show_height <= 0.0 {
            return;
        }

        let border_width = (1.0 * scale).round();
        let underline_width = (2.0 * scale).round();
        let underline_margin = (1.0 * scale).round();

        let ox = self.rect.x;
        let oy = self.rect.y;
        let w = self.rect.w;

        // Dim overlay below the nag bar.
        if self.dim_alpha > 0.0 {
            let dim_y = oy + self.show_height;
            let dim_h = self.root_h - dim_y;
            let mut dim_color = style.nagbar_dim.to_array();
            dim_color[3] = (dim_color[3] as f64 * self.dim_alpha) as u8;
            ctx.draw_rect(ox, dim_y, self.root_w, dim_h, dim_color);
        }

        // Nag bar background.
        ctx.draw_rect(ox, oy, w, self.show_height, style.nagbar.to_array());

        // Clip to nag bar area.
        ctx.set_clip_rect(ox + style.padding_x, oy, w, self.show_height);

        let mut text_x = ox + style.padding_x;

        // Queue count.
        if self.queue_count > 0 {
            let text = format!("[{}]", self.queue_count);
            let adv = ctx.draw_text(
                style.font,
                &text,
                text_x,
                oy + (self.show_height - style.font_height) / 2.0,
                style.nagbar_text.to_array(),
            );
            text_x = adv + style.padding_x;
        }

        // Message text (multiline).
        let lh = style.font_height * line_height;
        let msg_height = self.message.matches('\n').count() as f64 * lh;
        let text_y_offset = (lh - style.font_height) / 2.0;
        let mut msg_y = oy + style.padding_y + (self.target_height - msg_height) / 2.0;

        for line in self.message.split('\n') {
            if line.is_empty() {
                continue;
            }
            ctx.draw_text(
                style.font,
                line,
                text_x,
                msg_y + text_y_offset,
                style.nagbar_text.to_array(),
            );
            msg_y += lh;
        }

        // Buttons.
        for button in &self.buttons {
            let fw = button.w - 2.0 * border_width;
            let fh = button.h - 2.0 * border_width;
            let fx = button.x + border_width;
            let fy = button.y + border_width;

            // Button border.
            ctx.draw_rect(
                button.x,
                button.y,
                button.w,
                button.h,
                style.nagbar_text.to_array(),
            );
            // Button fill.
            ctx.draw_rect(fx, fy, fw, fh, style.nagbar.to_array());

            // Hover underline animation.
            if button.index == self.hovered_item {
                let uw = fw - 2.0 * underline_margin;
                let halfuw = uw / 2.0;
                let lx = fx + underline_margin + halfuw - (halfuw * self.underline_progress);
                let ly = fy + fh - underline_margin - underline_width;
                let drawn_w = uw * self.underline_progress;
                ctx.draw_rect(lx, ly, drawn_w, underline_width, style.nagbar_text.to_array());
            }

            // Button text centered.
            let text_w = ctx.font_width(style.font, &button.text);
            let text_x = fx + (fw - text_w) / 2.0;
            let text_y = fy + (fh - style.font_height) / 2.0;
            ctx.draw_text(
                style.font,
                &button.text,
                text_x,
                text_y,
                style.nagbar_text.to_array(),
            );
        }

    }
}

impl Default for NagView {
    fn default() -> Self {
        Self::new()
    }
}

impl View for NagView {
    fn name(&self) -> &str {
        "Nag"
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
    fn nag_view_defaults() {
        let v = NagView::new();
        assert_eq!(v.name(), "Nag");
        assert!(!v.visible);
    }
}
