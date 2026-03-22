use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn doc_folds(lua: &Lua, doc: &LuaTable) -> LuaResult<LuaTable> {
    let folds: LuaValue = doc.get("folds")?;
    match folds {
        LuaValue::Table(t) => Ok(t),
        _ => {
            let t = lua.create_table()?;
            doc.set("folds", t.clone())?;
            Ok(t)
        }
    }
}

fn has_active_folds(doc: &LuaTable) -> bool {
    let folds: LuaValue = doc.get("folds").unwrap_or(LuaValue::Nil);
    match folds {
        LuaValue::Table(t) => t.pairs::<LuaValue, LuaValue>().next().is_some(),
        _ => false,
    }
}

fn get_fold_end(lua: &Lua, doc: &LuaTable, line: i64) -> LuaResult<Option<i64>> {
    let affordance = require_table(lua, "affordance_model")?;
    let lines: LuaTable = doc.get("lines")?;
    affordance.call_function("get_fold_end", (lines, line))
}

fn visible_line_count(lua: &Lua, doc: &LuaTable) -> LuaResult<i64> {
    let affordance = require_table(lua, "affordance_model")?;
    let lines: LuaTable = doc.get("lines")?;
    let num_lines = lines.raw_len() as i64;
    let folds = doc_folds(lua, doc)?;
    affordance.call_function("visible_line_count", (num_lines, folds))
}

fn actual_to_visible(lua: &Lua, doc: &LuaTable, line: i64) -> LuaResult<i64> {
    let affordance = require_table(lua, "affordance_model")?;
    let folds = doc_folds(lua, doc)?;
    affordance.call_function("actual_to_visible", (line, folds))
}

fn visible_to_actual(lua: &Lua, doc: &LuaTable, visible: i64) -> LuaResult<i64> {
    let affordance = require_table(lua, "affordance_model")?;
    let lines: LuaTable = doc.get("lines")?;
    let num_lines = lines.raw_len() as i64;
    let folds = doc_folds(lua, doc)?;
    affordance.call_function("visible_to_actual", (visible, num_lines, folds))
}

fn next_visible_line(lua: &Lua, doc: &LuaTable, line: i64) -> LuaResult<i64> {
    let affordance = require_table(lua, "affordance_model")?;
    let folds = doc_folds(lua, doc)?;
    affordance.call_function("next_visible_line", (line, folds))
}

fn toggle_fold(lua: &Lua, doc: &LuaTable, line: i64) -> LuaResult<()> {
    let folds = doc_folds(lua, doc)?;
    let existing: LuaValue = folds.get(line)?;
    if !matches!(existing, LuaValue::Nil) {
        folds.set(line, LuaValue::Nil)?;
        return Ok(());
    }
    if let Some(end_line) = get_fold_end(lua, doc, line)? {
        folds.set(line, end_line)?;
    }
    Ok(())
}

fn save_doc_folds(lua: &Lua, doc: &LuaTable) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let folding_cfg: LuaTable = plugins.get("folding")?;
    let persist: bool = folding_cfg.get("persist").unwrap_or(false);
    let abs_filename: Option<String> = doc.get("abs_filename")?;
    if !persist || abs_filename.is_none() {
        return Ok(());
    }
    let folds = doc_folds(lua, doc)?;
    let mut folded: Vec<i64> = folds
        .pairs::<i64, LuaValue>()
        .flatten()
        .map(|(k, _)| k)
        .collect();
    folded.sort();
    let folded_table = lua.create_sequence_from(folded)?;
    let storage = require_table(lua, "core.storage")?;
    storage.call_function::<()>("save", ("folding", abs_filename.unwrap(), folded_table))?;
    Ok(())
}

