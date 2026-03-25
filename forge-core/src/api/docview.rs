use mlua::prelude::*;


fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn clamp(n: f64, lo: f64, hi: f64) -> f64 {
    n.max(lo).min(hi)
}

fn sort_positions(
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) -> (usize, usize, usize, usize, bool) {
    if line1 > line2 || (line1 == line2 && col1 > col2) {
        (line2, col2, line1, col1, true)
    } else {
        (line1, col1, line2, col2, false)
    }
}

fn call_class_method<R>(
    lua: &Lua,
    class_name: &str,
    this: &LuaTable,
    name: &str,
    args: impl IntoLuaMulti,
) -> LuaResult<R>
where
    R: FromLuaMulti,
{
    let class = require_table(lua, class_name)?;
    let func: LuaFunction = class.get(name)?;
    let mut mv = LuaMultiValue::new();
    mv.push_front(LuaValue::Table(this.clone()));
    let mut rest = args.into_lua_multi(lua)?;
    mv.append(&mut rest);
    func.call(mv)
}

fn self_class(this: &LuaTable) -> LuaResult<LuaTable> {
    this.metatable()
        .ok_or_else(|| LuaError::runtime("docview instance missing class metatable"))
}

fn lines_table(doc: &LuaTable) -> LuaResult<LuaTable> {
    doc.get("lines")
}

fn line_count(doc: &LuaTable) -> LuaResult<usize> {
    Ok(lines_table(doc)?.raw_len())
}

fn line_text(doc: &LuaTable, line: usize) -> LuaResult<String> {
    lines_table(doc)?.get(line)
}

fn doc_selections(doc: &LuaTable) -> LuaResult<Vec<usize>> {
    let selections: LuaTable = doc.get("selections")?;
    selections.sequence_values::<usize>().collect()
}

fn selection_ranges(doc: &LuaTable, sort: bool) -> LuaResult<Vec<(usize, usize, usize, usize)>> {
    let selections = doc_selections(doc)?;
    let mut out = Vec::with_capacity(selections.len() / 4);
    for chunk in selections.chunks_exact(4) {
        let (line1, col1, line2, col2, _) = if sort {
            sort_positions(chunk[0], chunk[1], chunk[2], chunk[3])
        } else {
            (chunk[0], chunk[1], chunk[2], chunk[3], false)
        };
        out.push((line1, col1, line2, col2));
    }
    Ok(out)
}

fn current_selection(doc: &LuaTable, sort: bool) -> LuaResult<(usize, usize, usize, usize)> {
    let selections = doc_selections(doc)?;
    let mut idx = doc.get::<Option<usize>>("last_selection")?.unwrap_or(1);
    if idx == 0 || selections.len() < idx * 4 {
        idx = 1;
    }
    let base = (idx - 1) * 4;
    let line1 = selections.get(base).copied().unwrap_or(1);
    let col1 = selections.get(base + 1).copied().unwrap_or(1);
    let line2 = selections.get(base + 2).copied().unwrap_or(line1);
    let col2 = selections.get(base + 3).copied().unwrap_or(col1);
    if sort {
        let (a, b, c, d, _) = sort_positions(line1, col1, line2, col2);
        Ok((a, b, c, d))
    } else {
        Ok((line1, col1, line2, col2))
    }
}

fn is_table_empty(table: &LuaTable) -> LuaResult<bool> {
    Ok(table.pairs::<LuaValue, LuaValue>().next().is_none())
}

fn docview_get_font(lua: &Lua, this: &LuaTable) -> LuaResult<LuaValue> {
    let style = require_table(lua, "core.style")?;
    let font_name: String = this.get("font")?;
    style.get(font_name)
}

fn font_call_method<R>(font: &LuaValue, name: &str, args: impl IntoLuaMulti) -> LuaResult<R>
where
    R: FromLuaMulti,
{
    match font {
        LuaValue::Table(table) => table.call_method(name, args),
        LuaValue::UserData(ud) => ud.call_method(name, args),
        _ => Err(LuaError::runtime(format!(
            "expected font table/userdata, got {}",
            font.type_name()
        ))),
    }
}

fn docview_get_line_height(lua: &Lua, this: &LuaTable) -> LuaResult<f64> {
    let config = require_table(lua, "core.config")?;
    let line_height: f64 = config.get("line_height")?;
    let font = docview_get_font(lua, this)?;
    let height: f64 = font_call_method(&font, "get_height", ())?;
    Ok((height * line_height).floor())
}

fn docview_get_gutter_width(lua: &Lua, this: &LuaTable) -> LuaResult<(f64, f64)> {
    let style = require_table(lua, "core.style")?;
    let doc: LuaTable = this.get("doc")?;
    let padding_x: f64 = style.get::<LuaTable>("padding")?.get("x")?;
    let padding = padding_x * 2.0;
    let font = docview_get_font(lua, this)?;
    let width: f64 = font_call_method(&font, "get_width", line_count(&doc)?)?;
    Ok((width + padding, padding))
}

fn gutter_width_from_method(this: &LuaTable) -> LuaResult<(f64, f64)> {
    let values: LuaMultiValue = this.call_method("get_gutter_width", ())?;
    let gw = match values.front() {
        Some(LuaValue::Number(n)) => *n,
        Some(LuaValue::Integer(i)) => *i as f64,
        Some(LuaValue::String(s)) => s.to_str()?.parse::<f64>().map_err(LuaError::external)?,
        Some(v) => return Err(LuaError::runtime(format!("invalid gutter width type: {}", v.type_name()))),
        None => return Err(LuaError::runtime("missing gutter width")),
    };
    let gpad = match values.get(1) {
        Some(LuaValue::Number(n)) => *n,
        Some(LuaValue::Integer(i)) => *i as f64,
        Some(LuaValue::String(s)) => s.to_str()?.parse::<f64>().map_err(LuaError::external)?,
        Some(LuaValue::Nil) | None => 0.0,
        Some(v) => return Err(LuaError::runtime(format!("invalid gutter padding type: {}", v.type_name()))),
    };
    Ok((gw, gpad))
}

fn docview_get_line_screen_position(
    lua: &Lua,
    this: &LuaTable,
    line: usize,
    col: Option<usize>,
) -> LuaResult<LuaMultiValue> {
    let (mut x, mut y): (f64, f64) = this.call_method("get_content_offset", ())?;
    let lh = docview_get_line_height(lua, this)?;
    let (gw, _) = gutter_width_from_method(this)?;
    let mut out = LuaMultiValue::new();

    if super::linewrap::is_active(this)? {
        let (idx, _, _, _) = super::linewrap::get_line_idx_col_count(this, line, col, false)?;
        y += (idx.saturating_sub(1)) as f64 * lh;
        let col_x = if let Some(col) = col {
            this.call_method::<f64>("get_col_x_offset", (line, col))?
        } else {
            0.0
        };
        out.push_back(LuaValue::Number(x + gw + col_x));
        out.push_back(LuaValue::Number(y));
        return Ok(out);
    }

    let style = require_table(lua, "core.style")?;
    let padding_y: f64 = style.get::<LuaTable>("padding")?.get("y")?;
    y += (line.saturating_sub(1)) as f64 * lh + padding_y;
    if let Some(col) = col {
        let col_x: f64 = this.call_method("get_col_x_offset", (line, col))?;
        out.push_back(LuaValue::Number(x + gw + col_x));
        out.push_back(LuaValue::Number(y));
    } else {
        x += gw;
        out.push_back(LuaValue::Number(x));
        out.push_back(LuaValue::Number(y));
    }
    Ok(out)
}

fn docview_get_visible_line_range(lua: &Lua, this: &LuaTable) -> LuaResult<(usize, usize)> {
    let (x, y, x2, y2): (f64, f64, f64, f64) = this.call_method("get_content_bounds", ())?;
    let _ = x;
    let _ = x2;
    let lh = docview_get_line_height(lua, this)?;

    if super::linewrap::is_active(this)? {
        let total = super::linewrap::get_total_wrapped_lines(this)? as f64;
        let min_idx = (y / lh).floor().max(1.0).min(total) as usize;
        let max_idx = (y2 / lh).floor().max(1.0).min(total) as usize + 1;
        let (minline, _) = super::linewrap::get_idx_line_col(this, min_idx)?;
        let (maxline, _) = super::linewrap::get_idx_line_col(this, max_idx.min(total as usize))?;
        return Ok((minline, maxline));
    }

    let style = require_table(lua, "core.style")?;
    let padding_y: f64 = style.get::<LuaTable>("padding")?.get("y")?;
    let doc: LuaTable = this.get("doc")?;
    let max_lines = line_count(&doc)? as f64;
    let minline = ((y - padding_y) / lh).floor() + 1.0;
    let maxline = ((y2 - padding_y) / lh).floor() + 1.0;
    Ok((
        clamp(minline, 1.0, max_lines.max(1.0)) as usize,
        clamp(maxline, 1.0, max_lines.max(1.0)) as usize,
    ))
}

