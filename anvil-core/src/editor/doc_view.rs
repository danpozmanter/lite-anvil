use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::types::Rect;
use crate::editor::view::{DrawContext, UpdateContext, View};

/// Native document editor view state.
/// The actual buffer is managed by native::buffer::BufferState.
#[derive(Debug)]
pub struct DocView {
    rect: Rect,
    pub buffer_id: Option<u64>,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub target_scroll_x: f64,
    pub target_scroll_y: f64,
    pub blink_timer: f64,
    pub last_line_count: usize,
    pub gutter_width: f64,
    /// Cached monospace character width in pixels for the code font, refreshed
    /// each frame from the active draw context. Used for horizontal scroll math
    /// in command handlers that don't have direct draw-context access.
    pub code_char_w: f64,
    pub indent_size: usize,
    /// Fold ranges: Vec of (start_line, end_line) where lines start+1..=end are hidden.
    pub folds: Vec<(usize, usize)>,
    /// Whether to render whitespace markers (dots for spaces, arrows for tabs).
    pub show_whitespace: bool,
    /// Bookmarked lines (1-based, kept sorted).
    pub bookmarks: Vec<usize>,
}

impl DocView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            buffer_id: None,
            scroll_x: 0.0,
            scroll_y: 0.0,
            target_scroll_x: 0.0,
            target_scroll_y: 0.0,
            blink_timer: 0.0,
            last_line_count: 0,
            gutter_width: 0.0,
            code_char_w: 0.0,
            indent_size: 4,
            folds: Vec::new(),
            show_whitespace: false,
            bookmarks: Vec::new(),
        }
    }
}

impl Default for DocView {
    fn default() -> Self {
        Self::new()
    }
}

/// A resolved line for native document drawing.
///
/// When line wrapping is off, each logical line produces exactly one
/// `RenderLine` with `wrap_start_col = 0`. When wrapping is on, a long
/// logical line is split into multiple `RenderLine` entries that all share
/// the same `line_number`; continuation rows carry `wrap_start_col` equal
/// to the 0-based character offset of the first char shown in that row
/// (so cursor and click math can add it back to get a logical column).
#[derive(Debug, Clone)]
pub struct RenderLine {
    pub line_number: usize,
    pub wrap_start_col: usize,
    pub tokens: Vec<RenderToken>,
}

/// A token within a rendered line.
#[derive(Debug, Clone)]
pub struct RenderToken {
    pub text: String,
    pub color: [u8; 4],
}

/// A selection range for rendering.
#[derive(Debug, Clone, Copy)]
pub struct SelectionRange {
    pub line1: usize,
    pub col1: usize,
    pub line2: usize,
    pub col2: usize,
}

