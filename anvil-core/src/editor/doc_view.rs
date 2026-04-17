use crate::editor::buffer;
use crate::editor::event::{EditorEvent, EventResult};
use crate::editor::lsp_client::InlayHint;
use crate::editor::style_ctx::StyleContext;
use crate::editor::tokenizer::{self, CompiledSyntax};
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

// ── Syntax color lookup and render-line builder ─────────────────────────
//
// These were in `main_loop.rs` purely for historical reasons; they are
// methods of (or helpers for) the doc view and belong next to
// `DocView::draw_native`.

thread_local! {
    pub(crate) static SYNTAX_COLORS: std::cell::RefCell<std::collections::HashMap<String, [u8; 4]>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Get syntax color from loaded theme, with fallback.
pub(crate) fn syntax_color(token_type: &str, style: &StyleContext) -> [u8; 4] {
    SYNTAX_COLORS.with(|s| {
        let colors = s.borrow();
        if let Some(c) = colors.get(token_type) {
            return *c;
        }
        // Markdown emphasis token types fall back to keyword2.
        if token_type.starts_with("markdown_") {
            if let Some(c) = colors.get("keyword2") {
                return *c;
            }
        }
        // Fallback: check "normal" key for symbol/operator.
        if let Some(c) = colors.get("normal") {
            if token_type == "symbol" || token_type == "operator" {
                return *c;
            }
        }
        style.text.to_array()
    })
}

/// Classify a word as a syntax token type based on common keywords.
pub(crate) fn classify_word(word: &str, ext: &str) -> &'static str {
    match ext {
        "rs" => match word {
            "fn" | "let" | "mut" | "pub" | "use" | "mod" | "struct" | "enum" | "impl" | "trait"
            | "for" | "while" | "loop" | "if" | "else" | "match" | "return" | "break"
            | "continue" | "where" | "type" | "const" | "static" | "ref" | "self" | "Self"
            | "super" | "crate" | "as" | "in" | "move" | "async" | "await" | "unsafe"
            | "extern" | "dyn" | "true" | "false" => "keyword",
            "bool" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32"
            | "i64" | "i128" | "isize" | "f32" | "f64" | "str" | "String" | "Option" | "Result"
            | "Vec" | "Box" | "Arc" | "Mutex" | "HashMap" | "Ok" | "Err" | "Some" | "None" => {
                "keyword2"
            }
            _ => "normal",
        },
        "lua" => match word {
            "local" | "function" | "end" | "if" | "then" | "else" | "elseif" | "for" | "while"
            | "do" | "repeat" | "until" | "return" | "break" | "in" | "and" | "or" | "not"
            | "true" | "false" | "nil" => "keyword",
            _ => "normal",
        },
        "py" => match word {
            "def" | "class" | "if" | "elif" | "else" | "for" | "while" | "return" | "import"
            | "from" | "as" | "try" | "except" | "finally" | "with" | "yield" | "lambda"
            | "and" | "or" | "not" | "in" | "is" | "True" | "False" | "None" | "pass" | "break"
            | "continue" | "raise" | "global" | "nonlocal" | "async" | "await" => "keyword",
            _ => "normal",
        },
        "js" | "ts" | "jsx" | "tsx" => match word {
            "function" | "var" | "let" | "const" | "if" | "else" | "for" | "while" | "do"
            | "switch" | "case" | "break" | "continue" | "return" | "new" | "delete" | "typeof"
            | "instanceof" | "class" | "extends" | "import" | "export" | "default" | "from"
            | "try" | "catch" | "finally" | "throw" | "async" | "await" | "yield" | "true"
            | "false" | "null" | "undefined" | "this" | "super" => "keyword",
            _ => "normal",
        },
        "c" | "h" | "cpp" | "hpp" | "cc" => match word {
            "if" | "else" | "for" | "while" | "do" | "switch" | "case" | "break" | "continue"
            | "return" | "struct" | "enum" | "union" | "typedef" | "static" | "const"
            | "extern" | "void" | "int" | "char" | "float" | "double" | "long" | "short"
            | "unsigned" | "signed" | "sizeof" | "NULL" | "true" | "false" | "class" | "public"
            | "private" | "protected" | "virtual" | "override" | "template" | "typename"
            | "namespace" | "using" | "new" | "delete" | "throw" | "try" | "catch" | "#include"
            | "#define" => "keyword",
            _ => "normal",
        },
        "toml" => match word {
            "true" | "false" => "keyword",
            _ => "normal",
        },
        _ => "normal",
    }
}