fn syntax_font_for_type(
    lua: &Lua,
    _this: &LuaTable,
    token_type: &str,
    default_font: &LuaValue,
) -> LuaResult<LuaValue> {
    let style = require_table(lua, "core.style")?;
    let syntax_fonts: LuaTable = style.get("syntax_fonts")?;
    if let Some(font) = syntax_fonts.get::<Option<LuaValue>>(token_type)? {
        Ok(font)
    } else {
        Ok(default_font.clone())
    }
}

fn docview_get_col_x_offset(lua: &Lua, this: &LuaTable, line: usize, col: usize) -> LuaResult<f64> {
    let doc: LuaTable = this.get("doc")?;
    let style = require_table(lua, "core.style")?;
    let syntax_fonts: LuaTable = style.get("syntax_fonts")?;
    if is_table_empty(&syntax_fonts)? {
        let (_, indent_size): (LuaValue, usize) = doc.call_method("get_indent_info", ())?;
        let native_doc_layout = require_table(lua, "doc_layout")?;
        let font = docview_get_font(lua, this)?;
        let cell_width: f64 = font_call_method(&font, "get_width", "M")?;
        return native_doc_layout.call_function(
            "col_x_offset",
            (line_text(&doc, line).unwrap_or_else(|_| "\n".to_string()), col, indent_size, cell_width),
        );
    }

    let default_font = docview_get_font(lua, this)?;
    let (_, indent_size): (LuaValue, usize) = doc.call_method("get_indent_info", ())?;
    let _: () = font_call_method(&default_font, "set_tab_size", indent_size)?;
    let highlighter: LuaTable = doc.get("highlighter")?;
    let line_info: LuaTable = highlighter.call_method("get_line", line)?;
    let tokens: LuaTable = line_info.get("tokens")?;
    let mut column = 1usize;
    let mut xoffset = 0.0;
    let mut idx = 1usize;
    while idx <= tokens.raw_len() {
        let token_type: String = tokens.get(idx)?;
        let text: String = tokens.get(idx + 1)?;
        let font = syntax_font_for_type(lua, this, &token_type, &default_font)?;
        let _: () = font_call_method(&font, "set_tab_size", indent_size)?;
        let length = text.len();
        if column + length <= col {
            let opts = lua.create_table()?;
            opts.set("tab_offset", xoffset)?;
            let width: f64 = font_call_method(&font, "get_width", (text.clone(), opts))?;
            xoffset += width;
            column += length;
            if column >= col {
                return Ok(xoffset);
            }
        } else {
            for ch in text.chars() {
                if column >= col {
                    return Ok(xoffset);
                }
                let opts = lua.create_table()?;
                opts.set("tab_offset", xoffset)?;
                let width: f64 = font_call_method(&font, "get_width", (ch.to_string(), opts))?;
                xoffset += width;
                column += ch.len_utf8();
            }
        }
        idx += 2;
    }
    Ok(xoffset)
}

fn docview_get_x_offset_col(lua: &Lua, this: &LuaTable, line: usize, x: f64) -> LuaResult<usize> {
    let doc: LuaTable = this.get("doc")?;
    let line_text = line_text(&doc, line)?;
    let style = require_table(lua, "core.style")?;
    let syntax_fonts: LuaTable = style.get("syntax_fonts")?;
    if is_table_empty(&syntax_fonts)? {
        let (_, indent_size): (LuaValue, usize) = doc.call_method("get_indent_info", ())?;
        let native_doc_layout = require_table(lua, "doc_layout")?;
        let font = docview_get_font(lua, this)?;
        let cell_width: f64 = font_call_method(&font, "get_width", "M")?;
        let col: i64 = native_doc_layout.call_function(
            "x_offset_col",
            (line_text, x, indent_size, cell_width),
        )?;
        return Ok(col as usize);
    }

    let default_font = docview_get_font(lua, this)?;
    let (_, indent_size): (LuaValue, usize) = doc.call_method("get_indent_info", ())?;
    let _: () = font_call_method(&default_font, "set_tab_size", indent_size)?;
    let highlighter: LuaTable = doc.get("highlighter")?;
    let line_info: LuaTable = highlighter.call_method("get_line", line)?;
    let tokens: LuaTable = line_info.get("tokens")?;
    let mut xoffset = 0.0;
    let mut i = 1usize;
    let mut idx = 1usize;
    while idx <= tokens.raw_len() {
        let token_type: String = tokens.get(idx)?;
        let text: String = tokens.get(idx + 1)?;
        let font = syntax_font_for_type(lua, this, &token_type, &default_font)?;
        let _: () = font_call_method(&font, "set_tab_size", indent_size)?;
        let opts = lua.create_table()?;
        opts.set("tab_offset", xoffset)?;
        let width: f64 = font_call_method(&font, "get_width", (text.clone(), opts))?;
        if xoffset + width < x {
            xoffset += width;
            i += text.len();
        } else {
            for ch in text.chars() {
                let opts = lua.create_table()?;
                opts.set("tab_offset", xoffset)?;
                let w: f64 = font_call_method(&font, "get_width", (ch.to_string(), opts))?;
                if xoffset + w >= x {
                    return Ok(if x <= xoffset + (w / 2.0) { i } else { i + ch.len_utf8() });
                }
                xoffset += w;
                i += ch.len_utf8();
            }
        }
        idx += 2;
    }
    Ok(line_text.len())
}

fn move_to_line_offset(lua: &Lua, this: &LuaTable, line: usize, col: usize, offset: isize) -> LuaResult<(usize, usize)> {
    let last_x_offset: LuaTable = this.get("last_x_offset")?;
    let xo_line = last_x_offset.get::<Option<usize>>("line")?;
    let xo_col = last_x_offset.get::<Option<usize>>("col")?;
    if xo_line != Some(line) || xo_col != Some(col) {
        let xoff: f64 = docview_get_col_x_offset(lua, this, line, col)?;
        last_x_offset.set("offset", xoff)?;
    }
    let target_line = (line as isize + offset) as usize;
    let xoff: f64 = last_x_offset.get::<Option<f64>>("offset")?.unwrap_or(0.0);
    let target_col: usize = docview_get_x_offset_col(lua, this, target_line, xoff)?;
    last_x_offset.set("line", target_line)?;
    last_x_offset.set("col", target_col)?;
    Ok((target_line, target_col))
}

fn translate_previous_page(_: &Lua, (_doc, line, _col, dv): (LuaTable, usize, usize, LuaTable)) -> LuaResult<(usize, usize)> {
    let (min, max): (usize, usize) = dv.call_method("get_visible_line_range", ())?;
    Ok((line.saturating_sub(max.saturating_sub(min)), 1))
}

fn translate_next_page(_: &Lua, (doc, line, _col, dv): (LuaTable, usize, usize, LuaTable)) -> LuaResult<(usize, usize)> {
    let lines = line_count(&doc)?;
    if line == lines {
        let text = line_text(&doc, line)?;
        return Ok((lines, text.len()));
    }
    let (min, max): (usize, usize) = dv.call_method("get_visible_line_range", ())?;
    Ok((line + max.saturating_sub(min), 1))
}

fn translate_previous_line(lua: &Lua, (_doc, line, col, dv): (LuaTable, usize, usize, LuaTable)) -> LuaResult<(usize, usize)> {
    if super::linewrap::is_active(&dv)? {
        let (idx, _, _, _) = super::linewrap::get_line_idx_col_count(&dv, line, Some(col), false)?;
        if idx <= 1 {
            return Ok((1, 1));
        }
        let xoff: f64 = dv.call_method("get_col_x_offset", (line, col))?;
        return super::linewrap::get_line_col_from_x(lua, &dv, idx - 1, xoff);
    }
    if line == 1 {
        Ok((1, 1))
    } else {
        move_to_line_offset(lua, &dv, line, col, -1)
    }
}

fn translate_next_line(lua: &Lua, (doc, line, col, dv): (LuaTable, usize, usize, LuaTable)) -> LuaResult<(usize, usize)> {
    if super::linewrap::is_active(&dv)? {
        let total = super::linewrap::get_total_wrapped_lines(&dv)?;
        let (idx, _, _, _) = super::linewrap::get_line_idx_col_count(&dv, line, Some(col), false)?;
        if idx >= total {
            let lines = line_count(&doc)?;
            let text = line_text(&doc, lines)?;
            return Ok((lines, text.len()));
        }
        let xoff: f64 = dv.call_method("get_col_x_offset", (line, col))?;
        return super::linewrap::get_line_col_from_x(lua, &dv, idx + 1, xoff);
    }
    let lines = line_count(&doc)?;
    if line == lines {
        let text = line_text(&doc, line)?;
        Ok((lines, text.len()))
    } else {
        move_to_line_offset(lua, &dv, line, col, 1)
    }
}

fn table_equals(a: &LuaTable, b: &LuaTable) -> LuaResult<bool> {
    a.equals(b)
}

