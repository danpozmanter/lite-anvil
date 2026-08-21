//! Rendered markdown preview pane.
//!
//! Design principles learned the hard way:
//!
//! 1. **Measurement and drawing must use the same font metrics.** Every
//!    wrap-height calculation takes an explicit `base_font` / `base_lh`
//!    pair that the matching draw path also uses. Divergence between the
//!    two causes text to overlap.
//! 2. **There is no resized heading font slot.** `style.big_font` is 46pt
//!    by default (splash-screen logo), not a usable heading font. All
//!    headings use `style.font` and distinguish themselves via whitespace
//!    and divider rules.
//! 3. **Split-pane click routing is bounds-checked on both sides.** This
//!    module exposes `rect: Rect` on the state so the main loop can
//!    decide which pane a click belongs to.

use crate::editor::markdown::{Block, ListItem, Span};
use crate::editor::style_ctx::StyleContext;
use crate::editor::types::Rect;
use crate::editor::view::DrawContext;

/// Screen region linked to a clickable URL.
#[derive(Debug, Clone)]
pub struct LinkRegion {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub href: String,
}

/// Screen region linked to a task-list checkbox. `source_start` is the byte
/// offset of the list-item start in the source document — the caller uses
/// it to find and flip the `[ ]` / `[x]` marker in the buffer.
#[derive(Debug, Clone)]
pub struct CheckboxRegion {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub source_start: usize,
    pub checked: bool,
}

/// Per-document preview state. Lives on `OpenDoc` and is only populated
/// after the user toggles preview on.
#[derive(Debug, Default)]
pub struct MarkdownPreviewState {
    pub enabled: bool,
    pub blocks: Vec<Block>,
    pub layout: Vec<LayoutEntry>,
    pub content_height: f64,
    pub content_width: f64,
    pub scroll_y: f64,
    pub target_scroll_y: f64,
    pub scroll_x: f64,
    pub cached_change_id: i64,
    /// Most recent buffer version observed while waiting for the user to
    /// pause typing before reparsing the full document.
    pub pending_change_id: i64,
    pub reparse_at: Option<std::time::Instant>,
    pub cached_width: f64,
    pub link_regions: Vec<LinkRegion>,
    pub checkbox_regions: Vec<CheckboxRegion>,
    /// Rectangle this preview occupies. Refreshed each frame by the layout
    /// pass so hit-tests in the main loop know which pane a click is in.
    pub rect: Rect,
    /// Pixel-snapped rectangle the content is actually drawn into: `rect`
    /// minus the split divider. Written by `draw` so scrollbar hit-tests and
    /// scroll clamping use the same geometry the last frame drew.
    pub content_rect: Rect,
    /// Whether the pane holds keyboard focus. A focused preview scrolls with
    /// the navigation keys and swallows text input; it never edits.
    pub focused: bool,
    /// Parallel to `blocks`: pre-tokenized code-block lines (one entry per
    /// line) when the block's fence lang resolves to a bundled syntax; None
    /// otherwise. Populated by the main loop after each reparse so draws
    /// don't pay the tokenize cost every frame.
    pub code_tokens: Vec<Option<Vec<Vec<crate::editor::tokenizer::Token>>>>,
    /// Where the current selection was pressed, in content coordinates -
    /// the pane's top-left corner plus the scroll offset - so the selection
    /// stays anchored to the text while the pane scrolls.
    pub sel_anchor: Option<(f64, f64)>,
    /// Where the current selection currently reaches, in content
    /// coordinates. Equal to `sel_anchor` until the pointer moves.
    pub sel_head: Option<(f64, f64)>,
}

impl MarkdownPreviewState {
    /// Heap bytes this preview retains. Zero while preview is off, since
    /// nothing is parsed until the user opens the pane.
    pub fn retained_bytes(&self) -> u64 {
        let layout = (self.layout.capacity() * std::mem::size_of::<LayoutEntry>()) as u64;
        let code: u64 = self
            .code_tokens
            .iter()
            .flatten()
            .flatten()
            .flatten()
            .map(|token| {
                (token.text.capacity() + std::mem::size_of::<crate::editor::tokenizer::Token>())
                    as u64
            })
            .sum();
        layout + code
    }
}

/// A rendered document walked by the draw pass, plus everything that pass
/// hands back: the interactive regions, the selection highlight it paints,
/// and the selected text it collects.
struct Sink<'a> {
    links: &'a mut Vec<LinkRegion>,
    checkboxes: &'a mut Vec<CheckboxRegion>,
    /// Ordered selection bounds in screen coordinates, `(x0, y0, x1, y1)`,
    /// where `(x0, y0)` reads before `(x1, y1)`.
    selection: Option<(f64, f64, f64, f64)>,
    /// Fill painted behind the selected part of each fragment.
    highlight: [u8; 4],
    /// Set when the pass exists to assemble [`Self::picked`] rather than to
    /// paint; the draw pass leaves it false and skips the string building.
    collecting: bool,
    /// Selected text in reading order, ready for the clipboard.
    picked: String,
    /// Separator the next fragment joins with, set by whichever block-level
    /// code opens a new line, cell, or paragraph. A fragment consumes it and
    /// falls back to the separator the inline layout used.
    pending_sep: Option<&'static str>,
    /// Right edge and line top of the previously highlighted fragment, so a
    /// run of selected words fills the gaps between them instead of painting
    /// one patch per word.
    run_end: Option<(f64, f64)>,
    /// Screen point the pass is looking for a fragment under.
    probe: Option<(f64, f64)>,
    /// Screen box of the fragment the probe landed in.
    hit: Option<(f64, f64, f64, f64)>,
}

impl Sink<'_> {
    /// Open a new line, cell, or block in the collected text.
    fn separate(&mut self, sep: &'static str) {
        self.pending_sep = Some(sep);
    }

    /// Paint the selection highlight behind a text fragment that is about to
    /// be drawn at `(x, y)`, and collect the selected part of it. `word_sep`
    /// is what joins this fragment to the previous one when no block-level
    /// separator is pending.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        ctx: &mut dyn DrawContext,
        font: u64,
        text: &str,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        word_sep: &'static str,
    ) {
        if let Some((px, py)) = self.probe
            && px >= x
            && px < x + w
            && py >= y
            && py < y + h
        {
            self.hit = Some((x, y, x + w, y + h));
        }
        let Some(sel) = self.selection else {
            return;
        };
        // A pending separator is consumed by the first SELECTED fragment
        // after it, so an unselected run of words cannot swallow the line
        // break the next selected fragment needs.
        let Some((from, to)) = fragment_selection(ctx, font, text, x, y, w, h, sel) else {
            return;
        };
        let sep = self.pending_sep.take().unwrap_or(word_sep);
        let hl_x = x + ctx.font_width(font, &text[..from]);
        let hl_end = hl_x + ctx.font_width(font, &text[from..to]);
        // Reach back to the previous highlighted fragment on this line so
        // the space between two selected words is covered too.
        let hl_left = match self.run_end {
            Some((prev_end, prev_y)) if prev_y == y && from == 0 && prev_end <= hl_x => prev_end,
            _ => hl_x,
        };
        ctx.draw_rect(hl_left, y, hl_end - hl_left, h, self.highlight);
        self.run_end = if to == text.len() {
            Some((hl_end, y))
        } else {
            None
        };
        if self.collecting {
            if !self.picked.is_empty() {
                self.picked.push_str(sep);
            }
            self.picked.push_str(&text[from..to]);
        }
    }
}

/// Byte range of `text` the selection covers, given the screen box the
/// fragment occupies. `None` when the fragment lies outside the selection.
#[allow(clippy::too_many_arguments)]
fn fragment_selection(
    ctx: &dyn DrawContext,
    font: u64,
    text: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    sel: (f64, f64, f64, f64),
) -> Option<(usize, usize)> {
    let (sx, sy, ex, ey) = sel;
    if ey < y || sy >= y + h {
        return None;
    }
    let from = if sy < y {
        0
    } else if sx >= x + w {
        text.len()
    } else {
        char_offset_at(ctx, font, text, x, sx)
    };
    let to = if ey >= y + h {
        text.len()
    } else if ex <= x {
        0
    } else {
        char_offset_at(ctx, font, text, x, ex)
    };
    if from >= to { None } else { Some((from, to)) }
}