/// Tokenize a line into colored tokens using simple keyword + string/comment detection.
pub(crate) fn simple_tokenize(line: &str, ext: &str, style: &StyleContext) -> Vec<RenderToken> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    let mut current = String::new();
    let mut in_string: Option<char> = None;
    let mut in_line_comment = false;

    while let Some(&ch) = chars.peek() {
        if in_line_comment {
            current.push(ch);
            chars.next();
            continue;
        }

        if let Some(quote) = in_string {
            current.push(ch);
            chars.next();
            if ch == quote {
                tokens.push(RenderToken {
                    text: current.clone(),
                    color: syntax_color("string", style),
                });
                current.clear();
                in_string = None;
            }
            continue;
        }

        // Check for line comments.
        if ch == '/' {
            let mut peek = chars.clone();
            peek.next();
            if peek.peek() == Some(&'/') {
                if !current.is_empty() {
                    let tt = classify_word(&current, ext);
                    tokens.push(RenderToken {
                        text: current.clone(),
                        color: syntax_color(tt, style),
                    });
                    current.clear();
                }
                in_line_comment = true;
                current.push(ch);
                chars.next();
                continue;
            }
        }
        if ch == '#'
            && (ext == "py" || ext == "toml" || ext == "sh" || ext == "yml" || ext == "yaml")
        {
            if !current.is_empty() {
                let tt = classify_word(&current, ext);
                tokens.push(RenderToken {
                    text: current.clone(),
                    color: syntax_color(tt, style),
                });
                current.clear();
            }
            in_line_comment = true;
            current.push(ch);
            chars.next();
            continue;
        }
        if ch == '-' && ext == "lua" {
            let mut peek = chars.clone();
            peek.next();
            if peek.peek() == Some(&'-') {
                if !current.is_empty() {
                    let tt = classify_word(&current, ext);
                    tokens.push(RenderToken {
                        text: current.clone(),
                        color: syntax_color(tt, style),
                    });
                    current.clear();
                }
                in_line_comment = true;
                current.push(ch);
                chars.next();
                continue;
            }
        }

        // Strings.
        if ch == '"' || ch == '\'' {
            if !current.is_empty() {
                let tt = classify_word(&current, ext);
                tokens.push(RenderToken {
                    text: current.clone(),
                    color: syntax_color(tt, style),
                });
                current.clear();
            }
            in_string = Some(ch);
            current.push(ch);
            chars.next();
            continue;
        }

        // Numbers.
        if ch.is_ascii_digit() && current.is_empty() {
            let mut num = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == 'x' || c == 'b' {
                    num.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(RenderToken {
                text: num,
                color: syntax_color("number", style),
            });
            continue;
        }

        // Word boundary.
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
            chars.next();
        } else {
            if !current.is_empty() {
                let tt = classify_word(&current, ext);
                tokens.push(RenderToken {
                    text: current.clone(),
                    color: syntax_color(tt, style),
                });
                current.clear();
            }
            tokens.push(RenderToken {
                text: ch.to_string(),
                color: syntax_color("symbol", style),
            });
            chars.next();
        }
    }

    // Flush remaining.
    if !current.is_empty() {
        let color = if in_line_comment {
            syntax_color("comment", style)
        } else if in_string.is_some() {
            syntax_color("string", style)
        } else {
            let tt = classify_word(&current, ext);
            syntax_color(tt, style)
        };
        tokens.push(RenderToken {
            text: current,
            color,
        });
    }

    tokens
}