fn active_view_is(lua: &Lua, this: &LuaTable) -> LuaResult<bool> {
    let core = require_table(lua, "core")?;
    if let Some(active_view) = core.get::<Option<LuaTable>>("active_view")? {
        table_equals(&active_view, this)
    } else {
        Ok(false)
    }
}

fn docview_new(lua: &Lua, (this, doc): (LuaTable, Option<LuaTable>)) -> LuaResult<()> {
    call_class_method::<()>(lua, "core.view", &this, "new", ())?;
    this.set("cursor", "ibeam")?;
    this.set("scrollable", true)?;
    let doc = if let Some(doc) = doc {
        doc
    } else if let Some(doc) = this.get::<Option<LuaTable>>("doc")? {
        doc
    } else {
        {
            let doc_cls: LuaTable = require_table(lua, "core.doc")?;
            let getmt: LuaFunction = lua.globals().get("getmetatable")?;
            let mt: LuaTable = getmt.call(doc_cls.clone())?;
            let call_fn: LuaFunction = mt.get("__call")?;
            call_fn.call::<LuaTable>(doc_cls)?
        }
    };
    this.set("doc", doc.clone())?;
    this.set("font", "code_font")?;
    this.set("last_x_offset", lua.create_table()?)?;
    // Initialize wrapping state so linewrap helpers always find these fields.
    let cfg = require_table(lua, "core.config");
    let enable_by_default = cfg.ok()
        .and_then(|c| c.get::<LuaTable>("plugins").ok())
        .and_then(|p| p.get::<Option<LuaTable>>("linewrapping").ok().flatten())
        .and_then(|lw| lw.get::<Option<bool>>("enable_by_default").ok().flatten())
        .unwrap_or(false);
    this.set("wrapping_enabled", enable_by_default)?;
    let ime_selection = lua.create_table()?;
    ime_selection.set("from", 0)?;
    ime_selection.set("size", 0)?;
    this.set("ime_selection", ime_selection)?;
    this.set("ime_status", false)?;
    this.set("hovering_gutter", false)?;
    let config = require_table(lua, "core.config")?;
    let forced_status: LuaValue = config.get("force_scrollbar_status")?;
    let v_scrollbar: LuaTable = this.get("v_scrollbar")?;
    let h_scrollbar: LuaTable = this.get("h_scrollbar")?;
    v_scrollbar.call_method::<()>("set_forced_status", forced_status.clone())?;
    h_scrollbar.call_method::<()>("set_forced_status", forced_status)?;
    Ok(())
}

fn docview_try_close(lua: &Lua, (this, do_close): (LuaTable, LuaFunction)) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let doc: LuaTable = this.get("doc")?;
    let is_dirty: bool = doc.call_method("is_dirty", ())?;
    let refs: LuaTable = core.call_function("get_views_referencing_doc", doc.clone())?;
    if is_dirty && refs.raw_len() == 1 {
        let command_view: LuaTable = core.get("command_view")?;
        let spec = lua.create_table()?;
        let this_submit = this.clone();
        let do_close_submit = do_close.clone();
        spec.set(
            "submit",
            lua.create_function(move |_, (_text, item): (LuaValue, LuaTable)| {
                let item_text: String = item.get("text")?;
                if item_text.starts_with('C') || item_text.starts_with('c') {
                    do_close_submit.call::<()>(())?;
                } else if item_text.starts_with('S') || item_text.starts_with('s') {
                    let doc: LuaTable = this_submit.get("doc")?;
                    doc.call_method::<()>("save", ())?;
                    do_close_submit.call::<()>(())?;
                }
                Ok(())
            })?,
        )?;
        spec.set(
            "suggest",
            lua.create_function(|lua, text: String| {
                let items = lua.create_table()?;
                let mut idx = 1;
                if !text.chars().next().is_some_and(|ch| ch != 'c' && ch != 'C') {
                    items.set(idx, "Close Without Saving")?;
                    idx += 1;
                }
                if !text.chars().next().is_some_and(|ch| ch != 's' && ch != 'S') {
                    items.set(idx, "Save And Close")?;
                }
                Ok(items)
            })?,
        )?;
        command_view.call_method::<()>("enter", ("Unsaved Changes; Confirm Close", spec))?;
    } else {
        do_close.call::<()>(())?;
    }
    Ok(())
}

fn docview_get_name(_: &Lua, this: LuaTable) -> LuaResult<String> {
    let doc: LuaTable = this.get("doc")?;
    let dirty: bool = doc.call_method("is_dirty", ())?;
    let name: String = doc.call_method("get_name", ())?;
    let tail = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name.as_str())
        .to_string();
    Ok(if dirty { format!("{tail}*") } else { tail })
}

fn docview_get_filename(lua: &Lua, this: LuaTable) -> LuaResult<String> {
    let doc: LuaTable = this.get("doc")?;
    if let Some(abs) = doc.get::<Option<String>>("abs_filename")? {
        let common = require_table(lua, "core.common")?;
        let mut encoded: String = common.call_function("home_encode", abs)?;
        let dirty: bool = doc.call_method("is_dirty", ())?;
        if dirty {
            encoded.push('*');
        }
        Ok(encoded)
    } else {
        docview_get_name(lua, this)
    }
}

fn docview_get_scrollable_size(lua: &Lua, this: LuaTable) -> LuaResult<f64> {
    let lh = docview_get_line_height(lua, &this)?;
    if super::linewrap::is_active(&this)? {
        let total = super::linewrap::get_total_wrapped_lines(&this)? as f64;
        let config = require_table(lua, "core.config")?;
        let scroll_past_end: bool = config.get("scroll_past_end")?;
        if !scroll_past_end {
            let style = require_table(lua, "core.style")?;
            let padding_y: f64 = style.get::<LuaTable>("padding")?.get("y")?;
            return Ok(lh * total + padding_y * 2.0);
        }
        return Ok(lh * (total - 1.0) + this.get::<LuaTable>("size")?.get::<f64>("y")?);
    }
    let config = require_table(lua, "core.config")?;
    let scroll_past_end: bool = config.get("scroll_past_end")?;
    if !scroll_past_end {
        let h_scrollbar: LuaTable = this.get("h_scrollbar")?;
        let (_, _, _, h_scroll): (f64, f64, f64, f64) = h_scrollbar.call_method("get_track_rect", ())?;
        let doc: LuaTable = this.get("doc")?;
        let style = require_table(lua, "core.style")?;
        let padding_y: f64 = style.get::<LuaTable>("padding")?.get("y")?;
        return Ok(lh * line_count(&doc)? as f64 + padding_y * 2.0 + h_scroll);
    }
    let doc: LuaTable = this.get("doc")?;
    Ok(lh * line_count(&doc)?.saturating_sub(1) as f64 + this.get::<LuaTable>("size")?.get::<f64>("y")?)
}

fn docview_get_h_scrollable_size(_: &Lua, this: LuaTable) -> LuaResult<f64> {
    if this.get::<bool>("wrapping_enabled").unwrap_or(false) {
        return Ok(0.0);
    }
    Ok(f64::INFINITY)
}

fn docview_get_font_lua(lua: &Lua, this: LuaTable) -> LuaResult<LuaValue> {
    docview_get_font(lua, &this)
}

fn docview_get_line_height_lua(lua: &Lua, this: LuaTable) -> LuaResult<f64> {
    docview_get_line_height(lua, &this)
}

fn docview_get_gutter_width_lua(lua: &Lua, this: LuaTable) -> LuaResult<(f64, f64)> {
    docview_get_gutter_width(lua, &this)
}

fn docview_get_line_screen_position_lua(
    lua: &Lua,
    (this, line, col): (LuaTable, usize, Option<usize>),
) -> LuaResult<LuaMultiValue> {
    docview_get_line_screen_position(lua, &this, line, col)
}

fn docview_get_line_text_y_offset(lua: &Lua, this: LuaTable) -> LuaResult<f64> {
    let lh = docview_get_line_height(lua, &this)?;
    let font = docview_get_font(lua, &this)?;
    let th: f64 = font_call_method(&font, "get_height", ())?;
    Ok((lh - th) / 2.0)
}

fn docview_get_visible_line_range_lua(lua: &Lua, this: LuaTable) -> LuaResult<(usize, usize)> {
    docview_get_visible_line_range(lua, &this)
}

fn docview_get_col_x_offset_lua(lua: &Lua, (this, line, col, line_end): (LuaTable, usize, usize, Option<bool>)) -> LuaResult<f64> {
    if super::linewrap::is_active(&this)? {
        return super::linewrap::get_col_x_offset(lua, &this, line, col, line_end.unwrap_or(false));
    }
    docview_get_col_x_offset(lua, &this, line, col)
}

fn docview_get_x_offset_col_lua(lua: &Lua, (this, line, x): (LuaTable, usize, f64)) -> LuaResult<usize> {
    if super::linewrap::is_active(&this)? {
        let (idx, _, _, _) = super::linewrap::get_line_idx_col_count(&this, line, None, false)?;
        let (_, col) = super::linewrap::get_line_col_from_x(lua, &this, idx, x)?;
        return Ok(col);
    }
    docview_get_x_offset_col(lua, &this, line, x)
}