/// Byte offset of the character boundary in `text` nearest to screen x
/// `px`, for a fragment drawn at `x` in `font`.
fn char_offset_at(ctx: &dyn DrawContext, font: u64, text: &str, x: f64, px: f64) -> usize {
    let mut best = 0usize;
    let mut best_d = (px - x).abs();
    for (i, ch) in text.char_indices() {
        let boundary = i + ch.len_utf8();
        let d = (px - (x + ctx.font_width(font, &text[..boundary]))).abs();
        if d < best_d {
            best_d = d;
            best = boundary;
        }
    }
    best
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LayoutEntry {
    pub y: f64,
    pub h: f64,
}

// ── Constants ────────────────────────────────────────────────────────────

/// Outer margin from the preview rect to the content.
const PAD: f64 = 20.0;
/// Baseline gap between blocks.
const GAP: f64 = 14.0;
/// Extra top padding before an h1 (in units of the heading's own line height).
const H1_TOP_GAP_MUL: f64 = 0.5;
/// Extra top padding before h2-h6 (in units of body line height).
const HX_TOP_GAP_MUL: f64 = 0.7;
/// Space reserved below h1/h2 for the divider rule.
const HEADING_RULE_GAP: f64 = 10.0;
/// Indent for blockquote content.
const QUOTE_INDENT: f64 = 18.0;
/// Left-bar thickness inside a blockquote.
const QUOTE_BAR_W: f64 = 3.0;
/// Bullet/checkbox gutter reserved on the left of a list item. Wide enough
/// to fit the checkbox outline plus a comfortable right-side gap before
/// the item's text column starts.
const LIST_GUTTER: f64 = 44.0;
/// Left inset inside `LIST_GUTTER` where the bullet/number/checkbox is drawn.
const LIST_MARKER_INSET: f64 = 8.0;
/// Inner cell padding in tables.
const TABLE_CELL_PAD: f64 = 10.0;
/// Link color — a readable sky-blue that stands out against the neutral
/// theme text color.
const LINK_COLOR: [u8; 4] = [88, 166, 255, 255];
/// Cap on how deep the measure/draw recursion descends through nested
/// blockquotes and lists. Content nested past this level renders nothing,
/// bounding stack use against pathologically deep documents.
const MAX_RENDER_DEPTH: usize = 64;

/// Resolve (font, line_height) for a heading level. h1/h2/h3 use the
/// dedicated scaled UI font slots loaded at startup (1.75x/1.45x/1.20x of
/// body). h4, h5, h6 share the body font. The returned `lh` is measured
/// via `ctx.font_height(...)` / the cached value on `StyleContext` so
/// `inlines_height` and `draw_inlines` agree on vertical advance — the
/// previous bug was scaling body lh by a factor that didn't match the
/// font's real height.
fn heading_metrics(ctx: &dyn DrawContext, level: u8, style: &StyleContext) -> (u64, f64) {
    let body_lh = style.font_height.max(1.0);
    match level {
        1 => {
            let h = style.h1_font_height.max(body_lh);
            let f = if style.h1_font != 0 {
                style.h1_font
            } else {
                style.font
            };
            (f, h)
        }
        2 => {
            let h = style.h2_font_height.max(body_lh);
            let f = if style.h2_font != 0 {
                style.h2_font
            } else {
                style.font
            };
            (f, h)
        }
        3 => {
            let h = style.h3_font_height.max(body_lh);
            let f = if style.h3_font != 0 {
                style.h3_font
            } else {
                style.font
            };
            (f, h)
        }
        _ => (style.font, ctx.font_height(style.font).max(body_lh)),
    }
}

/// Color for a heading level. h1-h3 use the theme accent for strong
/// hierarchy; h4-h5 use body text; h6 fades to dim.
fn heading_color(level: u8, style: &StyleContext) -> [u8; 4] {
    match level {
        1..=3 => style.accent.to_array(),
        6 => style.dim.to_array(),
        _ => style.text.to_array(),
    }
}

// ── Measurement ──────────────────────────────────────────────────────────

/// Measure the wrapped height of a span sequence at `width` pixels using
/// `base_font` / `base_lh` for non-code spans.
///
/// Invariant: every caller passes the same font/lh pair to the matching
/// draw function. This is the single source of truth for wrap metrics.
///
/// Punctuation rule: a separating space is only inserted between two words
/// when the source actually had whitespace there — either the previous
/// span ended with whitespace, the current span starts with whitespace, or
/// the two words come from the same span (split_whitespace guarantees a
/// split point means whitespace existed). This keeps `see [LICENSE](...).`
/// from rendering as `see LICENSE .`.
fn inlines_height(
    ctx: &dyn DrawContext,
    spans: &[Span],
    width: f64,
    base_font: u64,
    base_lh: f64,
    style: &StyleContext,
) -> f64 {
    if spans.is_empty() || width <= 0.0 {
        return 0.0;
    }
    let code = style.code_font;
    let mut x = 0.0;
    let mut lines = 1.0;
    let mut last = false;
    let mut ws_pending = false;

    for span in spans {
        if span.text == "\n" {
            x = 0.0;
            lines += 1.0;
            last = false;
            ws_pending = false;
            continue;
        }
        let font = if span.code { code } else { base_font };
        let sw = ctx.font_width(font, " ");
        let leads_ws = span.text.starts_with(char::is_whitespace);
        let trails_ws = span.text.ends_with(char::is_whitespace);
        let mut placed_any = false;
        for (i, word) in span.text.split_whitespace().enumerate() {
            placed_any = true;
            let ww = ctx.font_width(font, word);
            let needs_space = if i == 0 {
                last && (ws_pending || leads_ws)
            } else {
                true
            };
            if needs_space {
                if x + sw + ww > width && x > 0.0 {
                    x = 0.0;
                    lines += 1.0;
                } else {
                    x += sw;
                }
            } else if x + ww > width && x > 0.0 {
                x = 0.0;
                lines += 1.0;
            }
            x += ww;
            last = true;
        }
        if !placed_any {
            if !span.text.is_empty() {
                ws_pending = true;
            }
            continue;
        }
        ws_pending = trails_ws;
    }
    lines * base_lh
}

fn code_block_line_count(text: &str) -> usize {
    // The parser strips the trailing newline, so the split-count equals the
    // rendered line count; an empty block yields one line.
    text.split('\n').count()
}

/// Height of one block at `width` pixels. Callers add inter-block `GAP`.
/// `depth` tracks nesting through quotes/lists so recursion stops at
/// `MAX_RENDER_DEPTH`.
fn block_height(
    ctx: &dyn DrawContext,
    blk: &Block,
    width: f64,
    style: &StyleContext,
    depth: usize,
) -> f64 {
    if depth >= MAX_RENDER_DEPTH {
        return 0.0;
    }
    let lh = style.font_height;
    let clh = style.code_font_height;
    let body = style.font;
    match blk {
        Block::Rule => lh + (lh * 0.5).ceil(),
        Block::Heading { level, inlines } => {
            // Measure with the heading's actual font metrics so drawing
            // and measurement agree on vertical advance.
            let (hfont, hlh) = heading_metrics(ctx, *level, style);
            let top_gap = if *level == 1 {
                (hlh * H1_TOP_GAP_MUL).ceil()
            } else {
                (lh * HX_TOP_GAP_MUL).ceil()
            };
            let text_h = inlines_height(ctx, inlines, width, hfont, hlh, style);
            let mut h = top_gap + text_h;
            if *level <= 2 {
                h += HEADING_RULE_GAP;
            }
            h
        }
        Block::Paragraph { inlines } => inlines_height(ctx, inlines, width, body, lh, style),
        Block::Code { text, .. } => {
            let pad = (lh * 0.75).ceil();
            code_block_line_count(text) as f64 * clh + pad * 2.0
        }
        Block::Quote { blocks } => {
            let inner_w = (width - QUOTE_INDENT).max(0.0);
            let vpad = (lh * 0.6).ceil();
            let mut h = vpad;
            let mut first = true;
            for sub in blocks {
                if !first {
                    h += GAP;
                }
                h += block_height(ctx, sub, inner_w, style, depth + 1);
                first = false;
            }
            (h + vpad).max(lh)
        }
        Block::List { items, .. } => {
            let inner_w = (width - LIST_GUTTER).max(0.0);
            let item_gap = (lh * 0.5).ceil();
            let mut h = 0.0;
            let mut first = true;
            for item in items {
                if !first {
                    h += item_gap;
                }
                let ih = inlines_height(ctx, &item.spans, inner_w, body, lh, style);
                h += ih.max(lh);
                for sub in &item.blocks {
                    h += GAP;
                    h += block_height(ctx, sub, inner_w, style, depth + 1);
                }
                first = false;
            }
            h.max(lh)
        }
        Block::Table {
            alignments,
            head,
            rows,
        } => {
            let n_cols = alignments.len().max(head.len()).max(1);
            let col_w = table_col_width(ctx, head, rows, n_cols, width, body, style);
            let inner_cell_w = (col_w - TABLE_CELL_PAD * 2.0).max(0.0);
            let mut h = 1.0;
            if !head.is_empty() {
                h += table_row_height(ctx, head, inner_cell_w, body, lh, style) + 1.0;
            }
            for row in rows {
                h += table_row_height(ctx, row, inner_cell_w, body, lh, style) + 1.0;
            }
            h
        }
    }
}

/// Width a block needs to be fully visible, measured in the same content
/// coordinates as `block_height` (i.e. excluding the outer `PAD`). Wrapped
/// text reflows to any width, so this only exceeds the available width for
/// content that cannot wrap: code lines, and single words wider than their
/// column.
fn block_width(
    ctx: &dyn DrawContext,
    blk: &Block,
    width: f64,
    style: &StyleContext,
    depth: usize,
) -> f64 {
    if depth >= MAX_RENDER_DEPTH {
        return 0.0;
    }
    let body = style.font;
    match blk {
        Block::Rule => 0.0,
        Block::Heading { level, inlines } => {
            let (hfont, _) = heading_metrics(ctx, *level, style);
            inlines_min_width(ctx, inlines, hfont, style)
        }
        Block::Paragraph { inlines } => inlines_min_width(ctx, inlines, body, style),
        Block::Code { text, .. } => code_block_width(ctx, text, style),
        Block::Quote { blocks } => {
            let inner_w = (width - QUOTE_INDENT).max(0.0);
            let mut max_w = 0.0_f64;
            for sub in blocks {
                let w = block_width(ctx, sub, inner_w, style, depth + 1);
                if w > max_w {
                    max_w = w;
                }
            }
            QUOTE_INDENT + max_w
        }
        Block::List { items, .. } => {
            let inner_w = (width - LIST_GUTTER).max(0.0);
            let mut max_w = 0.0_f64;
            for item in items {
                let w = inlines_min_width(ctx, &item.spans, body, style);
                if w > max_w {
                    max_w = w;
                }
                for sub in &item.blocks {
                    let w = block_width(ctx, sub, inner_w, style, depth + 1);
                    if w > max_w {
                        max_w = w;
                    }
                }
            }
            LIST_GUTTER + max_w
        }
        Block::Table {
            alignments,
            head,
            rows,
        } => {
            let n_cols = alignments.len().max(head.len()).max(1);
            n_cols as f64 * table_col_width(ctx, head, rows, n_cols, width, body, style)
        }
    }
}

/// Width of the widest unbreakable run in a span sequence — the narrowest
/// column the text can wrap into. `block_width` uses it so a horizontal
/// scrollbar appears only for content that genuinely cannot fit, not for
/// every long paragraph.
fn inlines_min_width(
    ctx: &dyn DrawContext,
    spans: &[Span],
    base_font: u64,
    style: &StyleContext,
) -> f64 {
    let code = style.code_font;
    let mut max_w = 0.0_f64;
    for span in spans {
        let font = if span.code { code } else { base_font };
        for word in span.text.split_whitespace() {
            let w = ctx.font_width(font, word);
            if w > max_w {
                max_w = w;
            }
        }
    }
    max_w
}

/// Natural width of a code block: its widest line plus the panel padding on
/// both sides. Code lines never wrap, so this is what the block needs to be
/// read in full.
fn code_block_width(ctx: &dyn DrawContext, text: &str, style: &StyleContext) -> f64 {
    let pad = (style.font_height * 0.75).ceil();
    let text_x = pad + 3.0;
    let mut max_w = 0.0_f64;
    for line in text.split('\n') {
        let w = ctx.font_width(style.code_font, line);
        if w > max_w {
            max_w = w;
        }
    }
    text_x * 2.0 + max_w
}

/// Column width for a table laid out in `avail_w` pixels. Columns split the
/// available width evenly unless some cell holds an unbreakable run wider
/// than that share, in which case every column widens to fit it and the
/// table reaches past the pane for the horizontal scrollbar to expose.
///
/// Invariant: `block_height`, `block_width`, and `draw_table` all size
/// columns through this function, so measured row heights match the drawn
/// ones.
fn table_col_width(
    ctx: &dyn DrawContext,
    head: &[Vec<Span>],
    rows: &[Vec<Vec<Span>>],
    n_cols: usize,
    avail_w: f64,
    body: u64,
    style: &StyleContext,
) -> f64 {
    let even = (avail_w.max(0.0) / n_cols as f64).floor();
    let mut min_cell = 0.0_f64;
    for cell in head {
        let w = inlines_min_width(ctx, cell, body, style);
        if w > min_cell {
            min_cell = w;
        }
    }
    for row in rows {
        for cell in row {
            let w = inlines_min_width(ctx, cell, body, style);
            if w > min_cell {
                min_cell = w;
            }
        }
    }
    even.max((min_cell + TABLE_CELL_PAD * 2.0).ceil())
}

/// Recompute `state.layout` and `state.content_height` for `width` pixels.
pub fn compute_layout(
    ctx: &dyn DrawContext,
    state: &mut MarkdownPreviewState,
    width: f64,
    style: &StyleContext,
) {
    let inner = (width - PAD * 2.0).max(0.0);
    let mut layout = Vec::with_capacity(state.blocks.len());
    let mut y = PAD;
    for blk in &state.blocks {
        let h = block_height(ctx, blk, inner, style, 0);
        layout.push(LayoutEntry { y, h });
        y += h + GAP;
    }
    state.layout = layout;
    state.content_height = y + PAD;
    // Widest block plus the outer padding, so horizontal scrolling can reach
    // the right edge of content that cannot wrap (code lines, wide table
    // columns, long unbreakable words). Never narrower than the pane.
    let mut widest = 0.0_f64;
    for blk in &state.blocks {
        let bw = block_width(ctx, blk, inner, style, 0);
        if bw > widest {
            widest = bw;
        }
    }
    state.content_width = (widest + PAD * 2.0).max(width);
    state.cached_width = width;
}

// ── Scrolling ────────────────────────────────────────────────────────────

/// Horizontal key step, in body line heights. Line height tracks the theme's
/// font size, so the step stays proportionate at any scale.
const KEY_H_STEP_MUL: f64 = 2.0;
/// Lines of the previous view kept on screen after a page scroll.
const PAGE_OVERLAP_LINES: f64 = 1.0;

/// Track and thumb rectangles of one preview scrollbar.
#[derive(Debug, Clone, Copy)]
pub struct ScrollbarGeometry {
    pub track: Rect,
    pub thumb: Rect,
}

impl ScrollbarGeometry {
    /// Whether a point lies anywhere on the track (thumb included).
    pub fn track_contains(&self, x: f64, y: f64) -> bool {
        x >= self.track.x
            && x < self.track.x + self.track.w
            && y >= self.track.y
            && y < self.track.y + self.track.h
    }

    /// Whether a point lies on the thumb.
    pub fn thumb_contains(&self, x: f64, y: f64) -> bool {
        x >= self.thumb.x
            && x < self.thumb.x + self.thumb.w
            && y >= self.thumb.y
            && y < self.thumb.y + self.thumb.h
    }
}

/// Largest valid `scroll_y` for the drawn pane.
pub fn max_scroll_y(state: &MarkdownPreviewState) -> f64 {
    (state.content_height - state.content_rect.h).max(0.0)
}

/// Largest valid `scroll_x` for the drawn pane.
pub fn max_scroll_x(state: &MarkdownPreviewState) -> f64 {
    (state.content_width - state.content_rect.w).max(0.0)
}

/// Vertical scrollbar geometry, or `None` when the content fits.
pub fn vertical_scrollbar(
    state: &MarkdownPreviewState,
    style: &StyleContext,
) -> Option<ScrollbarGeometry> {
    let pane = state.content_rect;
    if pane.w <= 0.0 || pane.h <= 0.0 || state.content_height <= pane.h {
        return None;
    }
    let size = style.scrollbar_size;
    let track = Rect {
        x: pane.x + pane.w - size,
        y: pane.y,
        w: size,
        h: pane.h,
    };
    let thumb_h = (track.h * (pane.h / state.content_height))
        .max(size * 2.0)
        .min(track.h);
    let max = max_scroll_y(state);
    let frac = if max > 0.0 {
        (state.scroll_y / max).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some(ScrollbarGeometry {
        track,
        thumb: Rect {
            x: track.x,
            y: track.y + frac * (track.h - thumb_h),
            w: size,
            h: thumb_h,
        },
    })
}

/// Horizontal scrollbar geometry, or `None` when the content fits. The track
/// stops short of the vertical scrollbar when both are present.
pub fn horizontal_scrollbar(
    state: &MarkdownPreviewState,
    style: &StyleContext,
) -> Option<ScrollbarGeometry> {
    let pane = state.content_rect;
    if pane.w <= 0.0 || pane.h <= 0.0 || state.content_width <= pane.w {
        return None;
    }
    let size = style.scrollbar_size;
    let track_w = if state.content_height > pane.h {
        (pane.w - size).max(0.0)
    } else {
        pane.w
    };
    let track = Rect {
        x: pane.x,
        y: pane.y + pane.h - size,
        w: track_w,
        h: size,
    };
    let thumb_w = (track.w * (pane.w / state.content_width))
        .max(size * 2.0)
        .min(track.w);
    let max = max_scroll_x(state);
    let frac = if max > 0.0 {
        (state.scroll_x / max).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some(ScrollbarGeometry {
        track,
        thumb: Rect {
            x: track.x + frac * (track.w - thumb_w),
            y: track.y,
            w: thumb_w,
            h: size,
        },
    })
}

/// Scroll by a pixel delta, clamped to the content extents.
pub fn scroll_by(state: &mut MarkdownPreviewState, dx: f64, dy: f64) {
    state.target_scroll_y = (state.target_scroll_y + dy).clamp(0.0, max_scroll_y(state));
    state.scroll_y = state.target_scroll_y;
    state.scroll_x = (state.scroll_x + dx).clamp(0.0, max_scroll_x(state));
}

/// Scroll so the vertical thumb's top edge lands at `thumb_top`.
pub fn scroll_to_thumb_top(state: &mut MarkdownPreviewState, style: &StyleContext, thumb_top: f64) {
    let Some(g) = vertical_scrollbar(state, style) else {
        return;
    };
    let travel = g.track.h - g.thumb.h;
    if travel <= 0.0 {
        return;
    }
    let frac = ((thumb_top - g.track.y) / travel).clamp(0.0, 1.0);
    state.target_scroll_y = frac * max_scroll_y(state);
    state.scroll_y = state.target_scroll_y;
}

/// Scroll so the horizontal thumb's left edge lands at `thumb_left`.
pub fn scroll_to_thumb_left(
    state: &mut MarkdownPreviewState,
    style: &StyleContext,
    thumb_left: f64,
) {
    let Some(g) = horizontal_scrollbar(state, style) else {
        return;
    };
    let travel = g.track.w - g.thumb.w;
    if travel <= 0.0 {
        return;
    }
    let frac = ((thumb_left - g.track.x) / travel).clamp(0.0, 1.0);
    state.scroll_x = frac * max_scroll_x(state);
}

/// Whether a focused preview swallows this keystroke instead of letting it
/// reach the document. The pane is read-only, so the keys that would type or
/// erase never pass; strokes carrying ctrl/alt/gui always do, keeping save,
/// find, and the preview toggle working while the preview holds focus.
pub fn swallows_edit_key(key: &str, mods: &crate::editor::event::Modifiers) -> bool {
    if mods.ctrl || mods.alt || mods.gui {
        return false;
    }
    matches!(
        key,
        "tab" | "backspace" | "delete" | "return" | "keypad enter"
    )
}

/// Apply a navigation key to a focused preview. Returns true when the key is
/// a scrolling key, so the caller can keep it from reaching the document.
pub fn scroll_key(state: &mut MarkdownPreviewState, key: &str, style: &StyleContext) -> bool {
    let line = style.font_height.max(1.0);
    let page = (state.content_rect.h - line * PAGE_OVERLAP_LINES).max(line);
    let step = line * KEY_H_STEP_MUL;
    match key {
        "up" => scroll_by(state, 0.0, -line),
        "down" => scroll_by(state, 0.0, line),
        "left" => scroll_by(state, -step, 0.0),
        "right" => scroll_by(state, step, 0.0),
        "pageup" => scroll_by(state, 0.0, -page),
        "pagedown" => scroll_by(state, 0.0, page),
        "home" => {
            state.target_scroll_y = 0.0;
            state.scroll_y = 0.0;
            state.scroll_x = 0.0;
        }
        "end" => {
            state.target_scroll_y = max_scroll_y(state);
            state.scroll_y = state.target_scroll_y;
        }
        _ => return false,
    }
    true
}

// ── Drawing ──────────────────────────────────────────────────────────────

fn span_color(span: &Span, style: &StyleContext) -> [u8; 4] {
    if span.href.is_some() {
        return LINK_COLOR;
    }
    if span.code {
        return style.good.to_array();
    }
    if span.strikethrough {
        let mut c = style.dim.to_array();
        c[3] = (c[3] as u16 * 3 / 4).min(255) as u8;
        return c;
    }
    // Italic has no italic font slot, so give it a distinctive tint instead —
    // the previous `style.dim` was too close to the strikethrough colour and
    // emphasis was invisible against body text. Using the accent colour makes
    // `*italic*` visually pop while staying theme-aware.
    if span.italic {
        return style.accent.to_array();
    }
    // Bold uses synthetic double-strike (see draw_inlines); no colour change.
    style.text.to_array()
}

/// Draw a wrapped span sequence starting at (x0, y0), using `base_font` and
/// `base_lh` for non-code spans. Mirrors `inlines_height` exactly — same
/// ws-gap handling so punctuation-following-inline-markup (`see [x](y).`)
/// renders without a spurious space before the period.
///
/// Returns the y below the last drawn line.
#[allow(clippy::too_many_arguments)]
fn draw_inlines(
    ctx: &mut dyn DrawContext,
    spans: &[Span],
    x0: f64,
    y0: f64,
    max_x: f64,
    base_font: u64,
    base_lh: f64,
    forced_color: Option<[u8; 4]>,
    style: &StyleContext,
    sink: &mut Sink<'_>,
    strike_through: bool,
) -> f64 {
    if spans.is_empty() || max_x <= x0 {
        return y0;
    }
    let code = style.code_font;
    let mut x = x0;
    let mut y = y0;
    let mut last = false;
    let mut ws_pending = false;
    for span in spans {
        if span.text == "\n" {
            x = x0;
            y += base_lh;
            last = false;
            ws_pending = false;
            sink.separate("\n");
            continue;
        }
        let font = if span.code { code } else { base_font };
        let col = forced_color.unwrap_or_else(|| span_color(span, style));
        let sw = ctx.font_width(font, " ");
        let leads_ws = span.text.starts_with(char::is_whitespace);
        let trails_ws = span.text.ends_with(char::is_whitespace);
        let mut placed_any = false;
        for (i, word) in span.text.split_whitespace().enumerate() {
            placed_any = true;
            let ww = ctx.font_width(font, word);
            let needs_space = if i == 0 {
                last && (ws_pending || leads_ws)
            } else {
                true
            };
            if needs_space {
                if x + sw + ww > max_x && x > x0 {
                    x = x0;
                    y += base_lh;
                } else {
                    x += sw;
                }
            } else if x + ww > max_x && x > x0 {
                x = x0;
                y += base_lh;
            }
            let wx0 = x;
            // Inline code gets a subtle background so it reads like a chip.
            if span.code {
                let mut bg = style.background2.to_array();
                bg[3] = 180;
                ctx.draw_rect(wx0 - 2.0, y, ww + 4.0, base_lh, bg);
            }
            sink.emit(
                ctx,
                font,
                word,
                wx0,
                y,
                ww,
                base_lh,
                if needs_space { " " } else { "" },
            );
            x = ctx.draw_text(font, word, wx0, y, col);
            // Synthetic bold: draw a second time offset by one pixel so the
            // glyph strokes thicken. Cheap and font-agnostic — we don't ship a
            // bold font slot, so this is the only way `**bold**` actually
            // looks bold. Applies to every bold span except inline code (which
            // already has its own colour).
            if span.bold && !span.code {
                ctx.draw_text(font, word, wx0 + 1.0, y, col);
            }
            if strike_through || span.strikethrough {
                // 1px horizontal line through the word at its visual
                // midline. `base_lh * 0.55` lands near the x-height
                // center for the body fonts we ship.
                let mid_y = (y + base_lh * 0.55).floor();
                ctx.draw_rect(wx0, mid_y, (x - wx0).max(1.0), 1.0, col);
            }
            if let Some(href) = &span.href {
                sink.links.push(LinkRegion {
                    x1: wx0,
                    y1: y,
                    x2: x,
                    y2: y + base_lh,
                    href: href.clone(),
                });
            }
            last = true;
        }
        if !placed_any {
            if !span.text.is_empty() {
                ws_pending = true;
            }
            continue;
        }
        ws_pending = trails_ws;
    }
    y + base_lh
}

#[allow(clippy::too_many_arguments)]
fn draw_block(
    ctx: &mut dyn DrawContext,
    blk: &Block,
    x: f64,
    y: f64,
    max_x: f64,
    style: &StyleContext,
    pane_clip: Rect,
    code_tokens: Option<&Vec<Vec<crate::editor::tokenizer::Token>>>,
    sink: &mut Sink<'_>,
    depth: usize,
) {
    if depth >= MAX_RENDER_DEPTH {
        return;
    }
    let lh = style.font_height;
    let clh = style.code_font_height;
    let body = style.font;

    match blk {
        Block::Heading { level, inlines } => {
            // Heading uses its own (font, lh) pair that `block_height`
            // already reserved space for — sharing the same call keeps
            // measurement and drawing aligned.
            let (hfont, hlh) = heading_metrics(ctx, *level, style);
            let top_gap = if *level == 1 {
                (hlh * H1_TOP_GAP_MUL).ceil()
            } else {
                (lh * HX_TOP_GAP_MUL).ceil()
            };
            let text_y = y + top_gap;
            let color = heading_color(*level, style);
            let end_y = draw_inlines(
                ctx,
                inlines,
                x,
                text_y,
                max_x,
                hfont,
                hlh,
                Some(color),
                style,
                sink,
                false,
            );
            if *level <= 2 {
                // Bottom rule inside the slot `HEADING_RULE_GAP` reserved.
                // h1 gets a thicker 2px rule in the accent color; h2 is a
                // subtle 1px divider line to signal secondary hierarchy.
                let rule_y = (end_y + HEADING_RULE_GAP * 0.5 - 1.0).floor();
                let (rule_h, rule_col) = if *level == 1 {
                    (2.0, style.accent.to_array())
                } else {
                    (1.0, style.divider.to_array())
                };
                ctx.draw_rect(x, rule_y, max_x - x, rule_h, rule_col);
            }
        }
        Block::Paragraph { inlines } => {
            draw_inlines(
                ctx, inlines, x, y, max_x, body, lh, None, style, sink, false,
            );
        }
        Block::Code { text, .. } => {
            // Match the pad used by `block_height` exactly. Pad scales
            // with body line height so small/large themes stay balanced.
            let pad = (lh * 0.75).ceil();
            let line_count = code_block_line_count(text);
            let total_h = line_count as f64 * clh + pad * 2.0;
            // The panel spans the widest line so horizontally scrolled code
            // keeps its background instead of running onto the page.
            let panel_w = (max_x - x).max(code_block_width(ctx, text, style));
            // Panel background + a thin left accent bar.
            ctx.draw_rect(x, y, panel_w, total_h, style.background2.to_array());
            ctx.draw_rect(x, y, 3.0, total_h, style.accent.to_array());
            let mut cy = y + pad;
            let text_x = x + pad + 3.0;
            if let Some(lines) = code_tokens {
                // Tokenized path: colour each run using the active theme's
                // syntax palette so ```lang fences read like the editor does.
                for (line_idx, line) in text.split('\n').enumerate() {
                    sink.separate("\n");
                    if let Some(tokens) = lines.get(line_idx) {
                        let mut tx = text_x;
                        for tok in tokens {
                            let color =
                                crate::editor::doc_view::syntax_color(&tok.token_type, style);
                            let tw = ctx.font_width(style.code_font, &tok.text);
                            sink.emit(ctx, style.code_font, &tok.text, tx, cy, tw, clh, "");
                            tx = ctx.draw_text(style.code_font, &tok.text, tx, cy, color);
                        }
                    } else {
                        let lw = ctx.font_width(style.code_font, line);
                        sink.emit(ctx, style.code_font, line, text_x, cy, lw, clh, "");
                        ctx.draw_text(style.code_font, line, text_x, cy, style.text.to_array());
                    }
                    cy += clh;
                }
            } else {
                // No fence language (or an unknown one) — render with the
                // plain body text colour. The old green `style.good` tint
                // looked like "this is highlighted" even when there was no
                // syntax behind it, which misled readers.
                let code_color = style.text.to_array();
                for line in text.split('\n') {
                    sink.separate("\n");
                    let lw = ctx.font_width(style.code_font, line);
                    sink.emit(ctx, style.code_font, line, text_x, cy, lw, clh, "");
                    ctx.draw_text(style.code_font, line, text_x, cy, code_color);
                    cy += clh;
                }
            }
        }
        Block::Rule => {
            // Center the rule vertically inside the full slot that
            // `block_height` reserved, so space above and below is equal.
            let slot_h = lh + (lh * 0.5).ceil();
            let mid = (y + slot_h * 0.5).floor();
            ctx.draw_rect(x, mid, max_x - x, 1.0, style.divider.to_array());
        }
        Block::Quote { blocks } => {
            let vpad = (lh * 0.6).ceil();
            let inner_x = x + QUOTE_INDENT;
            // Left accent bar spans the whole block. Height is measured so
            // the bar ends flush with the last inner block.
            let mut cur_y = y + vpad;
            let mut first = true;
            for sub in blocks {
                if !first {
                    cur_y += GAP;
                }
                // Nested blockquotes don't carry pre-tokenized code; pass None
                // so the inner code block falls back to plain colour. Top-level
                // fences still highlight — this only affects fences embedded
                // inside quotes, which are rare.
                draw_block(
                    ctx,
                    sub,
                    inner_x,
                    cur_y,
                    max_x,
                    style,
                    pane_clip,
                    None,
                    sink,
                    depth + 1,
                );
                cur_y += block_height(ctx, sub, max_x - inner_x, style, depth + 1);
                first = false;
            }
            let total_h = (cur_y + vpad - y).max(lh);
            ctx.draw_rect(x, y, QUOTE_BAR_W, total_h, style.accent.to_array());
        }
        Block::List {
            items,
            ordered,
            start,
        } => draw_list(
            ctx, items, *ordered, *start, x, y, max_x, style, pane_clip, sink, depth,
        ),
        Block::Table {
            alignments,
            head,
            rows,
        } => draw_table(
            ctx, alignments, head, rows, x, y, max_x, style, pane_clip, sink,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_list(
    ctx: &mut dyn DrawContext,
    items: &[ListItem],
    ordered: bool,
    start_num: u64,
    x: f64,
    y: f64,
    max_x: f64,
    style: &StyleContext,
    pane_clip: Rect,
    sink: &mut Sink<'_>,
    depth: usize,
) {
    let lh = style.font_height;
    let body = style.font;
    let text_color = style.text.to_array();
    let dim_color = style.dim.to_array();
    let accent_color = style.accent.to_array();
    // Match the inter-item gap used by `block_height` or rows will overlap.
    let item_gap = (lh * 0.5).ceil();
    let content_x = x + LIST_GUTTER;

    let mut cur_y = y;
    let mut first = true;
    for (i, item) in items.iter().enumerate() {
        if !first {
            cur_y += item_gap;
        }
        sink.separate("\n");
        // Always draw the bullet/number/checkbox inside the fixed gutter so
        // the content column width stays constant. This keeps measurement
        // (`block_height` uses `width - LIST_GUTTER`) consistent with draw.
        if let Some(checked) = item.task {
            // Box sized to fit comfortably inside one line of body text.
            let box_size = (lh * 0.58).floor().clamp(10.0, lh - 5.0);
            // Center vertically on the glyph x-height rather than the
            // full line slot -- UI fonts carry more descender/leading
            // than ascender room, so the line-slot center lands below
            // where the eye reads "middle of the letters".
            let box_y = (cur_y + (style.font_height - box_size) * 0.5).round();
            let box_x = x + LIST_MARKER_INSET;
            // Interior fill: slightly lighter than the page background so
            // the box reads as a distinct surface even when empty.
            ctx.draw_rect(
                box_x,
                box_y,
                box_size,
                box_size,
                style.background3.to_array(),
            );
            // Outline.
            ctx.draw_rect(box_x, box_y, box_size, 1.0, text_color);
            ctx.draw_rect(box_x, box_y + box_size - 1.0, box_size, 1.0, text_color);
            ctx.draw_rect(box_x, box_y, 1.0, box_size, text_color);
            ctx.draw_rect(box_x + box_size - 1.0, box_y, 1.0, box_size, text_color);
            if checked {
                // Filled inner square in accent.
                let inset = (box_size * 0.25).floor().max(2.0);
                let fill = (box_size - inset * 2.0).max(1.0);
                ctx.draw_rect(box_x + inset, box_y + inset, fill, fill, accent_color);
            }
            if let Some(src) = item.source_start {
                sink.checkboxes.push(CheckboxRegion {
                    x1: box_x,
                    y1: box_y,
                    x2: box_x + box_size,
                    y2: box_y + box_size,
                    source_start: src,
                    checked,
                });
            }
        } else {
            let bullet = if ordered {
                format!("{}.", start_num + i as u64)
            } else {
                "\u{2022}".to_string()
            };
            let bw = ctx.font_width(body, &bullet);
            let bx = x + LIST_MARKER_INSET;
            sink.emit(ctx, body, &bullet, bx, cur_y, bw, lh, "");
            ctx.draw_text(body, &bullet, bx, cur_y, dim_color);
            sink.separate(" ");
        }

        let ih = inlines_height(ctx, &item.spans, max_x - content_x, body, lh, style);
        // Checked task items render with a dim color + a horizontal
        // strikethrough through each word, matching the visual TODO
        // convention ("[x] done" = crossed out).
        let item_checked = item.task == Some(true);
        let item_color = if item_checked { Some(dim_color) } else { None };
        draw_inlines(
            ctx,
            &item.spans,
            content_x,
            cur_y,
            max_x,
            body,
            lh,
            item_color,
            style,
            sink,
            item_checked,
        );
        cur_y += ih.max(lh);
        // Block children (e.g. a nested list) render in the item's text
        // column, one level deeper so the recursion cap applies.
        for sub in &item.blocks {
            cur_y += GAP;
            draw_block(
                ctx,
                sub,
                content_x,
                cur_y,
                max_x,
                style,
                pane_clip,
                None,
                sink,
                depth + 1,
            );
            cur_y += block_height(ctx, sub, max_x - content_x, style, depth + 1);
        }
        first = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_table(
    ctx: &mut dyn DrawContext,
    alignments: &[crate::editor::markdown::Alignment],
    head: &[Vec<Span>],
    rows: &[Vec<Vec<Span>>],
    x: f64,
    y: f64,
    max_x: f64,
    style: &StyleContext,
    pane_clip: Rect,
    sink: &mut Sink<'_>,
) {
    let n_cols = alignments.len().max(head.len()).max(1);
    let lh = style.font_height;
    let body = style.font;
    let col_w = table_col_width(ctx, head, rows, n_cols, max_x - x, body, style);
    let total_w = col_w * n_cols as f64;
    let inner_cell_w = (col_w - TABLE_CELL_PAD * 2.0).max(0.0);
    let divider = style.divider.to_array();
    let line_hl = style.line_highlight.to_array();
    let text_color = style.text.to_array();
    let accent_color = style.accent.to_array();

    // Top border.
    ctx.draw_rect(x, y, total_w, 1.0, divider);
    let mut cur_y = y + 1.0;

    if !head.is_empty() {
        let h = table_row_height(ctx, head, inner_cell_w, body, lh, style);
        ctx.draw_rect(x, cur_y, total_w, h, line_hl);
        draw_table_row(
            ctx,
            head,
            x,
            cur_y,
            col_w,
            n_cols,
            h,
            Some(accent_color),
            body,
            lh,
            style,
            pane_clip,
            sink,
        );
        restore_pane_clip(ctx, pane_clip);
        cur_y += h;
        ctx.draw_rect(x, cur_y, total_w, 1.0, divider);
        cur_y += 1.0;
    }
    for row in rows {
        let h = table_row_height(ctx, row, inner_cell_w, body, lh, style);
        draw_table_row(
            ctx,
            row,
            x,
            cur_y,
            col_w,
            n_cols,
            h,
            Some(text_color),
            body,
            lh,
            style,
            pane_clip,
            sink,
        );
        restore_pane_clip(ctx, pane_clip);
        cur_y += h;
        ctx.draw_rect(x, cur_y, total_w, 1.0, divider);
        cur_y += 1.0;
    }
    // Left + right + interior column borders.
    let final_y = cur_y;
    let height = final_y - y;
    ctx.draw_rect(x, y, 1.0, height, divider);
    for i in 1..=n_cols {
        let cx = x + col_w * i as f64;
        ctx.draw_rect(cx, y, 1.0, height, divider);
    }
}

fn table_row_height(
    ctx: &dyn DrawContext,
    cells: &[Vec<Span>],
    inner_cell_w: f64,
    body: u64,
    lh: f64,
    style: &StyleContext,
) -> f64 {
    let mut max = lh;
    for cell in cells {
        let ch = inlines_height(ctx, cell, inner_cell_w, body, lh, style);
        if ch > max {
            max = ch;
        }
    }
    max + TABLE_CELL_PAD * 2.0
}

#[allow(clippy::too_many_arguments)]
fn draw_table_row(
    ctx: &mut dyn DrawContext,
    cells: &[Vec<Span>],
    x: f64,
    y: f64,
    col_w: f64,
    n_cols: usize,
    row_h: f64,
    forced_color: Option<[u8; 4]>,
    body: u64,
    lh: f64,
    style: &StyleContext,
    pane_clip: Rect,
    sink: &mut Sink<'_>,
) {
    for i in 0..n_cols {
        let cx = x + col_w * i as f64;
        if let Some(cell) = cells.get(i) {
            sink.separate(if i == 0 { "\n" } else { "\t" });
            // Clip so long content can't spill into the next column, while
            // still respecting the preview pane bounds.
            set_intersected_clip_rect(
                ctx,
                pane_clip,
                cx + TABLE_CELL_PAD,
                y,
                (col_w - TABLE_CELL_PAD * 2.0).max(0.0),
                row_h,
            );
            draw_inlines(
                ctx,
                cell,
                cx + TABLE_CELL_PAD,
                y + TABLE_CELL_PAD,
                cx + col_w - TABLE_CELL_PAD,
                body,
                lh,
                forced_color,
                style,
                sink,
                false,
            );
        }
    }
    // Note: the per-cell clip is not reset here. The outer `draw` loop
    // re-applies the preview pane clip after every block, and `draw_table`
    // restores it between rows before drawing row dividers.
}

fn set_intersected_clip_rect(
    ctx: &mut dyn DrawContext,
    pane_clip: Rect,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) {
    let x1 = x.max(pane_clip.x);
    let y1 = y.max(pane_clip.y);
    let x2 = (x + w).min(pane_clip.x + pane_clip.w);
    let y2 = (y + h).min(pane_clip.y + pane_clip.h);
    ctx.set_clip_rect(x1, y1, (x2 - x1).max(0.0), (y2 - y1).max(0.0));
}

fn restore_pane_clip(ctx: &mut dyn DrawContext, pane_clip: Rect) {
    ctx.set_clip_rect(pane_clip.x, pane_clip.y, pane_clip.w, pane_clip.h);
}

// ── Top-level draw + URL helpers ─────────────────────────────────────────

/// Where the block list is placed for one render pass.
#[derive(Clone, Copy)]
struct Geometry {
    pane: Rect,
    inner_x: f64,
    inner_max_x: f64,
    base_y: f64,
}

impl Geometry {
    /// Screen position of a content-space point.
    fn screen_of(&self, (cx, cy): (f64, f64)) -> (f64, f64) {
        (cx + self.inner_x - PAD, cy + self.base_y)
    }
}

/// Walk the block list, drawing into `ctx` and feeding `sink`. `cull` skips
/// blocks outside the pane, which is what the draw pass wants; the pass that
/// assembles the selected text walks everything so content scrolled out of
/// view is still part of the range.
#[allow(clippy::too_many_arguments)]
fn render_blocks(
    ctx: &mut dyn DrawContext,
    blocks: &[Block],
    layout: &[LayoutEntry],
    code_tokens: &[Option<Vec<Vec<crate::editor::tokenizer::Token>>>],
    style: &StyleContext,
    geom: Geometry,
    sink: &mut Sink<'_>,
    cull: bool,
) {
    let pane = geom.pane;
    restore_pane_clip(ctx, pane);
    for (i, blk) in blocks.iter().enumerate() {
        let Some(entry) = layout.get(i) else {
            continue;
        };
        let sy = geom.base_y + entry.y;
        if cull {
            if sy + entry.h < pane.y {
                continue;
            }
            if sy > pane.y + pane.h {
                break;
            }
        }
        sink.separate("\n\n");
        let tokens = code_tokens.get(i).and_then(|o| o.as_ref());
        draw_block(
            ctx,
            blk,
            geom.inner_x,
            sy,
            geom.inner_max_x,
            style,
            pane,
            tokens,
            sink,
            0,
        );
        // Re-apply the preview clip after every block. `draw_block` may
        // leave the clip narrowed (tables set per-cell clips for spill
        // protection), and without this the next block would render into
        // a tiny stale rect and silently disappear. This is specifically
        // what was cutting off content after the first table in README.md.
        restore_pane_clip(ctx, pane);
    }
}

/// The selection in screen coordinates, ordered so the first point reads
/// before the second. `None` when nothing is selected.
fn selection_bounds(state: &MarkdownPreviewState, geom: Geometry) -> Option<(f64, f64, f64, f64)> {
    let (anchor, head) = (state.sel_anchor?, state.sel_head?);
    let (ax, ay) = geom.screen_of(anchor);
    let (hx, hy) = geom.screen_of(head);
    if (ay, ax) <= (hy, hx) {
        Some((ax, ay, hx, hy))
    } else {
        Some((hx, hy, ax, ay))
    }
}

/// Draw the preview inside the given rect, recomputing layout when the
/// width has changed. Resets and repopulates `link_regions` /
/// `checkbox_regions` from the current frame's geometry.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ctx: &mut dyn DrawContext,
    state: &mut MarkdownPreviewState,
    style: &StyleContext,
    rect_x: f64,
    rect_y: f64,
    rect_w: f64,
    rect_h: f64,
) {
    if (state.cached_width - rect_w).abs() > 0.5 || state.layout.is_empty() {
        compute_layout(ctx, state, rect_w, style);
    }

    // Snap the rect to the enclosing integer-pixel box. Callers may pass
    // fractional values (split pane uses `padding_y * 0.5` etc.), and
    // `draw_rect`'s i32 cast truncates — without snapping, the bottom or
    // right row of pixels inside the "logical" rect is never cleared and
    // stale content from the previous frame shows through.
    let px = rect_x.floor();
    let py = rect_y.floor();
    let pr = (rect_x + rect_w).ceil();
    let pb = (rect_y + rect_h).ceil();
    let pw = pr - px;
    let ph = pb - py;

    // Background fill — covers the full integer-aligned preview rect every
    // frame so stale pixels from the previous frame can't leak through.
    ctx.draw_rect(px, py, pw, ph, style.background.to_array());

    // Publish the geometry the rest of the frame's content is placed in, so
    // scroll clamping and the main loop's scrollbar hit-tests agree with what
    // is drawn here.
    state.content_rect = Rect {
        x: px,
        y: py,
        w: pw,
        h: ph,
    };
    state.target_scroll_y = state.target_scroll_y.clamp(0.0, max_scroll_y(state));
    state.scroll_y = state.scroll_y.clamp(0.0, max_scroll_y(state));
    state.scroll_x = state.scroll_x.clamp(0.0, max_scroll_x(state));

    let geom = pane_geometry(state);
    let selection = selection_bounds(state, geom);
    // Same fill the document pane uses, so a selection reads the same on
    // both sides of the split.
    let highlight = style.selection.to_array();

    let MarkdownPreviewState {
        blocks,
        layout,
        code_tokens,
        link_regions,
        checkbox_regions,
        ..
    } = state;
    link_regions.clear();
    checkbox_regions.clear();
    let mut sink = Sink {
        links: link_regions,
        checkboxes: checkbox_regions,
        selection,
        highlight,
        collecting: false,
        picked: String::new(),
        pending_sep: None,
        run_end: None,
        probe: None,
        hit: None,
    };
    render_blocks(
        ctx,
        blocks,
        layout,
        code_tokens,
        style,
        geom,
        &mut sink,
        true,
    );

    // Scrollbars — mirror the main text panel so the preview also gets
    // vertical and horizontal scrollbars when its content overflows the
    // pane. Both come from the same geometry the input handlers hit-test.
    let sb_track = style.scrollbar_track.to_array();
    let sb_thumb = style.scrollbar.to_array();
    for bar in [
        vertical_scrollbar(state, style),
        horizontal_scrollbar(state, style),
    ]
    .into_iter()
    .flatten()
    {
        ctx.draw_rect(bar.track.x, bar.track.y, bar.track.w, bar.track.h, sb_track);
        ctx.draw_rect(bar.thumb.x, bar.thumb.y, bar.thumb.w, bar.thumb.h, sb_thumb);
    }
}

/// The placement the last draw used, rebuilt from the published pane rect
/// and the current scroll so hit-tests and the copy pass line up with what
/// the reader sees.
fn pane_geometry(state: &MarkdownPreviewState) -> Geometry {
    // Scroll is snapped to a whole pixel before block positions are
    // computed: the lerp used by the main loop produces fractional values
    // and `draw_text` truncates to i32, so an unsnapped origin leaves glyphs
    // half a pixel above the background clear.
    let pane = state.content_rect;
    let scroll_x = state.scroll_x.floor();
    let scroll_y = state.scroll_y.floor();
    Geometry {
        pane,
        inner_x: pane.x + PAD - scroll_x,
        inner_max_x: pane.x + pane.w - PAD - scroll_x,
        base_y: pane.y - scroll_y,
    }
}

/// A draw context that measures exactly as the real one but paints nothing.
/// Lets the selection pass reuse the layout code that draws the document.
struct MeasureOnly<'a>(&'a mut dyn DrawContext);

impl DrawContext for MeasureOnly<'_> {
    fn draw_rect(&mut self, _: f64, _: f64, _: f64, _: f64, _: [u8; 4]) {}
    fn draw_text(&mut self, font_id: u64, text: &str, x: f64, _: f64, _: [u8; 4]) -> f64 {
        x + self.0.font_width(font_id, text)
    }
    fn set_clip_rect(&mut self, _: f64, _: f64, _: f64, _: f64) {}
    fn font_height(&self, font_id: u64) -> f64 {
        self.0.font_height(font_id)
    }
    fn font_width(&self, font_id: u64, text: &str) -> f64 {
        self.0.font_width(font_id, text)
    }
    fn draw_image(&mut self, _: &std::sync::Arc<Vec<u8>>, _: i32, _: i32, _: f64, _: f64) {}
}

// ── Selection ────────────────────────────────────────────────────────────

/// Content-space position of a screen point inside the pane. Content space
/// is the pane's top-left corner plus the scroll offset, so a point keeps
/// pointing at the same text as the pane scrolls.
pub fn point_to_content(state: &MarkdownPreviewState, x: f64, y: f64) -> (f64, f64) {
    let geom = pane_geometry(state);
    (x - (geom.inner_x - PAD), y - geom.base_y)
}

impl MarkdownPreviewState {
    /// Whether the pane holds a selection that covers at least one character.
    pub fn has_selection(&self) -> bool {
        match (self.sel_anchor, self.sel_head) {
            (Some(a), Some(h)) => a != h,
            _ => false,
        }
    }

    /// Drop the selection.
    pub fn clear_selection(&mut self) {
        self.sel_anchor = None;
        self.sel_head = None;
    }

    /// Anchor a new selection at a screen point.
    pub fn begin_selection(&mut self, x: f64, y: f64) {
        let p = point_to_content(self, x, y);
        self.sel_anchor = Some(p);
        self.sel_head = Some(p);
    }

    /// Extend the anchored selection to a screen point.
    pub fn extend_selection(&mut self, x: f64, y: f64) {
        if self.sel_anchor.is_some() {
            self.sel_head = Some(point_to_content(self, x, y));
        }
    }

    /// Select the whole document.
    pub fn select_all(&mut self) {
        self.sel_anchor = Some((-1.0, -1.0));
        self.sel_head = Some((
            self.content_width + PAD + 1.0,
            self.content_height + PAD + 1.0,
        ));
    }
}

/// Select the word under a screen point, or the whole visual line it sits
/// on when `whole_line` is set. Returns false when the point is not over
/// any text, leaving the selection untouched.
pub fn select_at(
    ctx: &mut dyn DrawContext,
    state: &mut MarkdownPreviewState,
    style: &StyleContext,
    x: f64,
    y: f64,
    whole_line: bool,
) -> bool {
    let geom = pane_geometry(state);
    let mut links = Vec::new();
    let mut checkboxes = Vec::new();
    let mut sink = Sink {
        links: &mut links,
        checkboxes: &mut checkboxes,
        selection: None,
        highlight: [0, 0, 0, 0],
        collecting: false,
        picked: String::new(),
        pending_sep: None,
        run_end: None,
        probe: Some((x, y)),
        hit: None,
    };
    let mut measure = MeasureOnly(ctx);
    render_blocks(
        &mut measure,
        &state.blocks,
        &state.layout,
        &state.code_tokens,
        style,
        geom,
        &mut sink,
        true,
    );
    let Some((x0, y0, x1, y1)) = sink.hit else {
        return false;
    };
    // Anchor both ends on the fragment's own vertical midpoint so the range
    // covers exactly the line band the fragment was drawn in.
    let mid = (y0 + y1) / 2.0;
    let (left, right) = if whole_line {
        (geom.pane.x - 1.0, geom.pane.x + state.content_width + PAD)
    } else {
        (x0, x1)
    };
    state.sel_anchor = Some(point_to_content(state, left, mid));
    state.sel_head = Some(point_to_content(state, right, mid));
    true
}

/// The selected text, assembled in reading order. Walks the whole document,
/// so a selection that extends past the visible pane still copies in full.
pub fn selected_text(
    ctx: &mut dyn DrawContext,
    state: &MarkdownPreviewState,
    style: &StyleContext,
) -> String {
    let geom = pane_geometry(state);
    let Some(selection) = selection_bounds(state, geom) else {
        return String::new();
    };
    let mut links = Vec::new();
    let mut checkboxes = Vec::new();
    let mut sink = Sink {
        links: &mut links,
        checkboxes: &mut checkboxes,
        selection: Some(selection),
        highlight: [0, 0, 0, 0],
        collecting: true,
        picked: String::new(),
        pending_sep: None,
        run_end: None,
        probe: None,
        hit: None,
    };
    let mut measure = MeasureOnly(ctx);
    render_blocks(
        &mut measure,
        &state.blocks,
        &state.layout,
        &state.code_tokens,
        style,
        geom,
        &mut sink,
        false,
    );
    sink.picked
}

/// Open a URL in the OS default browser.
pub fn open_url(href: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(href).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", href])
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(href).spawn();
    }
}

/// Locate the `[ ]` / `[x]` marker for a task list item starting at
/// `source_start` in the source text. Returns `(line_1based, col_1based,
/// new_char)` so the caller can do a single-character replace.
pub fn toggle_task_at(
    source: &str,
    source_start: usize,
    currently_checked: bool,
) -> Option<(usize, usize, char)> {
    if source_start > source.len() {
        return None;
    }
    let line_end = source[source_start..]
        .find('\n')
        .map(|i| source_start + i)
        .unwrap_or(source.len());
    let slice = source.get(source_start..line_end)?;
    let bytes = slice.as_bytes();
    let mut marker_byte = None;
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i] == b'[' && bytes[i + 2] == b']' {
            let inner = bytes[i + 1];
            let matches = if currently_checked {
                inner == b'x' || inner == b'X'
            } else {
                inner == b' '
            };
            if matches {
                marker_byte = Some(source_start + i + 1);
                break;
            }
        }
        i += 1;
    }
    let marker_byte = marker_byte?;
    let (line, col) = byte_to_line_col(source, marker_byte);
    let new_char = if currently_checked { ' ' } else { 'x' };
    Some((line, col, new_char))
}

fn byte_to_line_col(source: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (i, b) in source.as_bytes().iter().enumerate() {
        if i == byte_offset {
            break;
        }
        if *b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let col = source
        .get(line_start..byte_offset)
        .map(|s| s.chars().count() + 1)
        .unwrap_or(1);
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed-metrics context: every glyph is `CHAR_W` wide and every font is
    /// `LINE_H` tall, so measurements in the tests are exact.
    struct FixedMetrics;

    const CHAR_W: f64 = 8.0;
    const LINE_H: f64 = 16.0;

    impl DrawContext for FixedMetrics {
        fn draw_rect(&mut self, _: f64, _: f64, _: f64, _: f64, _: [u8; 4]) {}
        fn draw_text(&mut self, _: u64, text: &str, x: f64, _: f64, _: [u8; 4]) -> f64 {
            x + text.chars().count() as f64 * CHAR_W
        }
        fn set_clip_rect(&mut self, _: f64, _: f64, _: f64, _: f64) {}
        fn font_height(&self, _: u64) -> f64 {
            LINE_H
        }
        fn font_width(&self, _: u64, text: &str) -> f64 {
            text.chars().count() as f64 * CHAR_W
        }
        fn draw_image(&mut self, _: &std::sync::Arc<Vec<u8>>, _: i32, _: i32, _: f64, _: f64) {}
    }

    fn test_style() -> StyleContext {
        StyleContext {
            font_height: LINE_H,
            code_font_height: LINE_H,
            h1_font_height: LINE_H,
            h2_font_height: LINE_H,
            h3_font_height: LINE_H,
            scrollbar_size: 10.0,
            ..StyleContext::default()
        }
    }

    /// Lay out `source` in a `w` x `h` pane and return the state with a
    /// `content_rect` matching what `draw` would publish.
    fn laid_out(source: &str, w: f64, h: f64) -> MarkdownPreviewState {
        let ctx = FixedMetrics;
        let style = test_style();
        let mut state = MarkdownPreviewState {
            enabled: true,
            blocks: crate::editor::markdown::parse(source),
            ..MarkdownPreviewState::default()
        };
        compute_layout(&ctx, &mut state, w, &style);
        state.content_rect = Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        };
        state
    }

    /// Copy what `draw` would show as selected between two content points.
    fn copy_between(state: &mut MarkdownPreviewState, from: (f64, f64), to: (f64, f64)) -> String {
        let mut ctx = FixedMetrics;
        state.sel_anchor = Some(from);
        state.sel_head = Some(to);
        selected_text(&mut ctx, state, &test_style())
    }

    #[test]
    fn selecting_nothing_copies_nothing() {
        let mut ctx = FixedMetrics;
        let state = laid_out("hello world", 400.0, 200.0);
        assert_eq!(selected_text(&mut ctx, &state, &test_style()), "");
        assert!(!state.has_selection());
    }

    #[test]
    fn dragging_across_a_paragraph_copies_its_words() {
        let mut state = laid_out("alpha beta gamma", 400.0, 200.0);
        let copied = copy_between(&mut state, (0.0, PAD), (1000.0, PAD + LINE_H));
        assert_eq!(copied, "alpha beta gamma");
    }

    #[test]
    fn a_partial_drag_copies_only_the_covered_characters() {
        let mut state = laid_out("alpha beta", 400.0, 200.0);
        // The first block sits at (PAD, PAD) in content space; take the
        // first three glyphs of "alpha".
        let mid = PAD + LINE_H / 2.0;
        let start = (PAD, mid);
        let end = (PAD + 3.0 * CHAR_W, mid);
        assert_eq!(copy_between(&mut state, start, end), "alp");
    }

    #[test]
    fn selecting_backwards_copies_the_same_text() {
        let mut state = laid_out("alpha beta", 400.0, 200.0);
        let mid = PAD + LINE_H / 2.0;
        let a = (PAD, mid);
        let b = (PAD + 5.0 * CHAR_W, mid);
        assert_eq!(
            copy_between(&mut state, b, a),
            copy_between(&mut state, a, b)
        );
    }

    #[test]
    fn select_all_copies_every_block_separated_by_blank_lines() {
        let mut ctx = FixedMetrics;
        let mut state = laid_out("first para\n\nsecond para", 400.0, 200.0);
        state.select_all();
        let copied = selected_text(&mut ctx, &state, &test_style());
        assert_eq!(copied, "first para\n\nsecond para");
    }

    #[test]
    fn select_all_keeps_code_block_lines_on_their_own_lines() {
        let mut ctx = FixedMetrics;
        let mut state = laid_out("```\nlet a = 1\nlet b = 2\n```", 400.0, 200.0);
        state.select_all();
        let copied = selected_text(&mut ctx, &state, &test_style());
        assert!(
            copied.contains("let a = 1\nlet b = 2"),
            "code lines should keep their breaks, got {copied:?}"
        );
    }

    #[test]
    fn select_all_reaches_content_scrolled_out_of_view() {
        let mut ctx = FixedMetrics;
        let source: String = (0..60)
            .map(|i| format!("line{i}\n\n"))
            .collect::<Vec<_>>()
            .join("");
        // A pane far shorter than the content, so the draw pass would cull
        // almost all of it.
        let mut state = laid_out(&source, 400.0, 40.0);
        state.select_all();
        let copied = selected_text(&mut ctx, &state, &test_style());
        assert!(copied.starts_with("line0"), "got {copied:?}");
        assert!(copied.ends_with("line59"), "got {copied:?}");
    }

    #[test]
    fn a_table_copies_as_tab_separated_rows() {
        let mut ctx = FixedMetrics;
        let mut state = laid_out("| a | b |\n| - | - |\n| 1 | 2 |", 600.0, 400.0);
        state.select_all();
        let copied = selected_text(&mut ctx, &state, &test_style());
        assert!(copied.contains("a\tb"), "got {copied:?}");
        assert!(copied.contains("1\t2"), "got {copied:?}");
    }

    #[test]
    fn a_list_copies_one_item_per_line_with_its_bullet() {
        let mut ctx = FixedMetrics;
        let mut state = laid_out("- one\n- two", 400.0, 200.0);
        state.select_all();
        let copied = selected_text(&mut ctx, &state, &test_style());
        assert!(copied.contains("\u{2022} one"), "got {copied:?}");
        assert!(copied.contains("\u{2022} two"), "got {copied:?}");
        assert!(
            copied.contains("one\n"),
            "items need their own lines: {copied:?}"
        );
    }

    #[test]
    fn a_selection_survives_scrolling_the_pane() {
        let mut state = laid_out("alpha beta\n\ngamma delta", 400.0, 200.0);
        let mid = PAD + LINE_H / 2.0;
        let before = copy_between(&mut state, (PAD, mid), (1000.0, mid));
        state.scroll_y = 7.0;
        let mut ctx = FixedMetrics;
        let after = selected_text(&mut ctx, &state, &test_style());
        assert_eq!(before, after);
    }

    #[test]
    fn clearing_the_selection_copies_nothing() {
        let mut ctx = FixedMetrics;
        let mut state = laid_out("alpha beta", 400.0, 200.0);
        state.select_all();
        state.clear_selection();
        assert_eq!(selected_text(&mut ctx, &state, &test_style()), "");
    }

    #[test]
    fn double_clicking_a_word_selects_just_that_word() {
        let mut ctx = FixedMetrics;
        let mut state = laid_out("alpha beta gamma", 400.0, 200.0);
        // "beta" starts one space after "alpha".
        let x = PAD + 7.0 * CHAR_W;
        let y = PAD + LINE_H / 2.0;
        assert!(select_at(&mut ctx, &mut state, &test_style(), x, y, false));
        assert_eq!(selected_text(&mut ctx, &state, &test_style()), "beta");
    }

    #[test]
    fn triple_clicking_selects_the_whole_line() {
        let mut ctx = FixedMetrics;
        let mut state = laid_out("alpha beta gamma", 400.0, 200.0);
        let x = PAD + 7.0 * CHAR_W;
        let y = PAD + LINE_H / 2.0;
        assert!(select_at(&mut ctx, &mut state, &test_style(), x, y, true));
        assert_eq!(
            selected_text(&mut ctx, &state, &test_style()),
            "alpha beta gamma"
        );
    }

    #[test]
    fn clicking_empty_space_selects_nothing() {
        let mut ctx = FixedMetrics;
        let mut state = laid_out("alpha", 400.0, 200.0);
        let below = state.content_height + 50.0;
        assert!(!select_at(
            &mut ctx,
            &mut state,
            &test_style(),
            10.0,
            below,
            false
        ));
        assert!(!state.has_selection());
    }

    #[test]
    fn a_screen_point_maps_to_the_content_it_covers() {
        let mut state = laid_out("alpha", 400.0, 200.0);
        state.scroll_y = 30.0;
        state.content_rect = Rect {
            x: 100.0,
            y: 50.0,
            w: 400.0,
            h: 200.0,
        };
        let (cx, cy) = point_to_content(&state, 140.0, 60.0);
        assert!((cx - 40.0).abs() < 0.001);
        assert!((cy - 40.0).abs() < 0.001);
    }

    #[test]
    fn wrapping_paragraph_needs_no_horizontal_scroll() {
        let long = "word ".repeat(200);
        let state = laid_out(&long, 400.0, 300.0);
        assert_eq!(state.content_width, 400.0);
        assert!(horizontal_scrollbar(&state, &test_style()).is_none());
    }

    #[test]
    fn wide_code_block_extends_content_width() {
        let source = format!("```\n{}\n```\n", "x".repeat(200));
        let state = laid_out(&source, 400.0, 300.0);
        assert!(state.content_width > 400.0);
        let bar = horizontal_scrollbar(&state, &test_style()).expect("horizontal scrollbar");
        assert!(bar.thumb.w > 0.0 && bar.thumb.w <= bar.track.w);
    }

    #[test]
    fn unbreakable_table_cell_widens_the_table() {
        let narrow = "| a | b |\n| --- | --- |\n| c | d |\n";
        let wide = format!("| a | b |\n| --- | --- |\n| {} | d |\n", "x".repeat(120));
        assert_eq!(laid_out(narrow, 400.0, 300.0).content_width, 400.0);
        assert!(laid_out(&wide, 400.0, 300.0).content_width > 400.0);
    }

    #[test]
    fn scroll_keys_move_within_bounds() {
        let style = test_style();
        let mut state = laid_out(&"para\n\n".repeat(200), 400.0, 300.0);
        assert!(scroll_key(&mut state, "down", &style));
        assert_eq!(state.scroll_y, LINE_H);
        assert!(scroll_key(&mut state, "up", &style));
        assert_eq!(state.scroll_y, 0.0);
        // Already at the top: up cannot go negative.
        assert!(scroll_key(&mut state, "up", &style));
        assert_eq!(state.scroll_y, 0.0);
        assert!(scroll_key(&mut state, "pagedown", &style));
        assert_eq!(state.scroll_y, 300.0 - LINE_H);
        assert!(scroll_key(&mut state, "end", &style));
        assert_eq!(state.scroll_y, max_scroll_y(&state));
        assert!(scroll_key(&mut state, "home", &style));
        assert_eq!(state.scroll_y, 0.0);
        assert!(!scroll_key(&mut state, "a", &style));
    }

    #[test]
    fn horizontal_keys_stop_at_the_content_edge() {
        let style = test_style();
        let source = format!("```\n{}\n```\n", "x".repeat(200));
        let mut state = laid_out(&source, 400.0, 300.0);
        for _ in 0..500 {
            assert!(scroll_key(&mut state, "right", &style));
        }
        assert_eq!(state.scroll_x, max_scroll_x(&state));
        for _ in 0..500 {
            assert!(scroll_key(&mut state, "left", &style));
        }
        assert_eq!(state.scroll_x, 0.0);
    }

    #[test]
    fn dragging_a_thumb_to_the_track_end_scrolls_to_the_content_end() {
        let style = test_style();
        let mut state = laid_out(&"para\n\n".repeat(200), 400.0, 300.0);
        let bar = vertical_scrollbar(&state, &style).expect("vertical scrollbar");
        scroll_to_thumb_top(&mut state, &style, bar.track.y + bar.track.h);
        assert_eq!(state.scroll_y, max_scroll_y(&state));
        assert_eq!(state.target_scroll_y, state.scroll_y);
        scroll_to_thumb_top(&mut state, &style, bar.track.y - 50.0);
        assert_eq!(state.scroll_y, 0.0);
    }

    #[test]
    fn scrollbar_thumbs_track_the_scroll_position() {
        let style = test_style();
        let mut state = laid_out(&"para\n\n".repeat(200), 400.0, 300.0);
        let top = vertical_scrollbar(&state, &style).expect("vertical scrollbar");
        assert_eq!(top.thumb.y, top.track.y);
        scroll_by(&mut state, 0.0, f64::MAX);
        let bottom = vertical_scrollbar(&state, &style).expect("vertical scrollbar");
        assert_eq!(
            bottom.thumb.y + bottom.thumb.h,
            bottom.track.y + bottom.track.h
        );
    }

    #[test]
    fn horizontal_track_clears_the_vertical_scrollbar() {
        let style = test_style();
        let source = format!("```\n{}\n```\n{}", "x".repeat(200), "para\n\n".repeat(200));
        let state = laid_out(&source, 400.0, 300.0);
        assert!(vertical_scrollbar(&state, &style).is_some());
        let h = horizontal_scrollbar(&state, &style).expect("horizontal scrollbar");
        assert_eq!(h.track.w, 400.0 - style.scrollbar_size);
    }

    #[test]
    fn read_only_pane_swallows_typing_keys_but_not_shortcuts() {
        use crate::editor::event::Modifiers;
        let plain = Modifiers::default();
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert!(swallows_edit_key("backspace", &plain));
        assert!(swallows_edit_key("return", &plain));
        assert!(swallows_edit_key("tab", &shift));
        assert!(!swallows_edit_key("s", &ctrl));
        assert!(!swallows_edit_key("f5", &plain));
    }

    #[test]
    fn toggle_task_unchecked_to_checked() {
        let src = "- [ ] task\n";
        let (line, col, ch) = toggle_task_at(src, 0, false).unwrap();
        assert_eq!(line, 1);
        assert_eq!(ch, 'x');
        assert_eq!(&src[col - 1..col], " ");
    }

    #[test]
    fn toggle_task_checked_to_unchecked() {
        let src = "- [x] task\n";
        let (_line, col, ch) = toggle_task_at(src, 0, true).unwrap();
        assert_eq!(ch, ' ');
        assert_eq!(&src[col - 1..col], "x");
    }

    #[test]
    fn toggle_task_with_indent() {
        let src = "  - [ ] indented\n";
        let (line, _col, ch) = toggle_task_at(src, 0, false).unwrap();
        assert_eq!(line, 1);
        assert_eq!(ch, 'x');
    }

    #[test]
    fn toggle_task_capital_x() {
        let src = "- [X] task\n";
        let (_line, col, ch) = toggle_task_at(src, 0, true).unwrap();
        assert_eq!(ch, ' ');
        assert_eq!(&src[col - 1..col], "X");
    }

    #[test]
    fn code_block_line_count_counts_lines() {
        assert_eq!(code_block_line_count("a"), 1);
        assert_eq!(code_block_line_count("a\nb"), 2);
        assert_eq!(code_block_line_count("a\nb\nc"), 3);
        assert_eq!(code_block_line_count(""), 1);
    }

    /// Regression test for the `see [LICENSE](LICENSE).` bug: parse a
    /// markdown fragment where a punctuation span follows a link span
    /// with no whitespace between them, and verify the parser kept them
    /// as adjacent spans without trailing whitespace on the link. The
    /// ws-pending state machine in `draw_inlines` / `inlines_height`
    /// uses those flags to decide whether to insert a separator space.
    #[test]
    fn link_followed_by_period_has_no_trailing_ws_on_link() {
        let blocks = crate::editor::markdown::parse("see [LICENSE](LICENSE).\n");
        match &blocks[0] {
            crate::editor::markdown::Block::Paragraph { inlines } => {
                // Find the link span and the span after it.
                let link_idx = inlines
                    .iter()
                    .position(|s| s.href.is_some())
                    .expect("link span present");
                let link_span = &inlines[link_idx];
                // The link itself should not end in whitespace.
                assert!(!link_span.text.ends_with(char::is_whitespace));
                // The span just after the link should be the bare ".".
                let after = &inlines[link_idx + 1];
                assert_eq!(after.text, ".");
                assert!(after.href.is_none());
                assert!(!after.text.starts_with(char::is_whitespace));
            }
            _ => panic!("expected paragraph"),
        }
    }
}
