use mlua::prelude::*;

use std::sync::Arc;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn font_get_height(font: &LuaValue) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_height", ()),
        LuaValue::UserData(ud) => ud.call_method("get_height", ()),
        _ => Ok(14.0),
    }
}

fn font_get_width(font: &LuaValue, text: &str) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_width", text.to_string()),
        LuaValue::UserData(ud) => ud.call_method("get_width", text.to_string()),
        _ => Ok(0.0),
    }
}

fn font_get_size(font: &LuaValue) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_size", ()),
        LuaValue::UserData(ud) => ud.call_method("get_size", ()),
        _ => Ok(14.0),
    }
}

fn font_copy(font: &LuaValue, size: f64) -> LuaResult<LuaValue> {
    match font {
        LuaValue::Table(t) => t.call_method("copy", size),
        LuaValue::UserData(ud) => ud.call_method("copy", size),
        _ => Ok(LuaValue::Nil),
    }
}

// ---- Layout module ----

const HEAD_SCALE: [f64; 6] = [2.0, 1.6, 1.3, 1.1, 1.0, 0.9];

fn quote_padding(gap: f64) -> f64 {
    10.0f64.max(gap)
}

fn quote_trailing_padding(gap: f64) -> f64 {
    14.0f64.max(gap * 2.0)
}

fn quote_block_gap(gap: f64) -> f64 {
    10.0f64.max(gap)
}

fn list_item_gap(gap: f64) -> f64 {
    2.0f64.max((gap * 0.5).floor())
}

fn code_block_line_count(text: &str) -> usize {
    let with_newline = format!("{text}\n");
    let lines = with_newline.matches('\n').count();
    1.max(lines)
}