fn docview_resolve_screen_position(lua: &Lua, (this, x, y): (LuaTable, f64, f64)) -> LuaResult<(usize, usize)> {
    let (ox, oy): (f64, f64) = this.call_method("get_line_screen_position", (1, LuaValue::Nil))?;
    let lh = docview_get_line_height(lua, &this)?;

    if super::linewrap::is_active(&this)? {
        let total = super::linewrap::get_total_wrapped_lines(&this)?;
        let idx = (((y - oy) / lh).floor() as isize + 1).clamp(1, total as isize) as usize;
        return super::linewrap::get_line_col_from_x(lua, &this, idx, x - ox);
    }

    let line = (((y - oy) / lh).floor() + 1.0) as usize;
    let doc: LuaTable = this.get("doc")?;
    let clamped = line.clamp(1, line_count(&doc)?.max(1));
    let col: usize = this.call_method("get_x_offset_col", (clamped, x - ox))?;
    Ok((clamped, col))
}

fn docview_scroll_to_line(
    lua: &Lua,
    (this, line, ignore_if_visible, instant): (LuaTable, usize, Option<bool>, Option<bool>),
) -> LuaResult<()> {
    if this.get::<bool>("wrapping_enabled").unwrap_or(false) {
        super::linewrap::update_docview_breaks(lua, &this)?;
    }
    let (min, max): (usize, usize) = this.call_method("get_visible_line_range", ())?;
    if !(ignore_if_visible.unwrap_or(false) && line > min && line < max) {
        let (_, y): (f64, f64) = this.call_method("get_line_screen_position", (line, LuaValue::Nil))?;
        let (_, oy): (f64, f64) = this.call_method("get_content_offset", ())?;
        let h_scrollbar: LuaTable = this.get("h_scrollbar")?;
        let (_, _, _, scroll_h): (f64, f64, f64, f64) = h_scrollbar.call_method("get_track_rect", ())?;
        let size: LuaTable = this.get("size")?;
        let scroll: LuaTable = this.get("scroll")?;
        let to: LuaTable = scroll.get("to")?;
        let target = (y - oy - (size.get::<f64>("y")? - scroll_h) / 2.0).max(0.0);
        to.set("y", target)?;
        if instant.unwrap_or(false) {
            scroll.set("y", target)?;
        }
    }
    Ok(())
}

fn docview_supports_text_input(_: &Lua, _: LuaTable) -> LuaResult<bool> {
    Ok(true)
}

fn docview_scroll_to_make_visible(lua: &Lua, (this, line, col): (LuaTable, usize, usize)) -> LuaResult<()> {
    if this.get::<bool>("wrapping_enabled").unwrap_or(false) {
        super::linewrap::update_docview_breaks(lua, &this)?;
    }
    let (_, oy): (f64, f64) = this.call_method("get_content_offset", ())?;
    let (_, ly): (f64, f64) = this.call_method("get_line_screen_position", (line, col))?;
    let lh = docview_get_line_height(lua, &this)?;
    let h_scrollbar: LuaTable = this.get("h_scrollbar")?;
    let (_, _, _, scroll_h): (f64, f64, f64, f64) = h_scrollbar.call_method("get_track_rect", ())?;
    let size: LuaTable = this.get("size")?;
    let overscroll = (lh * 2.0).min(size.get::<f64>("y")?);
    let scroll: LuaTable = this.get("scroll")?;
    let to: LuaTable = scroll.get("to")?;
    let current_to_y: f64 = to.get("y")?;
    let min_y = ly - oy - size.get::<f64>("y")? + scroll_h + overscroll;
    let max_y = ly - oy - lh;
    to.set("y", clamp(current_to_y, min_y, max_y))?;
    if super::linewrap::is_active(&this)? {
        to.set("x", 0.0f64)?;
        return Ok(());
    }
    let (gw, _) = gutter_width_from_method(&this)?;
    let xoffset: f64 = this.call_method("get_col_x_offset", (line, col))?;
    let font = docview_get_font(lua, &this)?;
    let xmargin: f64 = font_call_method(&font, "get_width", "   ")?;
    let xsup = xoffset + gw + xmargin;
    let xinf = xoffset - xmargin;
    let v_scrollbar: LuaTable = this.get("v_scrollbar")?;
    let (_, _, scroll_w, _): (f64, f64, f64, f64) = v_scrollbar.call_method("get_track_rect", ())?;
    let size_x = (size.get::<f64>("x")? - scroll_w).max(0.0);
    let scroll_x: f64 = scroll.get("x")?;
    if xsup > scroll_x + size_x {
        to.set("x", xsup - size_x)?;
    } else if xinf < scroll_x {
        to.set("x", xinf.max(0.0))?;
    }
    Ok(())
}

fn docview_mouse_selection(
    lua: &Lua,
    (this, doc, snap_type, mut line1, mut col1, mut line2, mut col2): (
        LuaTable,
        LuaTable,
        String,
        usize,
        usize,
        usize,
        usize,
    ),
) -> LuaResult<(usize, usize, usize, usize)> {
    let translate = require_table(lua, "core.doc.translate")?;
    let swap = line2 < line1 || (line2 == line1 && col2 <= col1);
    if swap {
        (line1, col1, line2, col2) = (line2, col2, line1, col1);
    }
    if snap_type == "word" {
        let res: (usize, usize) = translate.call_function("start_of_word", (doc.clone(), line1, col1))?;
        (line1, col1) = res;
        let res: (usize, usize) = translate.call_function("end_of_word", (doc, line2, col2))?;
        (line2, col2) = res;
    } else if snap_type == "lines" {
        col1 = 1;
        col2 = 1;
        line2 += 1;
    }
    let _ = this;
    if swap {
        Ok((line2, col2, line1, col1))
    } else {
        Ok((line1, col1, line2, col2))
    }
}

fn docview_on_mouse_moved(
    lua: &Lua,
    (this, x, y, dx, dy): (LuaTable, f64, f64, f64, f64),
) -> LuaResult<()> {
    call_class_method::<LuaValue>(lua, "core.view", &this, "on_mouse_moved", (x, y, dx, dy))?;
    this.set("hovering_gutter", false)?;
    let (gw, _) = gutter_width_from_method(&this)?;
    let position: LuaTable = this.get("position")?;
    let px: f64 = position.get("x")?;
    if this.call_method::<bool>("scrollbar_hovering", ())? || this.call_method::<bool>("scrollbar_dragging", ())? {
        this.set("cursor", "arrow")?;
    } else if gw > 0.0 && x >= px && x <= px + gw {
        this.set("cursor", "arrow")?;
        this.set("hovering_gutter", true)?;
    } else {
        this.set("cursor", "ibeam")?;
    }
    if let Some(mouse_selecting) = this.get::<Option<LuaTable>>("mouse_selecting")? {
        let (mut l1, mut c1): (usize, usize) = this.call_method("resolve_screen_position", (x, y))?;
        let mut l2: usize = mouse_selecting.get(1)?;
        let mut c2: usize = mouse_selecting.get(2)?;
        let snap_type: Option<String> = mouse_selecting.get(3)?;
        let keymap = require_table(lua, "core.keymap")?;
        let modkeys: LuaTable = keymap.get("modkeys")?;
        if modkeys.get::<Option<bool>>("ctrl")?.unwrap_or(false) {
            if l1 > l2 {
                std::mem::swap(&mut l1, &mut l2);
            }
            let doc: LuaTable = this.get("doc")?;
            doc.set("selections", lua.create_table()?)?;
            for i in l1..=l2 {
                let text = line_text(&doc, i)?;
                let a = c1.min(text.len());
                let b = c2.min(text.len());
                doc.call_method::<()>("set_selections", (i - l1 + 1, i, a, i, b))?;
            }
        } else {
            if let Some(snap) = snap_type {
                let doc: LuaTable = this.get("doc")?;
                (l1, c1, l2, c2) =
                    docview_mouse_selection(lua, (this.clone(), doc.clone(), snap, l1, c1, l2, c2))?;
            }
            let doc: LuaTable = this.get("doc")?;
            doc.call_method::<()>("set_selection", (l1, c1, l2, c2))?;
        }
    }
    Ok(())
}