fn load_doc_folds(lua: &Lua, doc: &LuaTable) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let folding_cfg: LuaTable = plugins.get("folding")?;
    let persist: bool = folding_cfg.get("persist").unwrap_or(false);
    let abs_filename: Option<String> = doc.get("abs_filename")?;
    if !persist || abs_filename.is_none() {
        return Ok(());
    }
    let folds = lua.create_table()?;
    doc.set("folds", folds.clone())?;
    let storage = require_table(lua, "core.storage")?;
    let loaded: LuaValue =
        storage.call_function("load", ("folding", abs_filename.unwrap()))?;
    if let LuaValue::Table(lines_table) = loaded {
        for pair in lines_table.sequence_values::<i64>() {
            let line = pair?;
            if let Some(end_line) = get_fold_end(lua, doc, line)? {
                folds.set(line, end_line)?;
            }
        }
    }
    Ok(())
}

fn set_config_defaults(lua: &Lua) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let common = require_table(lua, "core.common")?;
    let defaults = lua.create_table()?;
    defaults.set("persist", true)?;
    let merged: LuaTable =
        common.call_function("merge", (defaults, plugins.get::<LuaValue>("folding")?))?;
    plugins.set("folding", merged)?;
    Ok(())
}

fn patch_core_open_doc(lua: &Lua) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let old: LuaFunction = core.get("open_doc")?;
    let old_key = lua.create_registry_value(old)?;
    core.set(
        "open_doc",
        lua.create_function(move |lua, args: LuaMultiValue| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let result: LuaMultiValue = old.call(args)?;
            if let Some(LuaValue::Table(doc)) = result.front() {
                load_doc_folds(lua, doc)?;
            }
            Ok(result)
        })?,
    )?;
    Ok(())
}

fn patch_doc_methods(lua: &Lua) -> LuaResult<()> {
    let doc_class = require_table(lua, "core.doc")?;

    {
        let old: LuaFunction = doc_class.get("on_close")?;
        let old_key = lua.create_registry_value(old)?;
        doc_class.set(
            "on_close",
            lua.create_function(move |lua, this: LuaTable| {
                save_doc_folds(lua, &this)?;
                let old: LuaFunction = lua.registry_value(&old_key)?;
                old.call::<()>(this)
            })?,
        )?;
    }

    {
        let old: LuaFunction = doc_class.get("on_text_change")?;
        let old_key = lua.create_registry_value(old)?;
        doc_class.set(
            "on_text_change",
            lua.create_function(move |lua, (this, change_type): (LuaTable, LuaValue)| {
                let is_selection = match &change_type {
                    LuaValue::String(s) => s.to_str()? == "selection",
                    _ => false,
                };
                if !is_selection {
                    this.set("folds", LuaValue::Nil)?;
                }
                let old: LuaFunction = lua.registry_value(&old_key)?;
                old.call::<()>((this, change_type))
            })?,
        )?;
    }

    Ok(())
}

