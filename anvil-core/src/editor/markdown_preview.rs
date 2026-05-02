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
    pub scroll_y: f64,
    pub target_scroll_y: f64,
    pub cached_change_id: i64,
    pub cached_width: f64,
    pub link_regions: Vec<LinkRegion>,
    pub checkbox_regions: Vec<CheckboxRegion>,
    /// Rectangle this preview occupies. Refreshed each frame by the layout
    /// pass so hit-tests in the main loop know which pane a click is in.
    pub rect: Rect,
    /// Parallel to `blocks`: pre-tokenized code-block lines (one entry per
    /// line) when the block's fence lang resolves to a bundled syntax; None
    /// otherwise. Populated by the main loop after each reparse so draws
    /// don't pay the tokenize cost every frame.
    pub code_tokens: Vec<Option<Vec<Vec<crate::editor::tokenizer::Token>>>>,
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
        let words: Vec<&str> = span.text.split_whitespace().collect();
        if words.is_empty() {
            if !span.text.is_empty() {
                ws_pending = true;
            }
            continue;
        }
        let leads_ws = span.text.starts_with(char::is_whitespace);
        let trails_ws = span.text.ends_with(char::is_whitespace);
        for (i, word) in words.iter().enumerate() {
            let ww = ctx.font_width(font, word);
            let needs_space = if i == 0 {
                last && (ws_pending || leads_ws)
            } else {
                true
            };
            if needs_space {
                if x + sw + ww > width {
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
        ws_pending = trails_ws;
    }
    lines * base_lh
}

fn code_block_line_count(text: &str) -> usize {
    let with_newline = format!("{text}\n");
    1.max(with_newline.matches('\n').count())
}

/// Height of one block at `width` pixels. Callers add inter-block `GAP`.
fn block_height(ctx: &dyn DrawContext, blk: &Block, width: f64, style: &StyleContext) -> f64 {
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
                h += block_height(ctx, sub, inner_w, style);
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
            let col_w = (width / n_cols as f64).floor();
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
        let h = block_height(ctx, blk, inner, style);
        layout.push(LayoutEntry { y, h });
        y += h + GAP;
    }
    state.layout = layout;
    state.content_height = y + PAD;
    state.cached_width = width;
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
    link_regions: &mut Vec<LinkRegion>,
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
            continue;
        }
        let font = if span.code { code } else { base_font };
        let col = forced_color.unwrap_or_else(|| span_color(span, style));
        let sw = ctx.font_width(font, " ");
        let words: Vec<&str> = span.text.split_whitespace().collect();
        if words.is_empty() {
            if !span.text.is_empty() {
                ws_pending = true;
            }
            continue;
        }
        let leads_ws = span.text.starts_with(char::is_whitespace);
        let trails_ws = span.text.ends_with(char::is_whitespace);
        for (i, word) in words.iter().enumerate() {
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
                link_regions.push(LinkRegion {
                    x1: wx0,
                    y1: y,
                    x2: x,
                    y2: y + base_lh,
                    href: href.clone(),
                });
            }
            last = true;
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
    code_tokens: Option<&Vec<Vec<crate::editor::tokenizer::Token>>>,
    link_regions: &mut Vec<LinkRegion>,
    checkbox_regions: &mut Vec<CheckboxRegion>,
) {
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
                link_regions,
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
                ctx,
                inlines,
                x,
                y,
                max_x,
                body,
                lh,
                None,
                style,
                link_regions,
                false,
            );
        }
        Block::Code { text, .. } => {
            // Match the pad used by `block_height` exactly. Pad scales
            // with body line height so small/large themes stay balanced.
            let pad = (lh * 0.75).ceil();
            let line_count = code_block_line_count(text);
            let total_h = line_count as f64 * clh + pad * 2.0;
            // Panel background + a thin left accent bar.
            ctx.draw_rect(x, y, max_x - x, total_h, style.background2.to_array());
            ctx.draw_rect(x, y, 3.0, total_h, style.accent.to_array());
            let mut cy = y + pad;
            let text_x = x + pad + 3.0;
            if let Some(lines) = code_tokens {
                // Tokenized path: colour each run using the active theme's
                // syntax palette so ```lang fences read like the editor does.
                for (line_idx, line) in text.split('\n').enumerate() {
                    if let Some(tokens) = lines.get(line_idx) {
                        let mut tx = text_x;
                        for tok in tokens {
                            let color =
                                crate::editor::doc_view::syntax_color(&tok.token_type, style);
                            tx = ctx.draw_text(style.code_font, &tok.text, tx, cy, color);
                        }
                    } else {
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
                    None,
                    link_regions,
                    checkbox_regions,
                );
                cur_y += block_height(ctx, sub, max_x - inner_x, style);
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
            ctx,
            items,
            *ordered,
            *start,
            x,
            y,
            max_x,
            style,
            link_regions,
            checkbox_regions,
        ),
        Block::Table {
            alignments,
            head,
            rows,
        } => draw_table(
            ctx,
            alignments,
            head,
            rows,
            x,
            y,
            max_x,
            style,
            link_regions,
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
    link_regions: &mut Vec<LinkRegion>,
    checkbox_regions: &mut Vec<CheckboxRegion>,
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
                checkbox_regions.push(CheckboxRegion {
                    x1: box_x,
                    y1: box_y,
                    x2: box_x + box_size,
                    y2: box_y + box_size,
                    source_start: src,
                    checked,
                });
            }
        } else if ordered {
            let bullet = format!("{}.", start_num + i as u64);
            ctx.draw_text(body, &bullet, x + LIST_MARKER_INSET, cur_y, dim_color);
        } else {
            ctx.draw_text(body, "\u{2022}", x + LIST_MARKER_INSET, cur_y, dim_color);
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
            link_regions,
            item_checked,
        );
        cur_y += ih.max(lh);
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
    link_regions: &mut Vec<LinkRegion>,
) {
    let n_cols = alignments.len().max(head.len()).max(1);
    let lh = style.font_height;
    let body = style.font;
    let total_w = max_x - x;
    let col_w = (total_w / n_cols as f64).floor();
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
            link_regions,
        );
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
            link_regions,
        );
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
    link_regions: &mut Vec<LinkRegion>,
) {
    for i in 0..n_cols {
        let cx = x + col_w * i as f64;
        if let Some(cell) = cells.get(i) {
            // Clip so long content can't spill into the next column.
            ctx.set_clip_rect(
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
                link_regions,
                false,
            );
        }
    }
    // Note: the per-cell clip is not reset here. The outer `draw` loop
    // re-applies the preview pane clip after every block, so any narrow
    // clip left behind by this function is harmless — the next block
    // always starts fresh.
}