fn docview_on_mouse_pressed(
    lua: &Lua,
    (this, button, x, y, clicks): (LuaTable, String, f64, f64, i64),
) -> LuaResult<LuaValue> {
    let hovering_gutter: bool = this.get("hovering_gutter")?;
    if button != "left" || !hovering_gutter {
        return call_class_method(lua, "core.view", &this, "on_mouse_pressed", (button, x, y, clicks));
    }
    let (line, _): (usize, usize) = this.call_method("resolve_screen_position", (x, y))?;
    let keymap = require_table(lua, "core.keymap")?;
    let modkeys: LuaTable = keymap.get("modkeys")?;
    let doc: LuaTable = this.get("doc")?;
    if modkeys.get::<Option<bool>>("shift")?.unwrap_or(false) {
        let (sline, _scol, sline2, _scol2): (usize, usize, usize, usize) = doc.call_method("get_selection", true)?;
        if line > sline {
            let end_len = line_text(&doc, line)?.len();
            doc.call_method::<()>("set_selection", (sline, 1, line, end_len))?;
        } else {
            let end_len = line_text(&doc, sline2)?.len();
            doc.call_method::<()>("set_selection", (line, 1, sline2, end_len))?;
        }
    } else if clicks == 1 {
        doc.call_method::<()>("set_selection", (line, 1, line, 1))?;
    } else if clicks == 2 {
        let end_len = line_text(&doc, line)?.len();
        doc.call_method::<()>("set_selection", (line, 1, line, end_len))?;
    }
    Ok(LuaValue::Boolean(true))
}

fn docview_on_mouse_released(lua: &Lua, args: LuaMultiValue) -> LuaResult<()> {
    let this = match args.front() {
        Some(LuaValue::Table(t)) => t.clone(),
        _ => return Err(LuaError::runtime("docview:on_mouse_released missing self")),
    };
    let mut rest = args.clone();
    rest.pop_front();
    call_class_method::<()>(lua, "core.view", &this, "on_mouse_released", rest)?;
    this.set("mouse_selecting", LuaValue::Nil)?;
    Ok(())
}

fn docview_on_text_input(_: &Lua, (this, text): (LuaTable, String)) -> LuaResult<()> {
    let doc: LuaTable = this.get("doc")?;
    doc.call_method("text_input", text)
}

fn docview_on_ime_text_editing(
    _: &Lua,
    (this, text, start, length): (LuaTable, String, i64, i64),
) -> LuaResult<()> {
    let doc: LuaTable = this.get("doc")?;
    doc.call_method::<()>("ime_text_editing", (text.clone(), start, length))?;
    this.set("ime_status", !text.is_empty())?;
    let ime_selection: LuaTable = this.get("ime_selection")?;
    ime_selection.set("from", start)?;
    ime_selection.set("size", length)?;
    let (line1, col1, _line2, col2): (usize, usize, usize, usize) = doc.call_method("get_selection", true)?;
    let col = col1.min(col2);
    this.call_method::<()>("update_ime_location", ())?;
    this.call_method::<()>("scroll_to_make_visible", (line1, (col as i64 + start) as usize))?;
    Ok(())
}

fn docview_update_ime_location(lua: &Lua, this: LuaTable) -> LuaResult<()> {
    if !this.get::<bool>("ime_status")? {
        return Ok(());
    }
    let doc: LuaTable = this.get("doc")?;
    let (line1, col1, line2, col2): (usize, usize, usize, usize) = doc.call_method("get_selection", true)?;
    let (x, y): (f64, f64) = this.call_method("get_line_screen_position", (line1, LuaValue::Nil))?;
    let h = docview_get_line_height(lua, &this)?;
    let col = col1.min(col2);
    let ime_selection: LuaTable = this.get("ime_selection")?;
    let from: i64 = ime_selection.get("from")?;
    let size: i64 = ime_selection.get("size")?;
    let (x1, x2) = if size > 0 {
        let from_col = (col as i64 + from) as usize;
        let to_col = (from_col as i64 + size) as usize;
        let x1: f64 = this.call_method("get_col_x_offset", (line1, from_col))?;
        let x2: f64 = this.call_method("get_col_x_offset", (line1, to_col))?;
        (x1, x2)
    } else {
        let x1: f64 = this.call_method("get_col_x_offset", (line1, col1))?;
        let x2: f64 = this.call_method("get_col_x_offset", (line2, col2))?;
        (x1, x2)
    };
    let ime = require_table(lua, "core.ime")?;
    ime.call_function::<()>("set_location", (x + x1, y, x2 - x1, h))?;
    Ok(())
}

fn docview_update(lua: &Lua, this: LuaTable) -> LuaResult<()> {
    let doc: LuaTable = this.get("doc")?;
    let (line1, col1, line2, col2) = current_selection(&doc, false)?;
    let size: LuaTable = this.get("size")?;
    if (this.get::<Option<usize>>("last_line1")? != Some(line1)
        || this.get::<Option<usize>>("last_col1")? != Some(col1)
        || this.get::<Option<usize>>("last_line2")? != Some(line2)
        || this.get::<Option<usize>>("last_col2")? != Some(col2))
        && size.get::<f64>("x")? > 0.0
    {
        let ime = require_table(lua, "core.ime")?;
        if active_view_is(lua, &this)? && !ime.get::<bool>("editing")? {
            this.call_method::<()>("scroll_to_make_visible", (line1, col1))?;
        }
        let core = require_table(lua, "core")?;
        core.call_function::<()>("blink_reset", ())?;
        this.set("last_line1", line1)?;
        this.set("last_col1", col1)?;
        this.set("last_line2", line2)?;
        this.set("last_col2", col2)?;
    }
    let config = require_table(lua, "core.config")?;
    let core = require_table(lua, "core")?;
    let mouse_selecting = this.get::<Option<LuaTable>>("mouse_selecting")?.is_some();
    if !config.get::<bool>("disable_blink")?
        && require_table(lua, "system")?.call_function::<bool>("window_has_focus", core.get::<LuaValue>("window")?)?
        && active_view_is(lua, &this)?
        && !mouse_selecting
    {
        let period: f64 = config.get("blink_period")?;
        let t0: f64 = core.get("blink_start")?;
        let ta: f64 = core.get("blink_timer")?;
        let tb: f64 = require_table(lua, "system")?.call_function("get_time", ())?;
        if (((tb - t0) % period) < period / 2.0) != (((ta - t0) % period) < period / 2.0) {
            core.set("redraw", true)?;
        }
        core.set("blink_timer", tb)?;
    }
    this.call_method::<()>("update_ime_location", ())?;
    update_bracketmatch(lua, &this)?;
    if this.get::<bool>("wrapping_enabled").unwrap_or(false) && size.get::<f64>("x")? > 0.0 {
        super::linewrap::update_docview_breaks(lua, &this)?;
    }
    call_class_method::<()>(lua, "core.view", &this, "update", ())
}

fn all_lines(doc: &LuaTable) -> LuaResult<Vec<String>> {
    let table: LuaTable = doc.get("lines")?;
    let count = table.raw_len();
    let mut lines = Vec::with_capacity(count);
    for i in 1..=count {
        lines.push(table.get(i)?);
    }
    Ok(lines)
}

/// Recomputes the bracket-match pair for the current cursor and caches it on the DocView.
fn update_bracketmatch(lua: &Lua, this: &LuaTable) -> LuaResult<()> {
    let doc: LuaTable = this.get("doc")?;
    let (line1, col1, line2, col2) = current_selection(&doc, false)?;
    if line1 != line2 || col1 != col2 {
        this.set("_bm_pos", LuaValue::Nil)?;
        return Ok(());
    }
    let change_id: i64 = doc.call_method("get_change_id", ())?;
    let cached_line: Option<usize> = this.get("_bm_line")?;
    let cached_col: Option<usize> = this.get("_bm_col")?;
    let cached_cid: Option<i64> = this.get("_bm_cid")?;
    if cached_line == Some(line1) && cached_col == Some(col1) && cached_cid == Some(change_id) {
        return Ok(());
    }
    this.set("_bm_line", line1)?;
    this.set("_bm_col", col1)?;
    this.set("_bm_cid", change_id)?;
    let lines = all_lines(&doc)?;
    let pair = super::affordance_model::bracket_pair(&lines, line1, col1).or_else(|| {
        if col1 > 1 {
            super::affordance_model::bracket_pair(&lines, line1, col1 - 1)
        } else {
            None
        }
    });
    if let Some((l1, c1, l2, c2)) = pair {
        let pos = lua.create_table()?;
        pos.set(1, l1)?;
        pos.set(2, c1)?;
        pos.set(3, l2)?;
        pos.set(4, c2)?;
        this.set("_bm_pos", pos)?;
    } else {
        this.set("_bm_pos", LuaValue::Nil)?;
    }
    Ok(())
}

fn renderer_draw_rect(lua: &Lua, args: impl IntoLuaMulti) -> LuaResult<()> {
    let renderer: LuaTable = lua.globals().get("renderer")?;
    renderer.call_function("draw_rect", args)
}

fn docview_draw_line_highlight(lua: &Lua, (this, x, y): (LuaTable, f64, f64)) -> LuaResult<()> {
    let style = require_table(lua, "core.style")?;
    renderer_draw_rect(
        lua,
        (
            x,
            y,
            this.get::<LuaTable>("size")?.get::<f64>("x")?,
            docview_get_line_height(lua, &this)?,
            style.get::<LuaValue>("line_highlight")?,
        ),
    )
}

