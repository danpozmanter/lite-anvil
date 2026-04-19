use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::style_ctx::StyleContext;
use crate::editor::types::Rect;
use crate::editor::view::{DrawContext, UpdateContext, View};

/// Commands shown on the get-started splash screen.
const COMMANDS: &[(&str, &str)] = &[
    ("%s to run a command", "core:find-command"),
    ("%s for shortcuts", "core:show-shortcuts-help"),
    ("%s to open a file", "core:open-file"),
    ("%s to open a file from the project", "core:find-file"),
    ("%s to open a recent file or folder", "core:open-recent"),
    ("%s to toggle focus mode", "root:toggle-focus-mode"),
    (
        "%s to close the project folder",
        "core:close-project-folder",
    ),
    ("%s to change project folder", "core:change-project-folder"),
    ("%s to open a project folder", "core:open-project-folder"),
];

/// The "Get Started" splash screen.
#[derive(Debug)]
pub struct EmptyView {
    rect: Rect,
    /// Pre-resolved display strings for commands (e.g. "Open File (Ctrl+O)"),
    /// populated by the caller that has access to the current keymap.
    pub display_commands: Vec<String>,
    pub version: String,
}

impl EmptyView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            display_commands: Vec::new(),
            version: String::new(),
        }
    }

    /// Command definitions for the splash screen.
    pub fn commands() -> &'static [(&'static str, &'static str)] {
        COMMANDS
    }

    /// Draw using a StyleContext (native rendering path).
    pub fn draw_native(&self, ctx: &mut dyn DrawContext, style: &StyleContext) {
        let x = self.rect.x + self.rect.w / 2.0;
        let y = self.rect.y + self.rect.h / 2.0;
        let divider_w = (1.0 * style.scale).ceil();
        let cmds_x = x + (divider_w / 2.0).ceil() + style.padding_x;
        let logo_right = x - (divider_w / 2.0).ceil() - style.padding_x;

        // Background
        ctx.draw_rect(
            self.rect.x,
            self.rect.y,
            self.rect.w,
            self.rect.h,
            style.background.to_array(),
        );

        // Commands
        let cmd_h = style.font_height + style.padding_y;
        let count = self.display_commands.len() as f64;
        let cmds_y = y - (cmd_h * count) / 2.0;

        for (i, text) in self.display_commands.iter().enumerate() {
            ctx.draw_text(
                style.font,
                text,
                cmds_x,
                cmds_y + cmd_h * i as f64,
                style.dim.to_array(),
            );
        }

        // Title
        let title = &self.version[..self.version.find(" v").unwrap_or(self.version.len())];
        let big_h = ctx.font_height(style.big_font);
        let big_w = ctx.font_width(style.big_font, title);
        let logo_y = y - big_h + big_h / 4.0;
        let logo_x = logo_right - big_w;

        ctx.draw_text(style.big_font, title, logo_x, logo_y, style.dim.to_array());

        // Version
        let vers_w = ctx.font_width(style.font, &self.version);
        let vers_x = logo_right - vers_w;
        let vers_y = y + big_h / 8.0;

        ctx.draw_text(
            style.font,
            &self.version,
            vers_x,
            vers_y,
            style.dim.to_array(),
        );

        // Divider
        let divider_y = (cmds_y).min(logo_y) - style.padding_y;
        let divider_h = (y - divider_y) * 2.0;
        ctx.draw_rect(
            x - divider_w / 2.0,
            divider_y,
            divider_w,
            divider_h,
            style.dim.to_array(),
        );
    }
}

impl Default for EmptyView {
    fn default() -> Self {
        Self::new()
    }
}

impl View for EmptyView {
    fn name(&self) -> &str {
        "Get Started"
    }

    fn update(&mut self, _ctx: &UpdateContext) {}

    fn draw(&self, ctx: &mut dyn DrawContext) {
        // When called without StyleContext, draw nothing.
        // Use draw_native() with a StyleContext for full rendering.
        let _ = ctx;
    }

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
    use crate::editor::draw_context::HeadlessDrawContext;

    #[test]
    fn empty_view_name() {
        let view = EmptyView::new();
        assert_eq!(view.name(), "Get Started");
    }

    #[test]
    fn empty_view_commands() {
        let cmds = EmptyView::commands();
        assert!(cmds.len() >= 8);
        assert_eq!(cmds[0].1, "core:find-command");
    }

    #[test]
    fn empty_view_draw_native_with_headless() {
        let mut view = EmptyView::new();
        view.version = "2.0.0".into();
        view.display_commands = vec!["Ctrl+P to run a command".into()];
        view.set_rect(Rect {
            x: 0.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        });

        let style = StyleContext {
            font_height: 15.0,
            padding_x: 14.0,
            padding_y: 7.0,
            scale: 1.0,
            ..Default::default()
        };
        let mut ctx = HeadlessDrawContext;
        view.draw_native(&mut ctx, &style);
        // No panic = success (headless doesn't render)
    }

    #[test]
    fn empty_view_as_dyn_view() {
        let view: Box<dyn View> = Box::new(EmptyView::new());
        assert_eq!(view.name(), "Get Started");
        assert!(!view.focusable());
    }
}
