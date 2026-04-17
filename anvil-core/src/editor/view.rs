use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::Rect;

/// Drawing context passed to views during the render phase. Concrete
/// impls talk to SDL for real rendering; tests use a recording stub.
pub trait DrawContext {
    /// Draw a filled rectangle.
    fn draw_rect(&mut self, x: f64, y: f64, w: f64, h: f64, color: [u8; 4]);
    /// Draw text. Returns the x-advance.
    fn draw_text(&mut self, font_id: u64, text: &str, x: f64, y: f64, color: [u8; 4]) -> f64;
    /// Set the clip rectangle.
    fn set_clip_rect(&mut self, x: f64, y: f64, w: f64, h: f64);
    /// Get font height.
    fn font_height(&self, font_id: u64) -> f64;
    /// Get text width.
    fn font_width(&self, font_id: u64, text: &str) -> f64;
    /// Draw an RGBA image at the given position.
    fn draw_image(
        &mut self,
        data: &std::sync::Arc<Vec<u8>>,
        width: i32,
        height: i32,
        x: f64,
        y: f64,
    );
}

/// Update context passed to views during the update phase.
pub struct UpdateContext {
    pub dt: f64,
    pub window_width: f64,
    pub window_height: f64,
}

/// The core View trait. Every UI element implements this.
pub trait View: std::fmt::Debug {
    /// Human-readable name for display (e.g. tab title).
    fn name(&self) -> &str;

    /// Update state. Called every frame before draw.
    fn update(&mut self, ctx: &UpdateContext);

    /// Draw the view within its current position and size.
    fn draw(&self, ctx: &mut dyn DrawContext);

    /// Handle an input event. Return Consumed if handled.
    fn on_event(&mut self, event: &EditorEvent) -> EventResult;

    /// Current position and size.
    fn rect(&self) -> Rect;

    /// Set position and size.
    fn set_rect(&mut self, rect: Rect);

    /// Whether this view wants keyboard focus.
    fn focusable(&self) -> bool {
        false
    }
}

/// Identifies a view uniquely within the view tree.
pub type ViewId = u64;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestView {
        rect: Rect,
    }

    impl View for TestView {
        fn name(&self) -> &str {
            "TestView"
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

    #[test]
    fn view_trait_is_object_safe() {
        let view: Box<dyn View> = Box::new(TestView {
            rect: Rect::default(),
        });
        assert_eq!(view.name(), "TestView");
        assert!(!view.focusable());
    }

    #[test]
    fn update_context_fields() {
        let ctx = UpdateContext {
            dt: 0.016,
            window_width: 1920.0,
            window_height: 1080.0,
        };
        assert!(ctx.dt > 0.0);
    }
}