#[cfg(feature = "sdl")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn click_to_doc_pos(
    dv: &DocView,
    buf_id: u64,
    cached: &[RenderLine],
    x: f64,
    y: f64,
    text_x_start: f64,
    line_h: f64,
    style: &StyleContext,
    draw_ctx: &mut crate::editor::draw_context::NativeDrawContext,
) -> (usize, usize) {
    use crate::editor::view::DrawContext as _;
    let rect = dv.rect();
    // Clicked visual row relative to the first rendered row.
    let first_logical = cached
        .first()
        .map(|l| l.line_number as f64)
        .unwrap_or(1.0);
    let first_row_y = rect.y + (first_logical - 1.0) * line_h - dv.scroll_y;
    let row_f = (y - first_row_y) / line_h;
    let clicked_idx = if row_f < 0.0 { 0 } else { row_f.floor() as usize };

    // Resolve the logical line + wrap offset from the cached render lines.
    if let Some(line) = cached.get(clicked_idx) {
        let line_idx = line.line_number;
        let wrap_offset = line.wrap_start_col;
        let col_within_row = if x > text_x_start {
            let full_text: String = line.tokens.iter().map(|t| t.text.as_str()).collect();
            let mut col = 0usize;
            let mut cx = text_x_start;
            for ch in full_text.chars() {
                let cw = draw_ctx.font_width(style.code_font, &ch.to_string());
                if cx + cw / 2.0 > x {
                    break;
                }
                cx += cw;
                col += 1;
            }
            col
        } else {
            0
        };
        return (line_idx, wrap_offset + col_within_row + 1);
    }

    // Cache miss: fall back to the naive mapping (no wrap) so clicks below
    // the rendered area still land somewhere sensible.
    let click_line = ((y - rect.y + dv.scroll_y) / line_h).floor() as usize + 1;
    let click_col = if x > text_x_start {
        buffer::with_buffer(buf_id, |b| {
            let line_idx = click_line.min(b.lines.len()).max(1);
            let text = b.lines[line_idx - 1].trim_end_matches('\n');
            let mut col = 1usize;
            let mut cx = text_x_start;
            for ch in text.chars() {
                let cw = draw_ctx.font_width(style.code_font, &ch.to_string());
                if cx + cw / 2.0 > x {
                    break;
                }
                cx += cw;
                col += 1;
            }
            Ok(col)
        })
        .unwrap_or(1)
    } else {
        1
    };
    (click_line, click_col)
}