fn docview_draw_line_text(lua: &Lua, (this, line, x, y): (LuaTable, usize, f64, f64)) -> LuaResult<f64> {
    let doc: LuaTable = this.get("doc")?;
    let lh = docview_get_line_height(lua, &this)?;

    if super::linewrap::is_active(&this)? {
        return docview_draw_line_text_wrapped(lua, &this, &doc, line, x, y, lh);
    }

    let default_font = docview_get_font(lua, &this)?;
    let mut tx = x;
    let ty = y + this.call_method::<f64>("get_line_text_y_offset", ())?;
    let highlighter: LuaTable = doc.get("highlighter")?;
    let line_info: LuaTable = highlighter.call_method("get_line", line)?;
    let tokens: LuaTable = line_info.get("tokens")?;
    let tokens_count = tokens.raw_len();
    let mut last_token = None;
    if tokens_count > 0 {
        let last_text: String = tokens.get(tokens_count)?;
        if last_text.ends_with('\n') {
            last_token = Some(tokens_count - 1);
        }
    }
    let start_tx = tx;
    let style = require_table(lua, "core.style")?;
    let syntax: LuaTable = style.get("syntax")?;
    let syntax_fonts: LuaTable = style.get("syntax_fonts")?;
    let renderer: LuaTable = lua.globals().get("renderer")?;
    let pos: LuaTable = this.get("position")?;
    let limit_x = pos.get::<f64>("x")? + this.get::<LuaTable>("size")?.get::<f64>("x")?;
    let mut idx = 1usize;
    while idx <= tokens_count {
        let token_idx = idx;
        let token_type: String = tokens.get(idx)?;
        let mut text: String = tokens.get(idx + 1)?;
        let color = syntax
            .get::<Option<LuaValue>>(token_type.as_str())?
            .or_else(|| syntax.get::<Option<LuaValue>>("normal").ok().flatten())
            .unwrap_or(LuaValue::Nil);
        let font = syntax_fonts
            .get::<Option<LuaValue>>(token_type.as_str())?
            .unwrap_or_else(|| default_font.clone());
        if last_token == Some(token_idx) && text.ends_with('\n') {
            text.pop();
        }
        let opts = lua.create_table()?;
        opts.set("tab_offset", tx - start_tx)?;
        tx = renderer.call_function("draw_text", (font, text, tx, ty, color, opts))?;
        if tx > limit_x {
            break;
        }
        idx += 2;
    }
    Ok(lh)
}

fn docview_draw_line_text_wrapped(
    lua: &Lua,
    this: &LuaTable,
    doc: &LuaTable,
    line: usize,
    x: f64,
    y: f64,
    lh: f64,
) -> LuaResult<f64> {
    let default_font = docview_get_font(lua, this)?;
    let ty_offset = this.call_method::<f64>("get_line_text_y_offset", ())?;
    let offsets: LuaTable = this.get("wrapped_line_offsets")?;
    let begin_width: f64 = offsets.get(line).unwrap_or(0.0);

    let (mut vis_idx, _, count, _) = super::linewrap::get_line_idx_col_count(this, line, None, false)?;
    let mut tx = x;
    let mut ty = y + ty_offset;

    let style = require_table(lua, "core.style")?;
    let syntax: LuaTable = style.get("syntax")?;
    let syntax_fonts: LuaTable = style.get("syntax_fonts")?;
    let renderer: LuaTable = lua.globals().get("renderer")?;

    let highlighter: LuaTable = doc.get("highlighter")?;
    let line_info: LuaTable = highlighter.call_method("get_line", line)?;
    let tokens: LuaTable = line_info.get("tokens")?;
    let tokens_count = tokens.raw_len();

    let mut total_offset = 1usize;
    let mut tok_idx = 1usize;

    while tok_idx <= tokens_count {
        let token_type: String = tokens.get(tok_idx)?;
        let text: String = tokens.get(tok_idx + 1)?;
        let color = syntax
            .get::<Option<LuaValue>>(token_type.as_str())?
            .or_else(|| syntax.get::<Option<LuaValue>>("normal").ok().flatten())
            .unwrap_or(LuaValue::Nil);
        let font = syntax_fonts
            .get::<Option<LuaValue>>(token_type.as_str())?
            .unwrap_or_else(|| default_font.clone());

        let mut token_offset = 0usize;
        loop {
            if token_offset >= text.len() {
                break;
            }
            let (next_line, next_col) = super::linewrap::get_idx_line_col(this, vis_idx + 1)?;
            let next_line_start = if next_line != line {
                let line_text: String = doc.get::<LuaTable>("lines")?.get(line)?;
                line_text.len()
            } else {
                next_col
            };
            let max_len = next_line_start.saturating_sub(total_offset);
            let available = text.len() - token_offset;
            let render_len = max_len.min(available);
            let rendered = &text[token_offset..token_offset + render_len];
            // Strip trailing newline — the font renders it as a visible glyph.
            let draw_text = rendered.trim_end_matches('\n');
            if !draw_text.is_empty() {
                tx = renderer.call_function("draw_text", (font.clone(), draw_text, tx, ty, color.clone()))?;
            }
            total_offset += render_len;

            if total_offset < next_line_start || max_len == 0 {
                break;
            }
            token_offset += render_len;
            vis_idx += 1;
            tx = x + begin_width;
            ty += lh;
        }
        tok_idx += 2;
    }

    Ok(lh * count as f64)
}

fn docview_draw_overwrite_caret(lua: &Lua, (this, x, y, width): (LuaTable, f64, f64, f64)) -> LuaResult<()> {
    let style = require_table(lua, "core.style")?;
    let lh = docview_get_line_height(lua, &this)?;
    let caret_width: f64 = style.get("caret_width")?;
    renderer_draw_rect(
        lua,
        (x, y + lh - caret_width, width, caret_width, style.get::<LuaValue>("caret")?),
    )
}

fn docview_draw_caret(lua: &Lua, (this, x, y): (LuaTable, f64, f64)) -> LuaResult<()> {
    let style = require_table(lua, "core.style")?;
    renderer_draw_rect(
        lua,
        (
            x,
            y,
            style.get::<f64>("caret_width")?,
            docview_get_line_height(lua, &this)?,
            style.get::<LuaValue>("caret")?,
        ),
    )
}

fn selection_match_text(doc: &LuaTable) -> LuaResult<Option<(String, usize, usize, usize)>> {
    let selections = doc_selections(doc)?;
    if selections.len() != 4 {
        return Ok(None);
    }
    let (line1, col1, line2, col2) = current_selection(doc, true)?;
    if line1 != line2 || col1 == col2 {
        return Ok(None);
    }
    let text: String = doc.call_method("get_text", (line1, col1, line2, col2))?;
    if text.is_empty() || text.chars().any(|ch| ch.is_whitespace()) || text.len() > 128 {
        return Ok(None);
    }
    Ok(Some((text, line1, col1, col2)))
}

fn is_word_char_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn docview_draw_selection_matches(lua: &Lua, (this, line, x, y): (LuaTable, usize, f64, f64)) -> LuaResult<()> {
    let doc: LuaTable = this.get("doc")?;
    let Some((text, sel_line, sel_col1, sel_col2)) = selection_match_text(&doc)? else {
        return Ok(());
    };
    let line_text = line_text(&doc, line)?;
    let bytes = line_text.as_bytes();
    let needle = text.as_bytes();
    let style = require_table(lua, "core.style")?;
    let color = style
        .get::<Option<LuaValue>>("selection_match")?
        .or_else(|| style.get::<Option<LuaValue>>("line_highlight").ok().flatten())
        .unwrap_or(style.get("selection")?);
    let lh = docview_get_line_height(lua, &this)?;
    let mut start = 0usize;
    while start <= bytes.len() {
        let Some(off) = bytes[start..].windows(needle.len()).position(|w| w == needle) else {
            break;
        };
        let s = start + off + 1;
        let e = s + needle.len() - 1;
        let skip = line == sel_line && s == sel_col1 && e + 1 == sel_col2;
        let left_ok = s == 1 || !is_word_char_byte(bytes[s - 2]);
        let right_ok = e >= bytes.len() || !is_word_char_byte(bytes[e]);
        if !skip && left_ok && right_ok {
            let x1: f64 = this.call_method("get_col_x_offset", (line, s))?;
            let x2: f64 = this.call_method("get_col_x_offset", (line, e + 1))?;
            renderer_draw_rect(lua, (x + x1, y, x2 - x1, lh, color.clone()))?;
        }
        start = e;
    }
    Ok(())
}

