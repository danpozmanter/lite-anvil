use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::Rect;
use crate::editor::view::{DrawContext, UpdateContext, View, ViewId};

/// Split direction for a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

/// A node in the view tree: either a leaf (with tabs) or a branch (with two children).
#[derive(Debug)]
pub enum NodeKind {
    Leaf {
        views: Vec<ViewId>,
        active_view: usize,
        tab_offset: usize,
    },
    Branch {
        direction: SplitDir,
        divider: f64,
        child_a: Box<Node>,
        child_b: Box<Node>,
    },
}

/// A node in the split pane tree.
#[derive(Debug)]
pub struct Node {
    pub rect: Rect,
    pub kind: NodeKind,
    pub locked: bool,
}

impl Node {
    /// Create a new empty leaf node.
    pub fn leaf() -> Self {
        Self {
            rect: Rect::default(),
            kind: NodeKind::Leaf {
                views: Vec::new(),
                active_view: 0,
                tab_offset: 1,
            },
            locked: false,
        }
    }

    /// Create a new branch node.
    pub fn branch(direction: SplitDir, divider: f64, a: Node, b: Node) -> Self {
        Self {
            rect: Rect::default(),
            kind: NodeKind::Branch {
                direction,
                divider,
                child_a: Box::new(a),
                child_b: Box::new(b),
            },
            locked: false,
        }
    }

    /// Is this a leaf node?
    pub fn is_leaf(&self) -> bool {
        matches!(self.kind, NodeKind::Leaf { .. })
    }
}

/// Native root view -- manages the top-level node tree.
#[derive(Debug)]
pub struct RootView {
    rect: Rect,
    pub root_node: Node,
    pub grab_mouse: bool,
}

impl RootView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            root_node: Node::leaf(),
            grab_mouse: false,
        }
    }
}