// ── Top-level draw + URL helpers ─────────────────────────────────────────

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

    state.link_regions.clear();
    state.checkbox_regions.clear();

    let inner_x = px + PAD;
    let inner_max_x = px + pw - PAD;
    // Snap scroll to a whole pixel before computing block positions. The
    // lerp used by the main loop produces fractional scroll values and
    // `draw_text` truncates to i32 — without snapping, glyphs can sit
    // half a pixel above the background clear, leaving ghost rows below
    // the last legitimate line.
    let scroll_y_snap = state.scroll_y.floor();
    let base_y = py - scroll_y_snap;

    // Clip content to the preview rect.
    ctx.set_clip_rect(px, py, pw, ph);
    for (i, blk) in state.blocks.iter().enumerate() {
        let Some(entry) = state.layout.get(i) else {
            continue;
        };
        let sy = base_y + entry.y;
        if sy + entry.h < py {
            continue;
        }
        if sy > py + ph {
            break;
        }
        let tokens = state.code_tokens.get(i).and_then(|o| o.as_ref());
        draw_block(
            ctx,
            blk,
            inner_x,
            sy,
            inner_max_x,
            style,
            tokens,
            &mut state.link_regions,
            &mut state.checkbox_regions,
        );
        // Re-apply the preview clip after every block. `draw_block` may
        // leave the clip narrowed (tables set per-cell clips for spill
        // protection), and without this the next block would render into
        // a tiny stale rect and silently disappear. This is specifically
        // what was cutting off content after the first table in README.md.
        ctx.set_clip_rect(px, py, pw, ph);
    }
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