fn docview_draw_line_body(lua: &Lua, (this, line, x, y): (LuaTable, usize, f64, f64)) -> LuaResult<f64> {
    let config = require_table(lua, "core.config")?;
    let doc: LuaTable = this.get("doc")?;
    let lh = docview_get_line_height(lua, &this)?;
    let style = require_table(lua, "core.style")?;
    let is_wrapped = super::linewrap::is_active(&this)?;

    let (idx0, _, count, _) = if is_wrapped {
        super::linewrap::get_line_idx_col_count(&this, line, None, false)?
    } else {
        (line, 1, 1, 1)
    };

    let selections = selection_ranges(&doc, false)?;
    let mut draw_highlight = false;
    let hcl: LuaValue = config.get("highlight_current_line")?;
    if !matches!(hcl, LuaValue::Boolean(false)) {
        for (line1, col1, line2, col2) in &selections {
            if *line1 == line {
                if let LuaValue::String(s) = &hcl {
                    if s.to_str()? == "no_selection" && (*line1 != *line2 || *col1 != *col2) {
                        draw_highlight = false;
                        break;
                    }
                }
                draw_highlight = true;
                break;
            }
        }
    }
    if draw_highlight && active_view_is(lua, &this)? {
        let scroll: LuaTable = this.get("scroll")?;
        let sx: f64 = scroll.get("x")?;
        if is_wrapped {
            for i in 0..count {
                this.call_method::<()>("draw_line_highlight", (x + sx, y + lh * i as f64))?;
            }
        } else {
            this.call_method::<()>("draw_line_highlight", (x + sx, y))?;
        }
    }
    if active_view_is(lua, &this)? {
        this.call_method::<()>("draw_selection_matches", (line, x, y))?;
    }

    for (line1, mut col1, line2, mut col2) in selection_ranges(&doc, true)? {
        if line >= line1 && line <= line2 {
            let text = line_text(&doc, line)?;
            if line1 != line { col1 = 1; }
            if line2 != line { col2 = text.len() + 1; }
            if col1 == col2 { continue; }

            if is_wrapped {
                let (idx1, _, _, _) = super::linewrap::get_line_idx_col_count(&this, line, Some(col1), false)?;
                let (idx2, _, _, _) = super::linewrap::get_line_idx_col_count(&this, line, Some(col2), false)?;
                let mut start_col_acc = 0usize;
                for i in idx1..=idx2 {
                    let x1 = if i == idx1 {
                        x + this.call_method::<f64>("get_col_x_offset", (line, col1))?
                    } else {
                        x
                    };
                    let x2 = if i == idx2 {
                        x + this.call_method::<f64>("get_col_x_offset", (line, col2))?
                    } else {
                        start_col_acc += super::linewrap::get_idx_line_length(&this, i)?;
                        x + this.call_method::<f64>("get_col_x_offset", (line, start_col_acc + 1, true))?
                    };
                    let row_y = y + (i - idx0) as f64 * lh;
                    renderer_draw_rect(lua, (x1, row_y, (x2 - x1).max(0.0), lh, style.get::<LuaValue>("selection")?))?;
                }
            } else {
                let x1: f64 = this.call_method("get_col_x_offset", (line, col1))?;
                let x2: f64 = this.call_method("get_col_x_offset", (line, col2))?;
                if x1 != x2 {
                    renderer_draw_rect(lua, (x + x1, y, x2 - x1, lh, style.get::<LuaValue>("selection")?))?;
                }
            }
        }
    }

    let result = this.call_method::<f64>("draw_line_text", (line, x, y))?;
    if let Some(bm_pos) = this.get::<Option<LuaTable>>("_bm_pos")? {
        let style = require_table(lua, "core.style")?;
        let plugins: Option<LuaTable> = config.get::<LuaTable>("plugins").ok().and_then(|p| p.get("bracketmatch").ok().flatten());
        let color = plugins
            .as_ref()
            .and_then(|t| t.get::<Option<LuaValue>>("highlight_color").ok().flatten())
            .filter(|v| !matches!(v, LuaValue::Nil))
            .unwrap_or_else(|| style.get("caret").unwrap_or(LuaValue::Nil));
        let scale: f64 = lua.globals().get("SCALE").unwrap_or(1.0_f64);
        let uw = (2.0 * scale).floor().max(2.0);
        for i in [1usize, 3usize] {
            let bm_line: usize = bm_pos.get(i)?;
            if bm_line == line {
                let bc: usize = bm_pos.get(i + 1)?;
                let x1: f64 = this.call_method("get_col_x_offset", (line, bc))?;
                let x2: f64 = this.call_method("get_col_x_offset", (line, bc + 1))?;
                renderer_draw_rect(lua, (x + x1, y + lh - uw, x2 - x1, uw, color.clone()))?;
            }
        }
    }
    Ok(result)
}

fn docview_draw_line_gutter(lua: &Lua, (this, line, x, y, width): (LuaTable, usize, f64, f64, f64)) -> LuaResult<f64> {
    let style = require_table(lua, "core.style")?;
    let doc: LuaTable = this.get("doc")?;
    let mut color: LuaValue = style.get("line_number")?;
    for (line1, _, line2, _) in selection_ranges(&doc, true)? {
        if line >= line1 && line <= line2 {
            color = style.get("line_number2")?;
            break;
        }
    }
    let padding_x: f64 = style.get::<LuaTable>("padding")?.get("x")?;
    let common = require_table(lua, "core.common")?;
    let lh = docview_get_line_height(lua, &this)?;
    common.call_function::<()>(
        "draw_text",
        (docview_get_font(lua, &this)?, color, line, "right", x + padding_x, y, width, lh),
    )?;
    if super::linewrap::is_active(&this)? {
        let (_, _, count, _) = super::linewrap::get_line_idx_col_count(&this, line, None, false)?;
        return Ok(lh * count as f64);
    }
    Ok(lh)
}

fn docview_draw_ime_decoration(
    lua: &Lua,
    (this, line1, col1, line2, col2): (LuaTable, usize, usize, usize, usize),
) -> LuaResult<()> {
    let (x, y): (f64, f64) = this.call_method("get_line_screen_position", (line1, LuaValue::Nil))?;
    let style = require_table(lua, "core.style")?;
    let mut line_size = 1.0f64.max(lua.globals().get::<f64>("SCALE")?);
    let lh = docview_get_line_height(lua, &this)?;
    let mut x1: f64 = this.call_method("get_col_x_offset", (line1, col1))?;
    let mut x2: f64 = this.call_method("get_col_x_offset", (line2, col2))?;
    renderer_draw_rect(
        lua,
        (
            x + x1.min(x2),
            y + lh - line_size,
            (x1 - x2).abs(),
            line_size,
            style.get::<LuaValue>("text")?,
        ),
    )?;
    let col = col1.min(col2);
    let ime_selection: LuaTable = this.get("ime_selection")?;
    let from = (col as i64 + ime_selection.get::<i64>("from")?) as usize;
    let to = (from as i64 + ime_selection.get::<i64>("size")?) as usize;
    x1 = this.call_method("get_col_x_offset", (line1, from))?;
    if from != to {
        x2 = this.call_method("get_col_x_offset", (line1, to))?;
        line_size = style.get("caret_width")?;
        renderer_draw_rect(
            lua,
            (
                x + x1.min(x2),
                y + lh - line_size,
                (x1 - x2).abs(),
                line_size,
                style.get::<LuaValue>("caret")?,
            ),
        )?;
    }
    this.call_method::<()>("draw_caret", (x + x1, y))
}