/// Build render lines from buffer for the visible range, with syntax highlighting.
#[cfg(feature = "sdl")]
pub(crate) fn build_render_lines(
    buf_id: u64,
    dv: &DocView,
    style: &StyleContext,
    file_ext: &str,
    compiled: Option<&CompiledSyntax>,
    wrap_width: Option<f64>,
    inlay_hints: &[InlayHint],
) -> Vec<RenderLine> {
    let line_h = style.code_font_height * 1.2;
    let visible_lines = ((dv.rect().h / line_h).ceil() as usize).max(1);
    let hint_color = SYNTAX_COLORS.with(|s| {
        s.borrow()
            .get("comment")
            .copied()
            .unwrap_or(style.dim.to_array())
    });

    buffer::with_buffer(buf_id, |b| {
        let first = ((dv.scroll_y / line_h).floor() as usize).max(0) + 1;
        let last = (first + visible_lines + 1).min(b.lines.len());
        let mut render = Vec::new();
        let mut i = first;
        while i <= last && i <= b.lines.len() {
            // Skip folded lines.
            let mut folded = false;
            for (fs, fe) in &dv.folds {
                if i > *fs && i <= *fe {
                    folded = true;
                    break;
                }
            }
            if folded {
                i += 1;
                continue;
            }
            let raw_line = &b.lines[i - 1];
            let text = raw_line.trim_end_matches('\n');
            let mut tokens: Vec<RenderToken> = if let Some(syntax) = compiled {
                let toks = tokenizer::tokenize_line(syntax, raw_line);
                toks.iter()
                    .map(|t| {
                        let trimmed = t.text.trim_end_matches('\n').to_string();
                        // Rust attributes (#[...]) should render as normal/white, not keyword blue.
                        let tt = if t.token_type == "keyword" && trimmed.starts_with("#[") {
                            "attribute"
                        } else {
                            &t.token_type
                        };
                        RenderToken {
                            text: trimmed,
                            color: syntax_color(tt, style),
                        }
                    })
                    .collect()
            } else {
                simple_tokenize(text, file_ext, style)
            };

            // Bracket pair colorization: color ()[]{}  by nesting depth.
            let bracket_colors: [Option<[u8; 4]>; 3] = SYNTAX_COLORS.with(|s| {
                let c = s.borrow();
                [
                    c.get("bracket1").copied(),
                    c.get("bracket2").copied(),
                    c.get("bracket3").copied(),
                ]
            });
            if bracket_colors[0].is_some() {
                let mut depth: usize = 0;
                let mut new_tokens = Vec::with_capacity(tokens.len());
                for tok in tokens {
                    let has_bracket = tok.text.contains(['(', ')', '[', ']', '{', '}']);
                    if !has_bracket {
                        new_tokens.push(tok);
                        continue;
                    }
                    let base = tok.color;
                    let mut run = String::new();
                    for ch in tok.text.chars() {
                        if matches!(ch, '(' | '[' | '{') {
                            if !run.is_empty() {
                                new_tokens.push(RenderToken {
                                    text: std::mem::take(&mut run),
                                    color: base,
                                });
                            }
                            let bc = bracket_colors[depth % 3].unwrap_or(base);
                            new_tokens.push(RenderToken {
                                text: ch.to_string(),
                                color: bc,
                            });
                            depth += 1;
                        } else if matches!(ch, ')' | ']' | '}') {
                            if !run.is_empty() {
                                new_tokens.push(RenderToken {
                                    text: std::mem::take(&mut run),
                                    color: base,
                                });
                            }
                            depth = depth.saturating_sub(1);
                            let bc = bracket_colors[depth % 3].unwrap_or(base);
                            new_tokens.push(RenderToken {
                                text: ch.to_string(),
                                color: bc,
                            });
                        } else {
                            run.push(ch);
                        }
                    }
                    if !run.is_empty() {
                        new_tokens.push(RenderToken {
                            text: run,
                            color: base,
                        });
                    }
                }
                tokens = new_tokens;
            }

            // Inject inlay hints inline between tokens.
            // Hints use byte_col (0-based byte offset in the line text).
            // Split tokens at each inlay-hint byte position.
            let mut line_hints: Vec<(usize, &str)> = inlay_hints
                .iter()
                .filter(|h| h.line == i - 1)
                .map(|h| {
                    // Convert 0-based char col to 1-based byte col (matching legacy).
                    let byte_col = text
                        .char_indices()
                        .nth(h.col)
                        .map(|(bi, _)| bi + 1)
                        .unwrap_or(text.len() + 1);
                    (byte_col, h.label.as_str())
                })
                .collect();
            line_hints.sort_by_key(|h| h.0);

            if !line_hints.is_empty() {
                let mut new_tokens = Vec::new();
                let mut byte_col = 1usize; // 1-based
                let mut hint_idx = 0;
                for tok in &tokens {
                    let tok_bytes = tok.text.len();
                    let token_end = byte_col + tok_bytes;
                    if hint_idx < line_hints.len() && line_hints[hint_idx].0 < token_end {
                        let mut remaining = tok.text.as_str();
                        let mut cur_col = byte_col;
                        while hint_idx < line_hints.len() && line_hints[hint_idx].0 < token_end {
                            let (hcol, display) = line_hints[hint_idx];
                            if hcol > cur_col && !remaining.is_empty() {
                                let split_at = (hcol - cur_col).min(remaining.len());
                                let (before, after) = remaining.split_at(split_at);
                                new_tokens.push(RenderToken {
                                    text: before.to_string(),
                                    color: tok.color,
                                });
                                cur_col += split_at;
                                remaining = after;
                            }
                            new_tokens.push(RenderToken {
                                text: display.to_string(),
                                color: hint_color,
                            });
                            hint_idx += 1;
                        }
                        if !remaining.is_empty() {
                            new_tokens.push(RenderToken {
                                text: remaining.to_string(),
                                color: tok.color,
                            });
                        }
                    } else {
                        new_tokens.push(tok.clone());
                    }
                    byte_col = token_end;
                }
                while hint_idx < line_hints.len() {
                    new_tokens.push(RenderToken {
                        text: line_hints[hint_idx].1.to_string(),
                        color: hint_color,
                    });
                    hint_idx += 1;
                }
                tokens = new_tokens;
            }

            // If wrapping enabled, split tokens across multiple render lines.
            if let Some(max_w) = wrap_width {
                // Use the measured monospace char width when we have one
                // (populated each frame from the draw context), falling back
                // to a height-based estimate only before the first render.
                let char_w = if dv.code_char_w > 0.0 {
                    dv.code_char_w
                } else {
                    style.code_font_height * 0.6
                };
                let max_chars = (max_w / char_w).floor() as usize;
                if max_chars > 0 && text.chars().count() > max_chars {
                    // Word-aware wrap: prefer breaking at whitespace when a
                    // space is available within the current segment, and
                    // fall back to a hard cut at max_chars otherwise. Each
                    // emitted row shares the logical line_number so cursor
                    // and click math can locate the line; `wrap_start_col`
                    // records the 0-based char offset of the row's first
                    // char in the full line, so `full_col = wrap_start_col
                    // + col_within_row`. Original tokens are sliced at the
                    // wrap boundaries so syntax colors survive the wrap
                    // (otherwise highlighted constructs like markdown image
                    // links collapse to the default text color).
                    let token_chars: Vec<Vec<char>> =
                        tokens.iter().map(|t| t.text.chars().collect()).collect();
                    let total: usize = token_chars.iter().map(|c| c.len()).sum();
                    // Cumulative char offset at the start of each token; last
                    // entry is `total` and acts as a sentinel.
                    let mut token_offsets: Vec<usize> = Vec::with_capacity(tokens.len() + 1);
                    token_offsets.push(0);
                    for cs in &token_chars {
                        token_offsets.push(token_offsets.last().unwrap() + cs.len());
                    }

                    // Flat char view for wrap-break search.
                    let chars: Vec<char> =
                        token_chars.iter().flat_map(|c| c.iter().copied()).collect();
                    let mut offset = 0;
                    while offset < total {
                        let hard_end = (offset + max_chars).min(total);
                        let mut end = hard_end;
                        if hard_end < total {
                            let mut j = hard_end;
                            while j > offset + 1 {
                                j -= 1;
                                if chars[j - 1].is_whitespace() {
                                    end = j;
                                    break;
                                }
                            }
                        }
                        // Slice each original token that overlaps [offset,
                        // end) into the row's token list, preserving colors.
                        let mut row_tokens: Vec<RenderToken> =
                            Vec::with_capacity(tokens.len());
                        for (tidx, tok) in tokens.iter().enumerate() {
                            let tok_start = token_offsets[tidx];
                            let tok_end = token_offsets[tidx + 1];
                            if tok_end <= offset || tok_start >= end {
                                continue;
                            }
                            let clip_start = offset.max(tok_start);
                            let clip_end = end.min(tok_end);
                            let local_start = clip_start - tok_start;
                            let local_end = clip_end - tok_start;
                            let chunk: String =
                                token_chars[tidx][local_start..local_end].iter().collect();
                            if !chunk.is_empty() {
                                row_tokens.push(RenderToken {
                                    text: chunk,
                                    color: tok.color,
                                });
                            }
                        }
                        render.push(RenderLine {
                            line_number: i,
                            wrap_start_col: offset,
                            tokens: row_tokens,
                        });
                        offset = end;
                    }
                } else {
                    render.push(RenderLine {
                        line_number: i,
                        wrap_start_col: 0,
                        tokens,
                    });
                }
            } else {
                render.push(RenderLine {
                    line_number: i,
                    wrap_start_col: 0,
                    tokens,
                });
            }
            i += 1;
        }
        Ok(render)
    })
    .unwrap_or_default()
}