impl DocView {
    /// Draw a document natively. `lines` contains pre-tokenized lines for the
    /// visible range. `selections` contains all active selection ranges.
    /// Draw a document natively.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_native(
        &self,
        ctx: &mut dyn DrawContext,
        style: &crate::editor::style_ctx::StyleContext,
        lines: &[RenderLine],
        selections: &[SelectionRange],
        cursor_line: usize,
        cursor_col: usize,
        cursor_visible: bool,
        git_changes: &std::collections::HashMap<usize, crate::editor::git::LineChange>,
        extra_cursors: &[(usize, usize)],
    ) {
        // Background
        ctx.draw_rect(
            self.rect.x,
            self.rect.y,
            self.rect.w,
            self.rect.h,
            style.background.to_array(),
        );

        let line_h = style.code_font_height * 1.2; // line_height multiplier
        let gutter_w = self.gutter_width;
        let text_x = self.rect.x + gutter_w;
        let text_w = (self.rect.w - gutter_w).max(0.0);

        ctx.set_clip_rect(self.rect.x, self.rect.y, self.rect.w, self.rect.h);

        // `lines[0]` is the first visible row built by `build_render_lines`,
        // which starts at line `first = floor(scroll_y / line_h) + 1` rather
        // than line 1. The local index `i` is therefore not the absolute
        // visual row — it's the offset from `lines[0]`. Compute the y offset
        // for `lines[0]` from its absolute line number so every subsequent
        // row sits at its correct absolute position.
        let first_visual_row = lines
            .first()
            .map(|l| l.line_number.saturating_sub(1) as f64 * line_h)
            .unwrap_or(0.0);

        // Pass 1: gutter-side content drawn under the full clip. Line
        // highlights span the whole row (gutter + text), then line numbers,
        // fold markers, and git markers paint inside the gutter. Gutter
        // decorations only appear on the first visual row of each logical
        // line (wrap_start_col == 0) so they don't repeat on continuation
        // rows when wrapping is enabled.
        for (i, line) in lines.iter().enumerate() {
            let y = self.rect.y + first_visual_row + (i as f64 * line_h) - self.scroll_y;
            if y + line_h < self.rect.y || y > self.rect.y + self.rect.h {
                continue;
            }

            let on_cursor_line = line.line_number == cursor_line
                || extra_cursors.iter().any(|(cl, _)| *cl == line.line_number);
            if on_cursor_line {
                ctx.draw_rect(
                    self.rect.x,
                    y,
                    self.rect.w,
                    line_h,
                    style.line_highlight.to_array(),
                );
            }

            let is_first_row = line.wrap_start_col == 0;
            let text_y = y + (line_h - style.code_font_height) / 2.0;

            if is_first_row {
                // Line number
                let ln_str = line.line_number.to_string();
                let ln_w = ctx.font_width(style.code_font, &ln_str);
                let ln_x = self.rect.x + gutter_w - ln_w - style.padding_x;
                let ln_color = if line.line_number == cursor_line {
                    style.line_number2.to_array()
                } else {
                    style.line_number.to_array()
                };
                ctx.draw_text(style.code_font, &ln_str, ln_x, text_y, ln_color);

                // Fold indicator in gutter
                if self.folds.iter().any(|(s, _)| *s == line.line_number) {
                    let fold_x = self.rect.x + 4.0;
                    ctx.draw_text(style.code_font, ">", fold_x, text_y, style.dim.to_array());
                }

                // Bookmark marker
                if self.bookmarks.contains(&line.line_number) {
                    let bm_x = self.rect.x + 2.0;
                    let bm_y = y + line_h * 0.3;
                    let bm_size = line_h * 0.4;
                    ctx.draw_rect(bm_x, bm_y, bm_size, bm_size, style.accent.to_array());
                }

                // Git gutter marker
                if let Some(change) = git_changes.get(&line.line_number) {
                    use crate::editor::git::LineChange;
                    let marker_w = 3.0;
                    let marker_color = match change {
                        LineChange::Added => style.good.to_array(),
                        LineChange::Modified => style.warn.to_array(),
                        LineChange::Deleted => style.error.to_array(),
                    };
                    ctx.draw_rect(self.rect.x, y, marker_w, line_h, marker_color);
                }
            }
        }

        // Switch to a tighter clip for the text area so horizontally-scrolled
        // content cannot bleed left into the gutter and overlap line numbers.
        ctx.set_clip_rect(text_x, self.rect.y, text_w, self.rect.h);

        // Pass 2: text-area content. Indent guides, selection highlights,
        // tokens, whitespace markers, the column-80 guide, and cursors all
        // use scroll_x and must be clipped to the text area. With wrap on,
        // each `RenderLine` is one visual row: selection and cursor math
        // adjust for `wrap_start_col` so the coordinates map back to the
        // underlying logical line columns.
        for (i, line) in lines.iter().enumerate() {
            let y = self.rect.y + first_visual_row + (i as f64 * line_h) - self.scroll_y;
            if y + line_h < self.rect.y || y > self.rect.y + self.rect.h {
                continue;
            }
            let text_y = y + (line_h - style.code_font_height) / 2.0;

            let full_text: String = line.tokens.iter().map(|t| t.text.as_str()).collect();
            let row_char_count = full_text.chars().count();
            let row_start = line.wrap_start_col; // 0-based char offset in logical line
            let row_end = row_start + row_char_count; // exclusive

            // Indent guides: only on the first visual row of a line (leading
            // whitespace only appears there).
            if line.wrap_start_col == 0 {
                let indent_size = self.indent_size.max(1);
                let leading: usize = full_text
                    .chars()
                    .take_while(|c| c.is_ascii_whitespace() && *c != '\n')
                    .map(|c| if c == '\t' { indent_size } else { 1 })
                    .sum();
                let levels = if leading > 0 && indent_size > 0 {
                    leading / indent_size
                } else {
                    0
                };
                if levels > 0 {
                    let space_w = ctx.font_width(style.code_font, " ");
                    let step = space_w * indent_size as f64;
                    let guide_color = style.guide_color();
                    for g in 0..levels {
                        let gx = text_x + style.padding_x - self.scroll_x + step * g as f64;
                        ctx.draw_rect(gx, y, 1.0, line_h, guide_color);
                    }
                }
            }

            // Selection highlight (drawn before text so text is readable on top).
            for sel in selections {
                let ln = line.line_number;
                if ln < sel.line1 || ln > sel.line2 {
                    continue;
                }
                // Logical-line selection columns (1-based inclusive of start,
                // exclusive of end).
                let line_start_col = if ln == sel.line1 { sel.col1 } else { 1 };
                let line_end_col = if ln == sel.line2 {
                    sel.col2
                } else {
                    usize::MAX
                };
                // Clip to this visual row's [row_start+1, row_end+1) range.
                let row_start_col = row_start + 1;
                let row_end_col = row_end + 1;
                let clipped_start = line_start_col.max(row_start_col);
                let clipped_end = line_end_col.min(row_end_col);
                if clipped_start >= clipped_end {
                    continue;
                }
                // Convert to 0-based offsets within the row's text.
                let start_in_row = clipped_start - row_start_col;
                let end_in_row = clipped_end - row_start_col;
                let sel_x = text_x + style.padding_x - self.scroll_x
                    + ctx.font_width(
                        style.code_font,
                        &full_text.chars().take(start_in_row).collect::<String>(),
                    );
                let sel_end_x = text_x + style.padding_x - self.scroll_x
                    + ctx.font_width(
                        style.code_font,
                        &full_text.chars().take(end_in_row).collect::<String>(),
                    );
                let sel_w = (sel_end_x - sel_x).max(0.0);
                ctx.draw_rect(sel_x, y, sel_w, line_h, style.selection.to_array());
            }

            // Tokens
            let mut tx = text_x + style.padding_x - self.scroll_x;
            for token in &line.tokens {
                let adv = ctx.draw_text(style.code_font, &token.text, tx, text_y, token.color);
                tx = adv;
            }

            // Whitespace markers
            if self.show_whitespace {
                let ws_color = style.guide_color();
                let space_w = ctx.font_width(style.code_font, " ");
                let mut wx = text_x + style.padding_x - self.scroll_x;
                for ch in full_text.chars() {
                    match ch {
                        ' ' => {
                            let dot_y = text_y + style.code_font_height / 2.0 - 1.0;
                            ctx.draw_rect(wx + space_w / 2.0 - 1.0, dot_y, 2.0, 2.0, ws_color);
                            wx += space_w;
                        }
                        '\t' => {
                            let tab_w = space_w * self.indent_size as f64;
                            ctx.draw_text(style.code_font, ">", wx, text_y, ws_color);
                            wx += tab_w;
                        }
                        '\r' => {
                            ctx.draw_text(style.code_font, "\\r", wx, text_y, ws_color);
                            wx += ctx.font_width(style.code_font, "\\r");
                        }
                        '\n' => {
                            ctx.draw_text(style.code_font, "\\n", wx, text_y, ws_color);
                        }
                        _ => {
                            let cw = ctx.font_width(style.code_font, &ch.to_string());
                            wx += cw;
                        }
                    }
                }
                // Newline marker only after the final visual row of a line.
                let is_last_row_for_line = lines
                    .get(i + 1)
                    .is_none_or(|n| n.line_number != line.line_number);
                if is_last_row_for_line {
                    ctx.draw_text(style.code_font, "\\n", wx, text_y, ws_color);
                }
            }
        }

        // Line guide at column 80 (also clipped to the text area).
        {
            let space_w = ctx.font_width(style.code_font, "n");
            let guide_x = text_x + style.padding_x - self.scroll_x + space_w * 80.0;
            if guide_x >= text_x && guide_x <= self.rect.x + self.rect.w {
                let guide_color = style.guide_color();
                ctx.draw_rect(guide_x, self.rect.y, 2.0, self.rect.h, guide_color);
            }
        }

        // Cursors (primary + extras). When wrapping is on, a single logical
        // line may appear across several `RenderLine` entries; locate the
        // specific visual row whose [wrap_start_col, wrap_start_col +
        // row_chars] range contains the cursor column. A cursor past the
        // last char of the logical line pins to the final wrap row so
        // navigating to the end of a wrapped line is always visible.
        if cursor_visible {
            let mut all_cursors = vec![(cursor_line, cursor_col)];
            for &(cl, cc) in extra_cursors {
                if cl != cursor_line || cc != cursor_col {
                    all_cursors.push((cl, cc));
                }
            }
            for &(cl, cc) in &all_cursors {
                let mut target: Option<(usize, &RenderLine, usize)> = None;
                for (i, line) in lines.iter().enumerate() {
                    if line.line_number != cl {
                        continue;
                    }
                    let row_chars: usize =
                        line.tokens.iter().map(|t| t.text.chars().count()).sum();
                    let row_start_col = line.wrap_start_col + 1;
                    let row_end_col = line.wrap_start_col + row_chars + 1;
                    // Next row for the same logical line, if any, starts at
                    // row_end_col. A cursor sitting exactly at row_end_col
                    // belongs to the start of the next wrap row rather than
                    // the end of this one.
                    let is_last_row_for_line = lines
                        .get(i + 1)
                        .is_none_or(|n| n.line_number != cl);
                    let within = if is_last_row_for_line {
                        cc >= row_start_col
                    } else {
                        cc >= row_start_col && cc < row_end_col
                    };
                    if within {
                        let within_col = cc.saturating_sub(row_start_col);
                        target = Some((i, line, within_col));
                        break;
                    }
                }
                if let Some((i, line, within_col)) = target {
                    let y = self.rect.y + first_visual_row + (i as f64 * line_h) - self.scroll_y;
                    let full_text: String =
                        line.tokens.iter().map(|t| t.text.as_str()).collect();
                    let before: String = full_text.chars().take(within_col).collect();
                    let cx = text_x + style.padding_x - self.scroll_x
                        + ctx.font_width(style.code_font, &before);
                    ctx.draw_rect(cx, y, style.caret_width, line_h, style.caret.to_array());
                }
            }
        }

        // Restore full clip so the scrollbars (which sit at the right and
        // bottom edges, partly outside the text area) render correctly.
        ctx.set_clip_rect(self.rect.x, self.rect.y, self.rect.w, self.rect.h);

        // Vertical scrollbar
        if !lines.is_empty() {
            let total_lines = lines.last().map(|l| l.line_number).unwrap_or(1);
            let total_h = total_lines as f64 * line_h;
            if total_h > self.rect.h {
                let sb_w = style.scrollbar_size;
                let sb_x = self.rect.x + self.rect.w - sb_w;
                // Track
                ctx.draw_rect(
                    sb_x,
                    self.rect.y,
                    sb_w,
                    self.rect.h,
                    style.scrollbar_track.to_array(),
                );
                // Thumb
                let ratio = self.rect.h / total_h;
                let thumb_h = (self.rect.h * ratio).max(20.0);
                let scroll_frac = self.scroll_y / (total_h - self.rect.h).max(1.0);
                let thumb_y = self.rect.y + scroll_frac * (self.rect.h - thumb_h);
                ctx.draw_rect(sb_x, thumb_y, sb_w, thumb_h, style.scrollbar.to_array());
            }
        }

        // Horizontal scrollbar — measure the widest visible rendered line; if it
        // exceeds the text area, draw a track + thumb at the bottom edge.
        if !lines.is_empty() {
            let mut max_line_w = 0.0_f64;
            for line in lines {
                let mut w = 0.0_f64;
                for token in &line.tokens {
                    w += ctx.font_width(style.code_font, &token.text);
                }
                if w > max_line_w {
                    max_line_w = w;
                }
            }
            let text_w =
                (self.rect.w - gutter_w - style.padding_x * 2.0 - style.scrollbar_size).max(0.0);
            if max_line_w > text_w && text_w > 0.0 {
                let sb_h = style.scrollbar_size;
                let sb_x = self.rect.x + gutter_w + style.padding_x;
                let sb_y = self.rect.y + self.rect.h - sb_h;
                let track_w = text_w;
                ctx.draw_rect(sb_x, sb_y, track_w, sb_h, style.scrollbar_track.to_array());
                let ratio = (text_w / max_line_w).clamp(0.0, 1.0);
                let thumb_w = (track_w * ratio).max(20.0);
                let scroll_frac = self.scroll_x / (max_line_w - text_w).max(1.0);
                let thumb_x = sb_x + scroll_frac * (track_w - thumb_w);
                ctx.draw_rect(thumb_x, sb_y, thumb_w, sb_h, style.scrollbar.to_array());
            }
        }
    }
}

impl View for DocView {
    fn name(&self) -> &str {
        "Document"
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
    fn doc_view_defaults() {
        let v = DocView::new();
        assert_eq!(v.name(), "Document");
        assert!(v.focusable());
        assert!(v.buffer_id.is_none());
    }
}