fn docview_draw_overlay(lua: &Lua, this: LuaTable) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let class = self_class(&this)?;
    let docview_class = require_table(lua, "core.docview")?;
    if class.equals(&docview_class)?
        && config.get::<bool>("long_line_indicator")?
        && config.get::<i64>("line_limit")? > 0
    {
        let line_x: f64 = this.call_method("get_line_screen_position", (1, LuaValue::Nil))?;
        let font = docview_get_font(lua, &this)?;
        let character_width: f64 = font_call_method(&font, "get_width", "n")?;
        let x = line_x + character_width * config.get::<f64>("line_limit")?;
        let position: LuaTable = this.get("position")?;
        let style = require_table(lua, "core.style")?;
        let color = style
            .get::<Option<LuaValue>>("guide")?
            .unwrap_or(style.get("selection")?);
        renderer_draw_rect(
            lua,
            (
                x,
                position.get::<f64>("y")?,
                config.get::<f64>("long_line_indicator_width")?.max(1.0),
                this.get::<LuaTable>("size")?.get::<f64>("y")?,
                color,
            ),
        )?;
    }
    if active_view_is(lua, &this)? {
        let (minline, maxline): (usize, usize) = this.call_method("get_visible_line_range", ())?;
        let period: f64 = config.get("blink_period")?;
        let doc: LuaTable = this.get("doc")?;
        let core = require_table(lua, "core")?;
        let system = require_table(lua, "system")?;
        let ime = require_table(lua, "core.ime")?;
        for (line1, col1, line2, col2) in selection_ranges(&doc, false)? {
            if line1 >= minline
                && line1 <= maxline
                && system.call_function::<bool>("window_has_focus", core.get::<LuaValue>("window")?)?
            {
                if ime.get::<bool>("editing")? {
                    this.call_method::<()>("draw_ime_decoration", (line1, col1, line2, col2))?;
                } else if config.get::<bool>("disable_blink")?
                    || ((core.get::<f64>("blink_timer")? - core.get::<f64>("blink_start")?) % period) < period / 2.0
                {
                    let (x, y): (f64, f64) = this.call_method("get_line_screen_position", (line1, col1))?;
                    if doc.get::<bool>("overwrite")? {
                        let ch: LuaString = doc.call_method("get_char", (line1, col1))?;
                        let width: f64 = font_call_method(&docview_get_font(lua, &this)?, "get_width", ch)?;
                        this.call_method::<()>("draw_overwrite_caret", (x, y, width))?;
                    } else {
                        this.call_method::<()>("draw_caret", (x, y))?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn docview_draw(lua: &Lua, this: LuaTable) -> LuaResult<()> {
    let doc: LuaTable = this.get("doc")?;
    doc.call_method::<()>("ensure_loaded", ())?;
    let style = require_table(lua, "core.style")?;
    this.call_method::<()>("draw_background", style.get::<LuaValue>("background")?)?;
    let (_, indent_size): (LuaValue, usize) = doc.call_method("get_indent_info", ())?;
    let font = docview_get_font(lua, &this)?;
    font_call_method::<()>(&font, "set_tab_size", indent_size)?;
    let (minline, maxline): (usize, usize) = this.call_method("get_visible_line_range", ())?;
    let lh = docview_get_line_height(lua, &this)?;
    let (_, mut y): (f64, f64) = this.call_method("get_line_screen_position", (minline, LuaValue::Nil))?;
    let (gw, gpad) = gutter_width_from_method(&this)?;
    for i in minline..=maxline {
        let dy: f64 = this.call_method("draw_line_gutter", (i, this.get::<LuaTable>("position")?.get::<f64>("x")?, y, gw - gpad))?;
        y += if dy == 0.0 { lh } else { dy };
    }
    let pos: LuaTable = this.get("position")?;
    let (x, mut y): (f64, f64) = this.call_method("get_line_screen_position", (minline, LuaValue::Nil))?;
    let core = require_table(lua, "core")?;
    core.call_function::<()>(
        "push_clip_rect",
        (
            pos.get::<f64>("x")? + gw,
            pos.get::<f64>("y")?,
            this.get::<LuaTable>("size")?.get::<f64>("x")? - gw,
            this.get::<LuaTable>("size")?.get::<f64>("y")?,
        ),
    )?;
    for i in minline..=maxline {
        let dy: f64 = this.call_method("draw_line_body", (i, x, y))?;
        y += if dy == 0.0 { lh } else { dy };
    }
    this.call_method::<()>("draw_overlay", ())?;
    core.call_function::<()>("pop_clip_rect", ())?;
    if super::linewrap::is_active(&this)? {
        super::linewrap::draw_guide(lua, &this)?;
    }
    this.call_method::<()>("draw_scrollbar", ())
}

fn docview_on_context_menu(lua: &Lua, this: LuaTable) -> LuaResult<LuaMultiValue> {
    let class = self_class(&this)?;
    let divider: LuaValue = class.get("_context_menu_divider")?;
    let items = lua.create_table()?;
    let mk = |lua: &Lua, text: &str, command: &str| -> LuaResult<LuaTable> {
        let t = lua.create_table()?;
        t.set("text", text)?;
        t.set("command", command)?;
        Ok(t)
    };
    items.set(1, mk(lua, "Cut", "doc:cut")?)?;
    items.set(2, mk(lua, "Copy", "doc:copy")?)?;
    items.set(3, mk(lua, "Paste", "doc:paste")?)?;
    items.set(4, divider.clone())?;
    items.set(5, mk(lua, "Add Next Occurrence", "find-replace:select-add-next")?)?;
    items.set(6, mk(lua, "Add All Occurrences", "find-replace:select-add-all")?)?;
    items.set(7, divider)?;
    items.set(8, mk(lua, "Find", "find-replace:find")?)?;
    items.set(9, mk(lua, "Replace", "find-replace:replace")?)?;
    let details = lua.create_table()?;
    details.set("items", items)?;
    let mut out = LuaMultiValue::new();
    out.push_back(LuaValue::Table(details));
    out.push_back(LuaValue::Table(this));
    Ok(out)
}

fn populate_class(lua: &Lua, class: LuaTable) -> LuaResult<()> {
    let translate = lua.create_table()?;
    translate.set("previous_page", lua.create_function(translate_previous_page)?)?;
    translate.set("next_page", lua.create_function(translate_next_page)?)?;
    translate.set("previous_line", lua.create_function(translate_previous_line)?)?;
    translate.set("next_line", lua.create_function(translate_next_line)?)?;
    class.set("translate", translate)?;
    class.set("new", lua.create_function(docview_new)?)?;
    class.set("try_close", lua.create_function(docview_try_close)?)?;
    class.set("get_name", lua.create_function(docview_get_name)?)?;
    class.set("get_filename", lua.create_function(docview_get_filename)?)?;
    class.set("get_scrollable_size", lua.create_function(docview_get_scrollable_size)?)?;
    class.set("get_h_scrollable_size", lua.create_function(docview_get_h_scrollable_size)?)?;
    class.set("get_font", lua.create_function(docview_get_font_lua)?)?;
    class.set("get_line_height", lua.create_function(docview_get_line_height_lua)?)?;
    class.set("get_gutter_width", lua.create_function(docview_get_gutter_width_lua)?)?;
    class.set(
        "get_line_screen_position",
        lua.create_function(docview_get_line_screen_position_lua)?,
    )?;
    class.set(
        "get_line_text_y_offset",
        lua.create_function(docview_get_line_text_y_offset)?,
    )?;
    class.set(
        "get_visible_line_range",
        lua.create_function(docview_get_visible_line_range_lua)?,
    )?;
    class.set("get_col_x_offset", lua.create_function(docview_get_col_x_offset_lua)?)?;
    class.set("get_x_offset_col", lua.create_function(docview_get_x_offset_col_lua)?)?;
    class.set(
        "resolve_screen_position",
        lua.create_function(docview_resolve_screen_position)?,
    )?;
    class.set("scroll_to_line", lua.create_function(docview_scroll_to_line)?)?;
    class.set(
        "supports_text_input",
        lua.create_function(docview_supports_text_input)?,
    )?;
    class.set(
        "scroll_to_make_visible",
        lua.create_function(docview_scroll_to_make_visible)?,
    )?;
    class.set("on_mouse_moved", lua.create_function(docview_on_mouse_moved)?)?;
    class.set("mouse_selection", lua.create_function(docview_mouse_selection)?)?;
    class.set("on_mouse_pressed", lua.create_function(docview_on_mouse_pressed)?)?;
    class.set("on_mouse_released", lua.create_function(docview_on_mouse_released)?)?;
    class.set("on_text_input", lua.create_function(docview_on_text_input)?)?;
    class.set(
        "on_ime_text_editing",
        lua.create_function(docview_on_ime_text_editing)?,
    )?;
    class.set(
        "update_ime_location",
        lua.create_function(docview_update_ime_location)?,
    )?;
    class.set("update", lua.create_function(docview_update)?)?;
    class.set(
        "draw_line_highlight",
        lua.create_function(docview_draw_line_highlight)?,
    )?;
    class.set("draw_line_text", lua.create_function(docview_draw_line_text)?)?;
    class.set(
        "draw_overwrite_caret",
        lua.create_function(docview_draw_overwrite_caret)?,
    )?;
    class.set("draw_caret", lua.create_function(docview_draw_caret)?)?;
    class.set(
        "draw_selection_matches",
        lua.create_function(docview_draw_selection_matches)?,
    )?;
    class.set("draw_line_body", lua.create_function(docview_draw_line_body)?)?;
    class.set("draw_line_gutter", lua.create_function(docview_draw_line_gutter)?)?;
    class.set(
        "draw_ime_decoration",
        lua.create_function(docview_draw_ime_decoration)?,
    )?;
    class.set("draw_overlay", lua.create_function(docview_draw_overlay)?)?;
    class.set("draw", lua.create_function(docview_draw)?)?;
    class.set("on_context_menu", lua.create_function(docview_on_context_menu)?)?;
    Ok(())
}

fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set("populate", lua.create_function(|lua, class: LuaTable| populate_class(lua, class))?)?;
    Ok(module)
}

/// Registers "docview_native" (Rust methods) and "core.docview" (minimal bootstrap).
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    preload.set(
        "docview_native",
        lua.create_function(|lua, ()| make_module(lua))?,
    )?;
    preload.set(
        "core.docview",
        lua.create_function(|lua, ()| {
            let view: LuaTable = require_table(lua, "core.view")?;
            let context_menu: LuaTable = require_table(lua, "core.contextmenu")?;
            let docview: LuaTable = view.call_method("extend", ())?;
            docview.set("__tostring", lua.create_function(|_, _: LuaValue| Ok("DocView"))?)?;
            docview.set("context", "session")?;
            let divider: LuaValue = context_menu.get("DIVIDER")?;
            docview.set("_context_menu_divider", divider)?;
            let native: LuaTable = require_table(lua, "docview_native")?;
            let populate: LuaFunction = native.get("populate")?;
            populate.call::<()>(docview.clone())?;
            Ok(LuaValue::Table(docview))
        })?,
    )?;
    Ok(())
}