/// Draw the breadcrumb strip above the document area. When the path is
/// wider than `bar_w`, leading segments are dropped one at a time and
/// replaced with `… > `; the filename (the last segment) is always kept
/// and left-ellipsis-truncated if it overflows on its own.
#[cfg(feature = "sdl")]
pub(crate) fn draw_breadcrumb(
    ctx: &mut crate::editor::draw_context::NativeDrawContext,
    path: &str,
    bar_x: f64,
    bar_y: f64,
    bar_w: f64,
    bar_h: f64,
    style: &StyleContext,
) {
    use crate::editor::view::DrawContext as _;

    ctx.draw_rect(bar_x, bar_y, bar_w, bar_h, style.background3.to_array());

    let segments: Vec<&str> = path
        .split(|c: char| c == '/' || c == '\\')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        return;
    }

    let bx_start = bar_x + style.padding_x;
    let by = bar_y + style.padding_y * 0.25;
    let available = (bar_w - style.padding_x * 2.0).max(0.0);
    let arrow = " > ";
    let arrow_w = ctx.font_width(style.font, arrow);
    let ellipsis_prefix = "… > ";
    let ellipsis_prefix_w = ctx.font_width(style.font, ellipsis_prefix);

    // Start with the last segment (possibly shrunk) and prepend ancestors
    // while they fit.
    let last = *segments.last().unwrap();
    let last_w = ctx.font_width(style.font, last);
    let (mut displayed, mut truncated_first) = if last_w <= available {
        (vec![last.to_string()], false)
    } else {
        (
            vec![crate::editor::cmdview::truncate_left_to_width(
                last,
                available,
                style.font,
                ctx,
            )],
            false,
        )
    };
    let mut used_w = ctx.font_width(style.font, &displayed[0]);
    let mut first_kept_idx = segments.len() - 1;
    for i in (0..segments.len() - 1).rev() {
        let seg = segments[i];
        let seg_w = ctx.font_width(style.font, seg);
        let budget =
            available - used_w - arrow_w - if i == 0 { 0.0 } else { ellipsis_prefix_w };
        if seg_w > budget {
            truncated_first = true;
            break;
        }
        used_w += seg_w + arrow_w;
        first_kept_idx = i;
        displayed.insert(0, seg.to_string());
    }

    let mut bx = bx_start;
    if truncated_first && first_kept_idx > 0 {
        ctx.draw_text(style.font, ellipsis_prefix, bx, by, style.dim.to_array());
        bx += ellipsis_prefix_w;
    }
    for (i, seg) in displayed.iter().enumerate() {
        let is_last = i == displayed.len() - 1;
        let color = if is_last {
            style.text.to_array()
        } else {
            style.dim.to_array()
        };
        ctx.draw_text(style.font, seg, bx, by, color);
        bx += ctx.font_width(style.font, seg);
        if !is_last {
            ctx.draw_text(style.font, arrow, bx, by, style.dim.to_array());
            bx += arrow_w;
        }
    }
}

/// Format the window title from the active document's `name`. Empty name
/// collapses to just `app_name`; anything else becomes `"name - app_name"`.
pub(crate) fn format_window_title(doc_name: &str, app_name: &str) -> String {
    if doc_name.is_empty() {
        app_name.to_string()
    } else {
        format!("{doc_name} - {app_name}")
    }
}
