#[cfg(feature = "sdl")]
use crate::renderer::{FontRef, RenColor, RenRect, with_cache};

use crate::editor::view::DrawContext;

/// Native DrawContext that pushes commands directly into the renderer cache.
/// Pushes commands directly into the renderer cache.
#[cfg(feature = "sdl")]
pub struct NativeDrawContext {
    /// Fonts available for drawing, indexed by a simple slot system.
    /// Slot 0 = UI font, Slot 1 = code font, etc.
    fonts: Vec<Vec<FontRef>>,
}

#[cfg(feature = "sdl")]
impl NativeDrawContext {
    /// Create a new context with the given font slots.
    pub fn new() -> Self {
        Self { fonts: Vec::new() }
    }

    /// Register a font group in a slot, returning the slot index.
    pub fn add_font(&mut self, font_refs: Vec<FontRef>) -> u64 {
        let id = self.fonts.len() as u64;
        self.fonts.push(font_refs);
        id
    }

    fn get_font(&self, font_id: u64) -> Option<&Vec<FontRef>> {
        self.fonts.get(font_id as usize)
    }

}

#[cfg(feature = "sdl")]
impl DrawContext for NativeDrawContext {
    fn draw_rect(&mut self, x: f64, y: f64, w: f64, h: f64, color: [u8; 4]) {
        with_cache(|c| {
            c.push_draw_rect(
                RenRect {
                    x: x as i32,
                    y: y as i32,
                    w: w as i32,
                    h: h as i32,
                },
                RenColor {
                    r: color[0],
                    g: color[1],
                    b: color[2],
                    a: color[3],
                },
            );
        });
    }

    fn draw_text(&mut self, font_id: u64, text: &str, x: f64, y: f64, color: [u8; 4]) -> f64 {
        let Some(fonts) = self.get_font(font_id) else {
            return x;
        };
        let fonts = fonts.clone();
        let mut result_x = x;
        with_cache(|c| {
            result_x = c.push_draw_text(
                fonts,
                text.to_string(),
                x as f32,
                y as i32,
                RenColor {
                    r: color[0],
                    g: color[1],
                    b: color[2],
                    a: color[3],
                },
                0.0,
            ) as f64;
        });
        result_x
    }

    fn set_clip_rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        with_cache(|c| {
            c.push_set_clip(RenRect {
                x: x as i32,
                y: y as i32,
                w: w as i32,
                h: h as i32,
            });
        });
    }

    fn font_height(&self, font_id: u64) -> f64 {
        self.get_font(font_id)
            .and_then(|fonts| fonts.first())
            .map(|f| f.lock().height as f64)
            .unwrap_or(14.0)
    }

    fn font_width(&self, font_id: u64, text: &str) -> f64 {
        self.get_font(font_id)
            .and_then(|fonts| fonts.first())
            .map(|f| f.lock().text_width(text, 0.0) as f64)
            .unwrap_or(0.0)
    }
}

/// Headless DrawContext for testing and non-SDL builds.
pub struct HeadlessDrawContext;

impl DrawContext for HeadlessDrawContext {
    fn draw_rect(&mut self, _x: f64, _y: f64, _w: f64, _h: f64, _color: [u8; 4]) {}
    fn draw_text(&mut self, _font_id: u64, _text: &str, x: f64, _y: f64, _color: [u8; 4]) -> f64 {
        x
    }
    fn set_clip_rect(&mut self, _x: f64, _y: f64, _w: f64, _h: f64) {}
    fn font_height(&self, _font_id: u64) -> f64 {
        14.0
    }
    fn font_width(&self, _font_id: u64, _text: &str) -> f64 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::view::DrawContext;

    #[test]
    fn headless_draw_context_works() {
        let mut ctx = HeadlessDrawContext;
        ctx.draw_rect(0.0, 0.0, 100.0, 100.0, [255, 0, 0, 255]);
        let x = ctx.draw_text(0, "hello", 10.0, 20.0, [255, 255, 255, 255]);
        assert_eq!(x, 10.0); // headless returns input x
        assert_eq!(ctx.font_height(0), 14.0);
    }
}