impl Default for RootView {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolved tab state for native rendering.
#[derive(Debug, Clone)]
pub struct TabInfo {
    pub name: String,
    pub is_active: bool,
    pub is_hovered: bool,
    pub is_close_hovered: bool,
    pub is_dirty: bool,
    /// Pre-computed tab rect (x, y, w, h).
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Parameters for native tab bar rendering.
#[derive(Debug)]
pub struct TabBarParams {
    pub bar_x: f64,
    pub bar_y: f64,
    pub bar_w: f64,
    pub bar_h: f64,
    pub margin_top: f64,
    pub tab_close_button: bool,
    /// -1 if no scroll buttons needed, else total tab count.
    pub total_tabs: i64,
    pub visible_tabs: i64,
    pub tab_offset: i64,
    pub hovered_scroll: i64,
    /// Scroll button rects: (x, y, w, h, padding) for left and right.
    pub scroll_left: Option<(f64, f64, f64, f64, f64)>,
    pub scroll_right: Option<(f64, f64, f64, f64, f64)>,
}

/// Draw the complete tab bar natively.
pub fn draw_tab_bar(
    ctx: &mut dyn DrawContext,
    style: &crate::editor::style_ctx::StyleContext,
    params: &TabBarParams,
    tabs: &[TabInfo],
) {
    // Tab bar background.
    ctx.draw_rect(
        params.bar_x,
        params.bar_y,
        params.bar_w,
        params.bar_h,
        style.background2.to_array(),
    );
    // Bottom divider line.
    ctx.draw_rect(
        params.bar_x,
        params.bar_y + params.bar_h - style.divider_size,
        params.bar_w,
        style.divider_size,
        style.divider.to_array(),
    );

    // Clip to tab bar area.
    ctx.set_clip_rect(params.bar_x, params.bar_y, params.bar_w, params.bar_h);

    let ds = style.divider_size;
    let pad_y_border = 2.0_f64.max((style.padding_y * 0.75).floor());

    for tab in tabs {
        let tx = tab.x;
        let ty = tab.y + params.margin_top;
        let tw = tab.w;
        let th = tab.h - params.margin_top;

        // Tab border: right-side divider between tabs.
        ctx.draw_rect(
            tx + tw,
            ty + pad_y_border,
            ds,
            th - pad_y_border * 2.0,
            style.dim.to_array(),
        );

        // Active tab gets filled background + border lines.
        if tab.is_active {
            ctx.draw_rect(tx, ty, tw, th, style.background.to_array());
            ctx.draw_rect(tx, ty, tw, ds, style.divider.to_array());
            ctx.draw_rect(tx + tw, ty, ds, th, style.divider.to_array());
            ctx.draw_rect(tx - ds, ty, ds, th, style.divider.to_array());
        }

        let bx = tx + ds;
        let by = ty;
        let bw = tw - ds * 2.0;
        let bh = th;

        // Close button.
        let icon_w = ctx.font_width(style.icon_font, "C");
        let hit_w = (icon_w + style.padding_x).max(style.font_height);
        let show_close =
            (tab.is_active || tab.is_hovered) && params.tab_close_button;

        let close_area_w = if show_close { hit_w } else { 0.0 };

        if show_close {
            let cx = bx + bw - hit_w;
            if tab.is_close_hovered {
                let mut hover_bg = style.line_highlight.to_array();
                hover_bg[3] = 150;
                ctx.draw_rect(
                    cx,
                    by + style.padding_y / 2.0,
                    hit_w,
                    bh - style.padding_y,
                    hover_bg,
                );
            }
            let close_color = if tab.is_close_hovered {
                style.text.to_array()
            } else {
                style.dim.to_array()
            };
            let cpad = (style.padding_x / 2.0).max(((hit_w - icon_w) / 2.0).floor());
            let icon_h = ctx.font_height(style.icon_font);
            ctx.draw_text(
                style.icon_font,
                "C",
                cx + cpad,
                by + (bh - icon_h) / 2.0,
                close_color,
            );
        }

        // Tab title with clipping.
        let title_x = bx + (style.padding_x / 2.0).max(((hit_w - icon_w) / 2.0).floor());
        let title_w = (bw - close_area_w - (title_x - bx)).max(0.0);
        ctx.set_clip_rect(title_x, by, title_w, bh);

        let mut draw_x = title_x;
        let _draw_w = title_w;

        // Dirty marker.
        if tab.is_dirty {
            let marker = "\u{2022} ";
            let marker_w = ctx.font_width(style.font, marker);
            let marker_color = style.accent.to_array();
            ctx.draw_text(
                style.font,
                marker,
                draw_x,
                by + (bh - style.font_height) / 2.0,
                marker_color,
            );
            draw_x += marker_w;
            let _ = (_draw_w - marker_w).max(0.0);
        }

        // Tab name.
        let text_color = if tab.is_active || tab.is_hovered {
            style.text.to_array()
        } else {
            style.dim.to_array()
        };
        ctx.draw_text(
            style.font,
            &tab.name,
            draw_x,
            by + (bh - style.font_height) / 2.0,
            text_color,
        );

        // Restore clip to full bar.
        ctx.set_clip_rect(params.bar_x, params.bar_y, params.bar_w, params.bar_h);
    }

    // Scroll buttons (if tabs overflow).
    if let (Some(left), Some(right)) = (&params.scroll_left, &params.scroll_right) {
        // Background behind scroll buttons.
        let pad = ctx.font_width(style.font, ">");
        ctx.draw_rect(
            left.0 + pad,
            left.1,
            left.2 * 2.0,
            params.bar_h,
            style.background2.to_array(),
        );

        // Left arrow "<".
        let left_color = if params.hovered_scroll == 1 && params.tab_offset > 1 {
            style.text.to_array()
        } else {
            style.dim.to_array()
        };
        ctx.draw_text(
            style.font,
            "<",
            left.0 + left.4,
            left.1 + (params.bar_h - style.font_height) / 2.0,
            left_color,
        );

        // Right arrow ">".
        let right_color =
            if params.hovered_scroll == 2 && params.total_tabs > params.tab_offset + params.visible_tabs - 1
            {
                style.text.to_array()
            } else {
                style.dim.to_array()
            };
        ctx.draw_text(
            style.font,
            ">",
            right.0 + right.4,
            right.1 + (params.bar_h - style.font_height) / 2.0,
            right_color,
        );
    }

}

impl Node {
    /// Draw a branch node's divider natively.
    pub fn draw_divider(
        &self,
        ctx: &mut dyn DrawContext,
        style: &crate::editor::style_ctx::StyleContext,
    ) {
        let NodeKind::Branch {
            direction, divider, ..
        } = &self.kind
        else {
            return;
        };

        match direction {
            SplitDir::Horizontal => {
                let div_x = self.rect.x + self.rect.w * divider - style.divider_size / 2.0;
                ctx.draw_rect(
                    div_x,
                    self.rect.y,
                    style.divider_size,
                    self.rect.h,
                    style.divider.to_array(),
                );
            }
            SplitDir::Vertical => {
                let div_y = self.rect.y + self.rect.h * divider - style.divider_size / 2.0;
                ctx.draw_rect(
                    self.rect.x,
                    div_y,
                    self.rect.w,
                    style.divider_size,
                    style.divider.to_array(),
                );
            }
        }
    }
}

impl View for RootView {
    fn name(&self) -> &str {
        "Root"
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
    fn leaf_node() {
        let node = Node::leaf();
        assert!(node.is_leaf());
    }

    #[test]
    fn branch_node() {
        let a = Node::leaf();
        let b = Node::leaf();
        let node = Node::branch(SplitDir::Horizontal, 0.5, a, b);
        assert!(!node.is_leaf());
    }

    #[test]
    fn root_view_defaults() {
        let view = RootView::new();
        assert_eq!(view.name(), "Root");
        assert!(!view.grab_mouse);
    }
}