fn inlines_height(_lua: &Lua, inlines: &LuaTable, width: f64, fonts: &LuaTable) -> LuaResult<f64> {
    if inlines.raw_len() == 0 {
        return Ok(0.0);
    }
    let body: LuaValue = fonts.get("body")?;
    let lh = font_get_height(&body)?;
    let code_font: LuaValue = fonts.get("code")?;
    let mut x = 0.0;
    let mut lines = 1.0;
    let mut last = false;

    for pair in inlines.sequence_values::<LuaTable>() {
        let span = pair?;
        let text: String = span.get("text")?;
        if text == "\n" {
            x = 0.0;
            lines += 1.0;
            last = false;
        } else {
            let is_code: bool = span.get("code").unwrap_or(false);
            let font = if is_code { &code_font } else { &body };
            let sw = font_get_width(font, " ")?;
            for word in text.split_whitespace() {
                let ww = font_get_width(font, word)?;
                if last {
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
        }
    }
    Ok(lines * lh)
}

fn block_height(lua: &Lua, blk: &LuaTable, width: f64, fonts: &LuaTable, gap: f64) -> LuaResult<f64> {
    let body: LuaValue = fonts.get("body")?;
    let code_font: LuaValue = fonts.get("code")?;
    let lh = font_get_height(&body)?;
    let clh = font_get_height(&code_font)?;
    let blk_type: String = blk.get("type")?;

    match blk_type.as_str() {
        "rule" => Ok((lh / 2.0).floor()),
        "heading" => {
            let level: i64 = blk.get("level")?;
            let hf_key = format!("h{level}");
            let hf: LuaValue = fonts.get(hf_key.as_str())?;
            let hf_ref = if matches!(hf, LuaValue::Nil) { &body } else { &hf };
            let hfh = font_get_height(hf_ref)?;
            Ok(hfh + gap)
        }
        "paragraph" => {
            let il: LuaTable = blk.get("inlines")?;
            inlines_height(lua, &il, width, fonts)
        }
        "code_block" => {
            let text: String = blk.get("text")?;
            Ok(code_block_line_count(&text) as f64 * clh + gap * 2.0)
        }
        "blockquote" => {
            let pad = quote_padding(gap);
            let bg = quote_block_gap(gap);
            let trailing = quote_trailing_padding(gap);
            let blocks: LuaTable = blk.get("blocks")?;
            let mut h = pad;
            for pair in blocks.sequence_values::<LuaTable>() {
                let sub = pair?;
                h += block_height(lua, &sub, width - 14.0, fonts, gap)? + bg;
            }
            h += trailing;
            Ok(h.max(lh))
        }
        "list" => {
            let ig = list_item_gap(gap);
            let items: LuaTable = blk.get("items")?;
            let mut h = 0.0;
            for pair in items.sequence_values::<LuaTable>() {
                let item = pair?;
                h += inlines_height(lua, &item, width - 20.0, fonts)? + ig;
            }
            Ok(h.max(lh))
        }
        "table" => {
            let head: LuaTable = blk.get("head")?;
            let rows: LuaTable = blk.get("rows")?;
            let head_len = head.raw_len();
            let n = if head_len > 0 { 1 } else { 0 } + rows.raw_len();
            let extra = if head_len > 0 { 3.0 } else { 0.0 };
            Ok(n as f64 * (lh + gap + 1.0) + extra + gap)
        }
        _ => Ok(lh),
    }
}

fn compute_layout(lua: &Lua, view: &LuaTable, fonts: &LuaTable, pad: f64, gap: f64) -> LuaResult<()> {
    let size: LuaTable = view.get("size")?;
    let size_x: f64 = size.get("x")?;
    let width = size_x - pad * 2.0;
    let blocks: LuaTable = view.get("blocks")?;
    let layout = lua.create_table()?;
    let mut y = pad;
    for i in 1..=blocks.raw_len() as i64 {
        let blk: LuaTable = blocks.get(i)?;
        let h = block_height(lua, &blk, width, fonts, gap)?;
        let entry = lua.create_table()?;
        entry.set("y", y)?;
        entry.set("h", h)?;
        layout.set(i, entry)?;
        y += h + gap;
    }
    view.set("layout", layout)?;
    view.set("content_height", y + pad)?;
    Ok(())
}

// ---- Renderers module ----

fn span_color(lua: &Lua, span: &LuaTable) -> LuaResult<LuaValue> {
    let style = require_table(lua, "core.style")?;
    let href: LuaValue = span.get("href")?;
    if !matches!(href, LuaValue::Nil | LuaValue::Boolean(false)) {
        // Link color: {88, 166, 255, 255}
        let c = lua.create_table()?;
        c.push(88)?;
        c.push(166)?;
        c.push(255)?;
        c.push(255)?;
        return Ok(LuaValue::Table(c));
    }
    let is_code: bool = span.get("code").unwrap_or(false);
    if is_code {
        let syntax: LuaTable = style.get("syntax")?;
        return syntax.get("string");
    }
    let is_italic: bool = span.get("italic").unwrap_or(false);
    if is_italic {
        let syntax: LuaTable = style.get("syntax")?;
        return syntax.get("comment");
    }
    let is_bold: bool = span.get("bold").unwrap_or(false);
    if is_bold {
        let syntax: LuaTable = style.get("syntax")?;
        return syntax.get("keyword");
    }
    let is_strike: bool = span.get("strikethrough").unwrap_or(false);
    if is_strike {
        return style.get("dim");
    }
    style.get("text")
}

#[allow(clippy::too_many_arguments)]
// Mirrors the Lua draw_inlines(view, inlines, x0, y0, max_x, fonts, base_font, forced_color).
fn draw_inlines(
    lua: &Lua,
    view: &LuaTable,
    inlines: &LuaTable,
    x0: f64,
    y0: f64,
    max_x: f64,
    fonts: &LuaTable,
    base_font: Option<&LuaValue>,
    forced_color: Option<&LuaValue>,
) -> LuaResult<f64> {
    if inlines.raw_len() == 0 {
        return Ok(y0);
    }
    let body: LuaValue = fonts.get("body")?;
    let base = base_font.unwrap_or(&body);
    let lh = font_get_height(base)?;
    let code_font: LuaValue = fonts.get("code")?;
    let renderer: LuaTable = lua.globals().get("renderer")?;
    let link_regions: LuaTable = view.get("link_regions")?;

    let mut x = x0;
    let mut y = y0;
    let mut last = false;

    for pair in inlines.sequence_values::<LuaTable>() {
        let span = pair?;
        let text: String = span.get("text")?;
        if text == "\n" {
            x = x0;
            y += lh;
            last = false;
        } else {
            let is_code: bool = span.get("code").unwrap_or(false);
            let font = if is_code { &code_font } else { base };
            let col = match forced_color {
                Some(c) => c.clone(),
                None => span_color(lua, &span)?,
            };
            let sw = font_get_width(font, " ")?;

            for word in text.split_whitespace() {
                let ww = font_get_width(font, word)?;
                if last {
                    if x + sw + ww > max_x && x > x0 {
                        x = x0;
                        y += lh;
                    } else {
                        x += sw;
                    }
                } else if x + ww > max_x && x > x0 {
                    x = x0;
                    y += lh;
                }

                let wx0 = x;
                x = renderer.call_function("draw_text", (font.clone(), word, x, y, col.clone()))?;
                let href: LuaValue = span.get("href")?;
                if !matches!(href, LuaValue::Nil | LuaValue::Boolean(false)) {
                    let region = lua.create_table()?;
                    region.set("x1", wx0)?;
                    region.set("y1", y)?;
                    region.set("x2", x)?;
                    region.set("y2", y + lh)?;
                    region.set("href", href)?;
                    link_regions.push(region)?;
                }
                last = true;
            }
        }
    }
    Ok(y + lh)
}

#[allow(clippy::too_many_arguments)]
// Mirrors the Lua draw_block(view, blk, x, y, max_x, fonts, lh, gap).
fn draw_block(
    lua: &Lua,
    view: &LuaTable,
    blk: &LuaTable,
    x: f64,
    y: f64,
    max_x: f64,
    fonts: &LuaTable,
    lh: f64,
    gap: f64,
) -> LuaResult<()> {
    let style = require_table(lua, "core.style")?;
    let core = require_table(lua, "core")?;
    let renderer: LuaTable = lua.globals().get("renderer")?;
    let blk_type: String = blk.get("type")?;

    match blk_type.as_str() {
        "heading" => {
            let level: i64 = blk.get("level")?;
            let hf_key = format!("h{level}");
            let hf: LuaValue = fonts.get(hf_key.as_str())?;
            let body: LuaValue = fonts.get("body")?;
            let hf_ref = if matches!(hf, LuaValue::Nil) { &body } else { &hf };
            let syntax: LuaTable = style.get("syntax")?;
            let kw_color: LuaValue = syntax.get("keyword")?;
            let il: LuaTable = blk.get("inlines")?;
            draw_inlines(lua, view, &il, x, y, max_x, fonts, Some(hf_ref), Some(&kw_color))?;
        }
        "paragraph" => {
            let il: LuaTable = blk.get("inlines")?;
            draw_inlines(lua, view, &il, x, y, max_x, fonts, None, None)?;
        }
        "code_block" => {
            let text: String = blk.get("text")?;
            let entry_h = block_height(lua, blk, max_x - x, fonts, gap)?;
            let line_hl: LuaValue = style.get("line_highlight")?;
            renderer.call_function::<()>(
                "draw_rect",
                (x - 4.0, y, max_x - x + 8.0, entry_h, line_hl),
            )?;
            let code_font: LuaValue = fonts.get("code")?;
            let clh = font_get_height(&code_font)?;
            let syntax_tbl: LuaTable = style.get("syntax")?;
            let str_color: LuaValue = syntax_tbl.get("string")?;
            let mut cy = y + (gap / 2.0).floor();
            let with_newline = format!("{text}\n");
            for line in with_newline.split('\n') {
                if line.is_empty() && cy > y + entry_h {
                    break;
                }
                core.call_function::<()>("push_clip_rect", (x, cy, max_x - x, clh))?;
                renderer.call_function::<()>(
                    "draw_text",
                    (code_font.clone(), line, x, cy, str_color.clone()),
                )?;
                core.call_function::<()>("pop_clip_rect", ())?;
                cy += clh;
            }
        }
        "rule" => {
            let mid = (y + lh / 4.0).floor();
            let divider: LuaValue = style.get("divider")?;
            renderer.call_function::<()>(
                "draw_rect",
                (x, mid, max_x - x, 1.0, divider),
            )?;
        }
        "blockquote" => {
            let pad = quote_padding(gap);
            let bg = quote_block_gap(gap);
            let trailing = quote_trailing_padding(gap);
            let sx = x + 14.0;
            let start_y = y;
            let mut cur_y = y + pad;
            let blocks: LuaTable = blk.get("blocks")?;
            for pair in blocks.sequence_values::<LuaTable>() {
                let sub = pair?;
                let sh = block_height(lua, &sub, max_x - sx, fonts, gap)?;
                draw_block(lua, view, &sub, sx, cur_y, max_x, fonts, lh, gap)?;
                cur_y += sh + bg;
            }
            let syntax_tbl: LuaTable = style.get("syntax")?;
            let comment_color: LuaValue = syntax_tbl.get("comment")?;
            renderer.call_function::<()>(
                "draw_rect",
                (x, start_y, 3.0, cur_y - start_y + trailing, comment_color),
            )?;
        }
        "list" => {
            let items: LuaTable = blk.get("items")?;
            let ordered: bool = blk.get("ordered").unwrap_or(false);
            let start_num: i64 = blk.get("start").unwrap_or(1);
            let body: LuaValue = fonts.get("body")?;
            let text_color: LuaValue = style.get("text")?;
            let ig = list_item_gap(gap);
            let cx = x + 20.0;
            let mut cur_y = y;
            for i in 1..=items.raw_len() as i64 {
                let item: LuaTable = items.get(i)?;
                let bullet = if ordered {
                    format!("{}.", start_num + i - 1)
                } else {
                    "\u{2022}".to_string()
                };
                renderer.call_function::<()>(
                    "draw_text",
                    (body.clone(), bullet, x + 4.0, cur_y, text_color.clone()),
                )?;
                let ih = inlines_height(lua, &item, max_x - cx, fonts)?;
                core.call_function::<()>("push_clip_rect", (cx, cur_y, max_x - cx, ih + ig))?;
                draw_inlines(lua, view, &item, cx, cur_y, max_x, fonts, None, None)?;
                core.call_function::<()>("pop_clip_rect", ())?;
                cur_y += ih + ig;
            }
        }
        "table" => {
            let alignments: LuaTable = blk.get("alignments")?;
            let n_cols = alignments.raw_len() as i64;
            if n_cols == 0 {
                return Ok(());
            }
            let head: LuaTable = blk.get("head")?;
            let rows: LuaTable = blk.get("rows")?;
            let total_w = max_x - x;
            let col_w = (total_w / n_cols as f64).floor();
            let row_h = lh + gap;
            let tpad = 6.0;
            let divider: LuaValue = style.get("divider")?;
            let line_hl: LuaValue = style.get("line_highlight")?;
            let syntax_tbl: LuaTable = style.get("syntax")?;
            let kw_color: LuaValue = syntax_tbl.get("keyword")?;
            let text_color: LuaValue = style.get("text")?;

            let draw_row = |cells: &LuaTable, ry: f64, is_header: bool| -> LuaResult<()> {
                let mut cx = x;
                for col_i in 1..=n_cols {
                    if is_header {
                        renderer.call_function::<()>(
                            "draw_rect",
                            (cx, ry, col_w, row_h, line_hl.clone()),
                        )?;
                    }
                    let cell: LuaValue = cells.get(col_i)?;
                    if let LuaValue::Table(ref cell_t) = cell {
                        core.call_function::<()>(
                            "push_clip_rect",
                            (cx + tpad, ry, col_w - tpad * 2.0, row_h),
                        )?;
                        let col = if is_header {
                            kw_color.clone()
                        } else {
                            text_color.clone()
                        };
                        draw_inlines(
                            lua,
                            view,
                            cell_t,
                            cx + tpad,
                            ry + (gap / 2.0).floor(),
                            cx + col_w - tpad,
                            fonts,
                            None,
                            Some(&col),
                        )?;
                        core.call_function::<()>("pop_clip_rect", ())?;
                    }
                    renderer.call_function::<()>(
                        "draw_rect",
                        (cx + col_w, ry, 1.0, row_h, divider.clone()),
                    )?;
                    cx += col_w;
                }
                renderer.call_function::<()>(
                    "draw_rect",
                    (x, ry + row_h, total_w, 1.0, divider.clone()),
                )?;
                Ok(())
            };

            renderer.call_function::<()>("draw_rect", (x, y, total_w, 1.0, divider.clone()))?;
            let mut cur_y = y + 1.0;
            if head.raw_len() > 0 {
                draw_row(&head, cur_y, true)?;
                cur_y += row_h + 1.0;
                renderer.call_function::<()>(
                    "draw_rect",
                    (x, cur_y, total_w, 2.0, divider.clone()),
                )?;
                cur_y += 2.0;
            }
            for pair in rows.sequence_values::<LuaTable>() {
                let row = pair?;
                draw_row(&row, cur_y, false)?;
                cur_y += row_h + 1.0;
            }
        }
        _ => {}
    }
    Ok(())
}

// ---- Main plugin ----

fn build_markdown_view(lua: &Lua) -> LuaResult<(LuaTable, Arc<LuaRegistryKey>)> {
    let view_class = require_table(lua, "core.view")?;
    let md_view = view_class.call_method::<LuaTable>("extend", ())?;

    md_view.set(
        "__tostring",
        lua.create_function(|_, _: LuaTable| Ok("MarkdownView"))?,
    )?;

    let class_key = Arc::new(lua.create_registry_value(md_view.clone())?);

    // Font cache stored on class table
    md_view.set("_fonts_cache", LuaValue::Nil)?;
    md_view.set("_fonts_base_size", LuaValue::Nil)?;

    // get_fonts helper
    let ck = Arc::clone(&class_key);
    let get_fonts = lua.create_function(move |lua, ()| {
        let style = require_table(lua, "core.style")?;
        let font: LuaValue = style.get("font")?;
        let sz = font_get_size(&font)?;
        let class: LuaTable = lua.registry_value(&ck)?;
        let base_size: LuaValue = class.get("_fonts_base_size")?;
        let cached: LuaValue = class.get("_fonts_cache")?;
        let needs_rebuild = matches!(cached, LuaValue::Nil) || match &base_size {
            LuaValue::Number(n) => (*n - sz).abs() > 0.01,
            _ => true,
        };
        if needs_rebuild {
            let cache = lua.create_table()?;
            cache.set("body", font.clone())?;
            let code: LuaValue = style.get("code_font")?;
            cache.set("code", code)?;
            for (i, scale) in HEAD_SCALE.iter().enumerate() {
                let size = (sz * scale + 0.5).floor();
                let hf = font_copy(&font, size)?;
                cache.set(format!("h{}", i + 1).as_str(), hf)?;
            }
            class.set("_fonts_cache", cache.clone())?;
            class.set("_fonts_base_size", sz)?;
            Ok(cache)
        } else {
            match cached {
                LuaValue::Table(t) => Ok(t),
                _ => {
                    // Should not happen but fallback
                    let cache = lua.create_table()?;
                    cache.set("body", font)?;
                    Ok(cache)
                }
            }
        }
    })?;
    let gf_key = Arc::new(lua.create_registry_value(get_fonts)?);

    // new(self, doc)
    {
        let ck = Arc::clone(&class_key);
        md_view.set(
            "new",
            lua.create_function(move |lua, (this, doc): (LuaTable, LuaTable)| {
                let class: LuaTable = lua.registry_value(&ck)?;
                let view_cls: LuaTable = class.get("super")?;
                view_cls.call_method::<()>("new", this.clone())?;
                this.set("doc", doc)?;
                this.set("scrollable", true)?;
                this.set("cursor", "arrow")?;
                this.set("blocks", LuaValue::Nil)?;
                this.set("layout", LuaValue::Nil)?;
                this.set("content_height", 0.0)?;
                this.set("link_regions", lua.create_table()?)?;
                this.set("last_change_id", LuaValue::Nil)?;
                this.set("last_layout_width", LuaValue::Nil)?;
                Ok(())
            })?,
        )?;
    }

    // get_name
    md_view.set(
        "get_name",
        lua.create_function(|lua, this: LuaTable| {
            let doc: LuaTable = this.get("doc")?;
            let common = require_table(lua, "core.common")?;
            let filename: LuaValue = doc.get("filename")?;
            let base: String = match &filename {
                LuaValue::String(s) => common.call_function("basename", s.to_str()?)?,
                _ => "Untitled".to_string(),
            };
            Ok(format!("Preview: {base}"))
        })?,
    )?;

    // get_scrollable_size
    md_view.set(
        "get_scrollable_size",
        lua.create_function(|_, this: LuaTable| {
            let h: f64 = this.get("content_height")?;
            Ok(h)
        })?,
    )?;

    // on_scale_change
    {
        let ck = Arc::clone(&class_key);
        md_view.set(
            "on_scale_change",
            lua.create_function(move |lua, this: LuaTable| {
                let class: LuaTable = lua.registry_value(&ck)?;
                class.set("_fonts_cache", LuaValue::Nil)?;
                this.set("layout", LuaValue::Nil)?;
                Ok(())
            })?,
        )?;
    }

    // update
    {
        let ck = Arc::clone(&class_key);
        let gfk = Arc::clone(&gf_key);
        md_view.set(
            "update",
            lua.create_function(move |lua, this: LuaTable| {
                let class: LuaTable = lua.registry_value(&ck)?;
                let super_cls: LuaTable = class.get("super")?;
                let super_update: LuaFunction = super_cls.call_method("__index", "update")?;
                super_update.call::<()>(this.clone())?;

                let doc: LuaTable = this.get("doc")?;
                let change_id: LuaValue = doc.call_method("get_change_id", ())?;
                let last_id: LuaValue = this.get("last_change_id")?;
                let changed = !lua_values_equal(&change_id, &last_id);
                if changed {
                    this.set("last_change_id", change_id)?;
                    let text: String = doc.call_method(
                        "get_text",
                        (1, 1, f64::INFINITY, f64::INFINITY),
                    )?;
                    let markdown = require_table(lua, "markdown")?;
                    let blocks: LuaTable = markdown.call_function("parse", text)?;
                    this.set("blocks", blocks)?;
                    this.set("layout", LuaValue::Nil)?;
                }

                let blocks: LuaValue = this.get("blocks")?;
                if let LuaValue::Table(ref _b) = blocks {
                    let layout: LuaValue = this.get("layout")?;
                    let last_w: LuaValue = this.get("last_layout_width")?;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    let needs_layout = matches!(layout, LuaValue::Nil)
                        || !lua_values_equal(
                            &last_w,
                            &LuaValue::Number(size_x),
                        );
                    if needs_layout {
                        this.set("last_layout_width", size_x)?;
                        let gf: LuaFunction = lua.registry_value(&gfk)?;
                        let fonts: LuaTable = gf.call(())?;
                        let config = require_table(lua, "core.config")?;
                        let plugins: LuaTable = config.get("plugins")?;
                        let mp_cfg: LuaTable = plugins.get("markdown_preview")?;
                        let scale: f64 = lua.globals().get("SCALE")?;
                        let pad_cfg: f64 = mp_cfg.get("padding")?;
                        let gap_cfg: f64 = mp_cfg.get("block_gap")?;
                        let pad = (pad_cfg * scale).floor();
                        let gap = (gap_cfg * scale).floor();
                        compute_layout(lua, &this, &fonts, pad, gap)?;
                    }
                }
                Ok(())
            })?,
        )?;
    }

    // draw
    {
        let gfk = Arc::clone(&gf_key);
        md_view.set(
            "draw",
            lua.create_function(move |lua, this: LuaTable| {
                let layout: LuaValue = this.get("layout")?;
                if matches!(layout, LuaValue::Nil) {
                    return Ok(());
                }
                let layout = layout.as_table().unwrap();
                let style = require_table(lua, "core.style")?;
                let core = require_table(lua, "core")?;
                let config = require_table(lua, "core.config")?;

                let bg: LuaValue = style.get("background")?;
                this.call_method::<()>("draw_background", bg)?;
                this.set("link_regions", lua.create_table()?)?;

                let gf: LuaFunction = lua.registry_value(&gfk)?;
                let fonts: LuaTable = gf.call(())?;
                let body: LuaValue = fonts.get("body")?;
                let lh = font_get_height(&body)?;

                let plugins: LuaTable = config.get("plugins")?;
                let mp_cfg: LuaTable = plugins.get("markdown_preview")?;
                let scale: f64 = lua.globals().get("SCALE")?;
                let pad_cfg: f64 = mp_cfg.get("padding")?;
                let gap_cfg: f64 = mp_cfg.get("block_gap")?;
                let pad = (pad_cfg * scale).floor();
                let gap = (gap_cfg * scale).floor();

                let position: LuaTable = this.get("position")?;
                let pos_x: f64 = position.get("x")?;
                let pos_y: f64 = position.get("y")?;
                let size: LuaTable = this.get("size")?;
                let size_x: f64 = size.get("x")?;
                let size_y: f64 = size.get("y")?;

                let x = pos_x + pad;
                let max_x = pos_x + size_x - pad;
                let scroll: LuaTable = this.get("scroll")?;
                let scroll_y: f64 = scroll.get("y")?;
                let base_y = pos_y - scroll_y;

                let blocks: LuaTable = this.get("blocks")?;
                core.call_function::<()>(
                    "push_clip_rect",
                    (pos_x, pos_y, size_x, size_y),
                )?;
                for i in 1..=blocks.raw_len() as i64 {
                    let blk: LuaTable = blocks.get(i)?;
                    let entry: LuaTable = layout.get(i)?;
                    let ey: f64 = entry.get("y")?;
                    let eh: f64 = entry.get("h")?;
                    let sy = base_y + ey;
                    if sy + eh < pos_y {
                        continue;
                    }
                    if sy > pos_y + size_y {
                        break;
                    }
                    draw_block(lua, &this, &blk, x, sy, max_x, &fonts, lh, gap)?;
                }
                core.call_function::<()>("pop_clip_rect", ())?;
                this.call_method::<()>("draw_scrollbar", ())?;
                Ok(())
            })?,
        )?;
    }

    // on_mouse_pressed
    {
        let ck = Arc::clone(&class_key);
        md_view.set(
            "on_mouse_pressed",
            lua.create_function(move |lua, (this, button, x, y, clicks): (LuaTable, String, f64, f64, i64)| {
                let class: LuaTable = lua.registry_value(&ck)?;
                let super_cls: LuaTable = class.get("super")?;
                let super_omp: LuaFunction = super_cls.call_method("__index", "on_mouse_pressed")?;
                let caught: bool = super_omp.call((this.clone(), button.as_str(), x, y, clicks)).unwrap_or(false);
                if caught {
                    return Ok(LuaValue::Boolean(true));
                }
                if button != "left" {
                    return Ok(LuaValue::Nil);
                }
                let link_regions: LuaTable = this.get("link_regions")?;
                for pair in link_regions.sequence_values::<LuaTable>() {
                    let r = pair?;
                    let x1: f64 = r.get("x1")?;
                    let x2: f64 = r.get("x2")?;
                    let y1: f64 = r.get("y1")?;
                    let y2: f64 = r.get("y2")?;
                    if x >= x1 && x <= x2 && y >= y1 && y <= y2 {
                        let href: String = r.get("href")?;
                        open_url(lua, &href)?;
                        return Ok(LuaValue::Boolean(true));
                    }
                }
                Ok(LuaValue::Nil)
            })?,
        )?;
    }

    // on_mouse_moved
    {
        let ck = Arc::clone(&class_key);
        md_view.set(
            "on_mouse_moved",
            lua.create_function(move |lua, (this, x, y, dx, dy): (LuaTable, f64, f64, f64, f64)| {
                let class: LuaTable = lua.registry_value(&ck)?;
                let super_cls: LuaTable = class.get("super")?;
                let super_omm: LuaFunction = super_cls.call_method("__index", "on_mouse_moved")?;
                super_omm.call::<()>((this.clone(), x, y, dx, dy))?;
                let link_regions: LuaTable = this.get("link_regions")?;
                for pair in link_regions.sequence_values::<LuaTable>() {
                    let r = pair?;
                    let x1: f64 = r.get("x1")?;
                    let x2: f64 = r.get("x2")?;
                    let y1: f64 = r.get("y1")?;
                    let y2: f64 = r.get("y2")?;
                    if x >= x1 && x <= x2 && y >= y1 && y <= y2 {
                        this.set("cursor", "hand")?;
                        return Ok(());
                    }
                }
                this.set("cursor", "arrow")?;
                Ok(())
            })?,
        )?;
    }

    // on_mouse_left
    {
        let ck = Arc::clone(&class_key);
        md_view.set(
            "on_mouse_left",
            lua.create_function(move |lua, this: LuaTable| {
                let class: LuaTable = lua.registry_value(&ck)?;
                let super_cls: LuaTable = class.get("super")?;
                let super_oml: LuaFunction = super_cls.call_method("__index", "on_mouse_left")?;
                super_oml.call::<()>(this.clone())?;
                this.set("cursor", "arrow")
            })?,
        )?;
    }

    // on_mouse_wheel
    {
        let gfk = Arc::clone(&gf_key);
        md_view.set(
            "on_mouse_wheel",
            lua.create_function(move |lua, (this, dy, _dx): (LuaTable, f64, f64)| {
                let gf: LuaFunction = lua.registry_value(&gfk)?;
                let fonts: LuaTable = gf.call(())?;
                let body: LuaValue = fonts.get("body")?;
                let lh = font_get_height(&body)?;
                let scroll: LuaTable = this.get("scroll")?;
                let to: LuaTable = scroll.get("to")?;
                let cur_y: f64 = to.get("y")?;
                to.set("y", cur_y - dy * lh * 3.0)?;
                Ok(true)
            })?,
        )?;
    }

    Ok((md_view, class_key))
}

fn open_url(lua: &Lua, href: &str) -> LuaResult<()> {
    let escaped = href.replace('\'', "'\\''");
    let system: LuaTable = lua.globals().get("system")?;
    let has_open: Option<LuaTable> = system.call_function("get_file_info", "/usr/bin/open")?;
    let has_bin_open: Option<LuaTable> = system.call_function("get_file_info", "/bin/open")?;
    let cmd = if has_open.is_some() || has_bin_open.is_some() {
        format!("open '{escaped}'")
    } else {
        format!("xdg-open '{escaped}'")
    };
    system.call_function::<()>("exec", cmd)
}

fn lua_values_equal(a: &LuaValue, b: &LuaValue) -> bool {
    match (a, b) {
        (LuaValue::Nil, LuaValue::Nil) => true,
        (LuaValue::Boolean(x), LuaValue::Boolean(y)) => x == y,
        (LuaValue::Integer(x), LuaValue::Integer(y)) => x == y,
        (LuaValue::Number(x), LuaValue::Number(y)) => x == y,
        (LuaValue::Integer(x), LuaValue::Number(y)) => (*x as f64) == *y,
        (LuaValue::Number(x), LuaValue::Integer(y)) => *x == (*y as f64),
        (LuaValue::String(x), LuaValue::String(y)) => x.as_bytes() == y.as_bytes(),
        (LuaValue::Table(x), LuaValue::Table(y)) => x == y,
        _ => false,
    }
}

/// Registers `plugins.markdown_preview`: live markdown preview with layout, rendering,
/// link clicking, and table support.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;

    // Register layout and renderers as sub-modules that return true
    // (they are built into the main module now)
    preload.set(
        "plugins.markdown_preview.layout",
        lua.create_function(|_, ()| Ok(true))?,
    )?;
    preload.set(
        "plugins.markdown_preview.renderers",
        lua.create_function(|_, ()| Ok(true))?,
    )?;

    preload.set(
        "plugins.markdown_preview",
        lua.create_function(|lua, ()| {
            // Config defaults
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let common = require_table(lua, "core.common")?;
            let defaults = lua.create_table()?;
            defaults.set("padding", 16)?;
            defaults.set("block_gap", 8)?;
            let merged: LuaTable = common.call_function(
                "merge",
                (defaults, plugins.get::<LuaValue>("markdown_preview")?),
            )?;
            plugins.set("markdown_preview", merged)?;

            let (_md_view, class_key) = build_markdown_view(lua)?;

            // Toggle command
            let ck = Arc::clone(&class_key);
            let command = require_table(lua, "core.command")?;
            let cmds = lua.create_table()?;
            cmds.set(
                "markdown-preview:toggle",
                lua.create_function(move |lua, ()| {
                    let core = require_table(lua, "core")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaValue = dv.get("doc")?;
                    let doc = match doc {
                        LuaValue::Table(d) => d,
                        _ => {
                            core.call_function::<()>(
                                "warn",
                                "markdown-preview: active file is not a markdown document",
                            )?;
                            return Ok(());
                        }
                    };
                    let fname: LuaValue = doc.get("filename")?;
                    let is_md = match &fname {
                        LuaValue::String(s) => {
                            let s = s.to_str()?;
                            s.ends_with(".md") || s.ends_with(".markdown")
                        }
                        _ => false,
                    };
                    if !is_md {
                        core.call_function::<()>(
                            "warn",
                            "markdown-preview: active file is not a markdown document",
                        )?;
                        return Ok(());
                    }

                    // Find existing previews
                    let md_class: LuaTable = lua.registry_value(&ck)?;
                    let root_view: LuaTable = core.get("root_view")?;
                    let root_node: LuaTable = root_view.get("root_node")?;
                    let children: LuaTable = root_node.call_method("get_children", ())?;
                    let mut previews = Vec::new();
                    for pair in children.sequence_values::<LuaTable>() {
                        let view = pair?;
                        let is_md_view: bool = view.call_method("is", md_class.clone()).unwrap_or(false);
                        if is_md_view {
                            let vdoc: LuaValue = view.get("doc")?;
                            if let LuaValue::Table(ref vd) = vdoc {
                                if *vd == doc {
                                    let node: LuaTable =
                                        root_node.call_method("get_node_for_view", view.clone())?;
                                    previews.push((view, node));
                                }
                            }
                        }
                    }

                    if !previews.is_empty() {
                        for (pview, pnode) in previews {
                            let views: LuaTable = pnode.get("views")?;
                            let is_primary: bool =
                                pnode.get("is_primary_node").unwrap_or(false);
                            if views.raw_len() == 1 && !is_primary {
                                let parent: LuaValue =
                                    pnode.call_method("get_parent_node", root_node.clone())?;
                                if let LuaValue::Table(ref parent_t) = parent {
                                    let a: LuaTable = parent_t.get("a")?;
                                    let other: LuaTable = if a == pnode {
                                        parent_t.get("b")?
                                    } else {
                                        parent_t.get("a")?
                                    };
                                    parent_t.call_method::<()>("consume", other)?;
                                    parent_t.call_method::<()>("update_layout", ())?;
                                    core.call_function::<()>("set_active_view", dv.clone())?;
                                    continue;
                                }
                            }
                            pnode.call_method::<()>(
                                "close_view",
                                (root_node.clone(), pview),
                            )?;
                            core.call_function::<()>("set_active_view", dv.clone())?;
                        }
                    } else {
                        let src_node: LuaTable =
                            root_node.call_method("get_node_for_view", dv)?;
                        let new_view: LuaTable = md_class.call(doc)?;
                        src_node.call_method::<()>("split", ("right", new_view))?;
                    }
                    Ok(())
                })?,
            )?;
            command.call_function::<()>("add", ("core.docview", cmds))?;

            let keymap = require_table(lua, "core.keymap")?;
            let bindings = lua.create_table()?;
            bindings.set("ctrl+shift+m", "markdown-preview:toggle")?;
            keymap.call_function::<()>("add", bindings)?;

            Ok(LuaValue::Boolean(true))
        })?,
    )
}