fn patch_docview_methods(lua: &Lua) -> LuaResult<()> {
    let docview = require_table(lua, "core.docview")?;

    // get_scrollable_size
    {
        let old: LuaFunction = docview.get("get_scrollable_size")?;
        let old_key = lua.create_registry_value(old)?;
        docview.set(
            "get_scrollable_size",
            lua.create_function(move |lua, this: LuaTable| {
                let doc: LuaTable = this.get("doc")?;
                if !has_active_folds(&doc) {
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    return old.call::<f64>(this);
                }
                let h_scrollbar: LuaTable = this.get("h_scrollbar")?;
                let track: LuaMultiValue =
                    h_scrollbar.call_method("get_track_rect", ())?;
                let h_scroll = track.iter().nth(3).map(|v| match v {
                    LuaValue::Number(n) => *n,
                    LuaValue::Integer(n) => *n as f64,
                    _ => 0.0,
                }).unwrap_or(0.0);
                let line_height: f64 = this.call_method("get_line_height", ())?;
                let vc = visible_line_count(lua, &doc)? as f64;
                let config = require_table(lua, "core.config")?;
                let scroll_past_end: bool =
                    config.get("scroll_past_end").unwrap_or(false);
                let style = require_table(lua, "core.style")?;
                let padding: LuaTable = style.get("padding")?;
                let pad_y: f64 = padding.get("y")?;
                if !scroll_past_end {
                    Ok(line_height * vc + pad_y * 2.0 + h_scroll)
                } else {
                    let size: LuaTable = this.get("size")?;
                    let size_y: f64 = size.get("y")?;
                    Ok(line_height * (vc - 1.0).max(0.0) + size_y)
                }
            })?,
        )?;
    }

    // get_line_screen_position(line, col)
    {
        let old: LuaFunction = docview.get("get_line_screen_position")?;
        let old_key = lua.create_registry_value(old)?;
        docview.set(
            "get_line_screen_position",
            lua.create_function(
                move |lua, (this, line, col): (LuaTable, i64, LuaValue)| {
                    let doc: LuaTable = this.get("doc")?;
                    if !has_active_folds(&doc) {
                        let old: LuaFunction = lua.registry_value(&old_key)?;
                        return old.call::<LuaMultiValue>((this, line, col));
                    }
                    let (cx, cy): (f64, f64) =
                        this.call_method("get_content_offset", ())?;
                    let lh: f64 = this.call_method("get_line_height", ())?;
                    let (gw, _): (f64, f64) = this.call_method("get_gutter_width", ())?;
                    let style = require_table(lua, "core.style")?;
                    let padding: LuaTable = style.get("padding")?;
                    let pad_y: f64 = padding.get("y")?;
                    let y = cy + (actual_to_visible(lua, &doc, line)? - 1) as f64 * lh + pad_y;
                    match &col {
                        LuaValue::Nil | LuaValue::Boolean(false) => {
                            let mut mv = LuaMultiValue::new();
                            mv.push_back(LuaValue::Number(cx + gw));
                            mv.push_back(LuaValue::Number(y));
                            Ok(mv)
                        }
                        _ => {
                            let col_num = match &col {
                                LuaValue::Integer(n) => *n as f64,
                                LuaValue::Number(n) => *n,
                                _ => 0.0,
                            };
                            let col_x: f64 = this.call_method(
                                "get_col_x_offset",
                                (line, col_num),
                            )?;
                            let mut mv = LuaMultiValue::new();
                            mv.push_back(LuaValue::Number(cx + gw + col_x));
                            mv.push_back(LuaValue::Number(y));
                            Ok(mv)
                        }
                    }
                },
            )?,
        )?;
    }

    // get_visible_line_range
    {
        let old: LuaFunction = docview.get("get_visible_line_range")?;
        let old_key = lua.create_registry_value(old)?;
        docview.set(
            "get_visible_line_range",
            lua.create_function(move |lua, this: LuaTable| {
                let doc: LuaTable = this.get("doc")?;
                if !has_active_folds(&doc) {
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    return old.call::<(i64, i64)>(this);
                }
                let bounds: LuaMultiValue = this.call_method("get_content_bounds", ())?;
                let y = bounds.iter().nth(1).map(|v| match v {
                    LuaValue::Number(n) => *n,
                    LuaValue::Integer(n) => *n as f64,
                    _ => 0.0,
                }).unwrap_or(0.0);
                let y2 = bounds.iter().nth(3).map(|v| match v {
                    LuaValue::Number(n) => *n,
                    LuaValue::Integer(n) => *n as f64,
                    _ => 0.0,
                }).unwrap_or(0.0);
                let lh: f64 = this.call_method("get_line_height", ())?;
                let style = require_table(lua, "core.style")?;
                let padding: LuaTable = style.get("padding")?;
                let pad_y: f64 = padding.get("y")?;
                let vc = visible_line_count(lua, &doc)?;
                let min_vis = ((y - pad_y) / lh).floor() as i64 + 1;
                let min_vis = min_vis.max(1);
                let max_vis = ((y2 - pad_y) / lh).floor() as i64 + 1;
                let max_vis = max_vis.min(vc);
                let min_actual = visible_to_actual(lua, &doc, min_vis)?;
                let max_actual = visible_to_actual(lua, &doc, max_vis)?;
                Ok((min_actual, max_actual))
            })?,
        )?;
    }

    // resolve_screen_position(x, y)
    {
        let old: LuaFunction = docview.get("resolve_screen_position")?;
        let old_key = lua.create_registry_value(old)?;
        docview.set(
            "resolve_screen_position",
            lua.create_function(move |lua, (this, x, y): (LuaTable, f64, f64)| {
                let doc: LuaTable = this.get("doc")?;
                if !has_active_folds(&doc) {
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    return old.call::<(i64, i64)>((this, x, y));
                }
                let (ox, oy): (f64, f64) =
                    this.call_method("get_line_screen_position", (1i64, LuaValue::Nil))?;
                let lh: f64 = this.call_method("get_line_height", ())?;
                let vc = visible_line_count(lua, &doc)?;
                let visible = ((y - oy) / lh).floor() as i64 + 1;
                let visible = visible.clamp(1, vc);
                let line = visible_to_actual(lua, &doc, visible)?;
                let col: i64 = this.call_method("get_x_offset_col", (line, x - ox))?;
                Ok((line, col))
            })?,
        )?;
    }

    // draw
    {
        let old: LuaFunction = docview.get("draw")?;
        let old_key = lua.create_registry_value(old)?;
        docview.set(
            "draw",
            lua.create_function(move |lua, this: LuaTable| {
                let doc: LuaTable = this.get("doc")?;
                if !has_active_folds(&doc) {
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    return old.call::<()>(this);
                }
                let style = require_table(lua, "core.style")?;
                let background: LuaValue = style.get("background")?;
                this.call_method::<()>("draw_background", background)?;

                let (_, indent_size): (LuaValue, i64) =
                    doc.call_method("get_indent_info", ())?;
                let font: LuaValue = this.call_method("get_font", ())?;
                match &font {
                    LuaValue::Table(t) => t.call_method::<()>("set_tab_size", indent_size)?,
                    LuaValue::UserData(ud) => ud.call_method::<()>("set_tab_size", indent_size)?,
                    _ => {}
                }

                let (minline, maxline): (i64, i64) =
                    this.call_method("get_visible_line_range", ())?;
                let (gw, gpad): (f64, f64) = this.call_method("get_gutter_width", ())?;
                let inner_gw = if gpad != 0.0 { gw - gpad } else { gw };

                let position: LuaTable = this.get("position")?;
                let pos_x: f64 = position.get("x")?;

                let (_, mut y): (f64, f64) =
                    this.call_method("get_line_screen_position", (minline, LuaValue::Nil))?;

                let mut line = minline;
                while line <= maxline {
                    let drawn: f64 = this
                        .call_method("draw_line_gutter", (line, pos_x, y, inner_gw))?;
                    y += drawn;
                    line = next_visible_line(lua, &doc, line)?;
                }

                let (x, mut y): (f64, f64) =
                    this.call_method("get_line_screen_position", (minline, LuaValue::Nil))?;

                let core = require_table(lua, "core")?;
                let pos = this.get::<LuaTable>("position")?;
                let pos_y: f64 = pos.get("y")?;
                let size: LuaTable = this.get("size")?;
                let size_x: f64 = size.get("x")?;
                let size_y: f64 = size.get("y")?;
                core.call_function::<()>(
                    "push_clip_rect",
                    (pos_x + gw, pos_y, size_x - gw, size_y),
                )?;

                let mut line = minline;
                while line <= maxline {
                    let drawn: f64 = this.call_method("draw_line_body", (line, x, y))?;
                    y += drawn;
                    line = next_visible_line(lua, &doc, line)?;
                }

                this.call_method::<()>("draw_overlay", ())?;
                core.call_function::<()>("pop_clip_rect", ())?;
                this.call_method::<()>("draw_scrollbar", ())?;
                Ok(())
            })?,
        )?;
    }

    // draw_line_gutter(line, x, y, width)
    {
        let old: LuaFunction = docview.get("draw_line_gutter")?;
        let old_key = lua.create_registry_value(old)?;
        docview.set(
            "draw_line_gutter",
            lua.create_function(
                move |lua, (this, line, x, y, width): (LuaTable, i64, f64, f64, f64)| {
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    let lh: f64 = old.call((this.clone(), line, x, y, width))?;
                    let doc: LuaTable = this.get("doc")?;
                    if let Some(_end_line) = get_fold_end(lua, &doc, line)? {
                        let folds: LuaValue = doc.get("folds")?;
                        let icon = match &folds {
                            LuaValue::Table(t) => {
                                let fold_entry: LuaValue = t.get(line)?;
                                if matches!(fold_entry, LuaValue::Nil) { "v" } else { ">" }
                            }
                            _ => "v",
                        };
                        let style = require_table(lua, "core.style")?;
                        let icon_font: LuaValue = style.get("icon_font")?;
                        let dim: LuaValue = style.get("dim")?;
                        let common = require_table(lua, "core.common")?;
                        common.call_function::<()>(
                            "draw_text",
                            (icon_font, dim, icon, LuaValue::Nil, x + 2.0, y, 10.0, lh),
                        )?;
                    }
                    Ok(lh)
                },
            )?,
        )?;
    }

    // draw_line_text(line, x, y)
    {
        let old: LuaFunction = docview.get("draw_line_text")?;
        let old_key = lua.create_registry_value(old)?;
        docview.set(
            "draw_line_text",
            lua.create_function(
                move |lua, (this, line, x, y): (LuaTable, i64, f64, f64)| {
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    let lh: f64 = old.call((this.clone(), line, x, y))?;
                    let doc: LuaTable = this.get("doc")?;
                    let folds: LuaValue = doc.get("folds")?;
                    if let LuaValue::Table(folds_table) = &folds {
                        let end_line: LuaValue = folds_table.get(line)?;
                        if let LuaValue::Integer(end) = end_line {
                            let text = format!(" ... {} lines", end - line);
                            let font: LuaValue = this.call_method("get_font", ())?;
                            let col_x: f64 =
                                this.call_method("get_col_x_offset", (line, f64::INFINITY))?;
                            let style = require_table(lua, "core.style")?;
                            let padding: LuaTable = style.get("padding")?;
                            let pad_x: f64 = padding.get("x")?;
                            let text_y_offset: f64 =
                                this.call_method("get_line_text_y_offset", ())?;
                            let dim: LuaValue = style.get("dim")?;
                            let renderer: LuaTable = lua.globals().get("renderer")?;
                            renderer.call_function::<LuaValue>(
                                "draw_text",
                                (font, text, x + col_x + pad_x, y + text_y_offset, dim),
                            )?;
                        }
                    }
                    Ok(lh)
                },
            )?,
        )?;
    }

    // on_mouse_pressed(button, x, y, clicks)
    {
        let old: LuaFunction = docview.get("on_mouse_pressed")?;
        let old_key = lua.create_registry_value(old)?;
        docview.set(
            "on_mouse_pressed",
            lua.create_function(
                move |lua,
                      (this, button, mx, my, clicks): (
                    LuaTable,
                    LuaValue,
                    LuaValue,
                    LuaValue,
                    LuaValue,
                )| {
                    let is_left = match &button {
                        LuaValue::String(s) => s.as_bytes() == b"left",
                        _ => false,
                    };
                    let hovering: bool = this.get("hovering_gutter").unwrap_or(false);
                    if is_left && hovering {
                        let mx_f = match &mx {
                            LuaValue::Number(n) => *n,
                            LuaValue::Integer(n) => *n as f64,
                            _ => 0.0,
                        };
                        let my_f = match &my {
                            LuaValue::Number(n) => *n,
                            LuaValue::Integer(n) => *n as f64,
                            _ => 0.0,
                        };
                        let (line, _): (i64, i64) =
                            this.call_method("resolve_screen_position", (mx_f, my_f))?;
                        let position: LuaTable = this.get("position")?;
                        let pos_x: f64 = position.get("x")?;
                        let doc: LuaTable = this.get("doc")?;
                        if mx_f <= pos_x + 12.0 && get_fold_end(lua, &doc, line)?.is_some() {
                            toggle_fold(lua, &doc, line)?;
                            save_doc_folds(lua, &doc)?;
                            let core = require_table(lua, "core")?;
                            core.set("redraw", true)?;
                            return Ok(LuaValue::Boolean(true));
                        }
                    }
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    old.call::<LuaValue>((this, button, mx, my, clicks))
                },
            )?,
        )?;
    }

    // translate.previous_line and translate.next_line
    {
        let translate: LuaTable = docview.get("translate")?;

        translate.set(
            "previous_line",
            lua.create_function(
                |lua, (doc, line, _col, dv): (LuaTable, i64, LuaValue, LuaTable)| {
                    let visible = actual_to_visible(lua, &doc, line)?;
                    if visible <= 1 {
                        return Ok((1i64, 1i64));
                    }
                    let target = visible_to_actual(lua, &doc, visible - 1)?;
                    let last_x: Option<LuaTable> = dv.get("last_x_offset")?;
                    let offset: f64 = last_x
                        .and_then(|t| t.get::<Option<f64>>("offset").ok().flatten())
                        .unwrap_or(0.0);
                    let col: i64 = dv.call_method("get_x_offset_col", (target, offset))?;
                    Ok((target, col))
                },
            )?,
        )?;

        translate.set(
            "next_line",
            lua.create_function(
                |lua, (doc, line, _col, dv): (LuaTable, i64, LuaValue, LuaTable)| {
                    let visible = actual_to_visible(lua, &doc, line)?;
                    let vc = visible_line_count(lua, &doc)?;
                    if visible >= vc {
                        let lines: LuaTable = doc.get("lines")?;
                        let num_lines = lines.raw_len() as i64;
                        return Ok((num_lines, i64::MAX));
                    }
                    let target = visible_to_actual(lua, &doc, visible + 1)?;
                    let last_x: Option<LuaTable> = dv.get("last_x_offset")?;
                    let offset: f64 = last_x
                        .and_then(|t| t.get::<Option<f64>>("offset").ok().flatten())
                        .unwrap_or(0.0);
                    let col: i64 = dv.call_method("get_x_offset_col", (target, offset))?;
                    Ok((target, col))
                },
            )?,
        )?;
    }

    Ok(())
}

fn register_commands(lua: &Lua) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;
    let cmds = lua.create_table()?;
    cmds.set(
        "fold:toggle",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let selection: LuaMultiValue = doc.call_method("get_selection", ())?;
            let line = match selection.front() {
                Some(LuaValue::Integer(n)) => *n,
                Some(LuaValue::Number(n)) => *n as i64,
                _ => return Ok(()),
            };
            toggle_fold(lua, &doc, line)?;
            save_doc_folds(lua, &doc)?;
            Ok(())
        })?,
    )?;
    command.call_function::<()>("add", ("core.docview", cmds))?;

    let keymap = require_table(lua, "core.keymap")?;
    let bindings = lua.create_table()?;
    bindings.set("ctrl+alt+[", "fold:toggle")?;
    keymap.call_function::<()>("add", bindings)?;

    Ok(())
}

/// Registers `plugins.folding`: fold persistence, DocView method patches for
/// fold-aware layout, and `fold:toggle` command with `ctrl+alt+[` keymap.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.folding",
        lua.create_function(|lua, ()| {
            set_config_defaults(lua)?;
            patch_core_open_doc(lua)?;
            patch_doc_methods(lua)?;
            patch_docview_methods(lua)?;
            register_commands(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )
}
