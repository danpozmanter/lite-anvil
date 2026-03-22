use mlua::prelude::*;
use std::sync::Arc;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn lua_eq(a: &LuaValue, b: &LuaValue) -> bool {
    match (a, b) {
        (LuaValue::Nil, LuaValue::Nil) => true,
        (LuaValue::Boolean(x), LuaValue::Boolean(y)) => x == y,
        (LuaValue::Integer(x), LuaValue::Integer(y)) => x == y,
        (LuaValue::Number(x), LuaValue::Number(y)) => x == y,
        (LuaValue::Integer(x), LuaValue::Number(y)) => (*x as f64) == *y,
        (LuaValue::Number(x), LuaValue::Integer(y)) => *x == (*y as f64),
        (LuaValue::String(x), LuaValue::String(y)) => x.as_bytes() == y.as_bytes(),
        (LuaValue::Table(x), LuaValue::Table(y)) => x == y,
        (LuaValue::UserData(x), LuaValue::UserData(y)) => x == y,
        _ => false,
    }
}

fn make_weak_table(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.set("__mode", "k")?;
    t.set_metatable(Some(mt))?;
    Ok(t)
}

fn snapshot_settings(lua: &Lua, state: &LuaTable) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let dw: LuaTable = plugins.get("drawwhitespace")?;
    let snap = lua.create_table()?;
    for key in &[
        "show_leading",
        "show_trailing",
        "show_middle",
        "show_middle_min",
        "color",
        "leading_color",
        "middle_color",
        "trailing_color",
        "substitutions",
    ] {
        snap.set(*key, dw.get::<LuaValue>(*key)?)?;
    }
    state.set("cached_settings", snap)?;
    Ok(())
}

fn reset_cache(lua: &Lua, state: &LuaTable) -> LuaResult<()> {
    state.set("ws_cache", make_weak_table(lua)?)?;
    snapshot_settings(lua, state)?;
    Ok(())
}

fn settings_changed(lua: &Lua, state: &LuaTable) -> LuaResult<bool> {
    let cached: LuaTable = state.get("cached_settings")?;
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let dw: LuaTable = plugins.get("drawwhitespace")?;
    for key in &[
        "show_leading",
        "show_trailing",
        "show_middle",
        "show_middle_min",
        "color",
        "leading_color",
        "middle_color",
        "trailing_color",
        "substitutions",
    ] {
        let current: LuaValue = dw.get(*key)?;
        let cached_val: LuaValue = cached.get(*key)?;
        if !lua_eq(&current, &cached_val) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn get_or_create_hl_cache(
    lua: &Lua,
    ws_cache: &LuaTable,
    hl: &LuaTable,
) -> LuaResult<LuaTable> {
    match ws_cache.raw_get::<LuaValue>(hl.clone())? {
        LuaValue::Table(t) => Ok(t),
        _ => {
            let t = lua.create_table()?;
            ws_cache.raw_set(hl.clone(), t.clone())?;
            Ok(t)
        }
    }
}

fn font_get_size(font: &LuaValue) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_size", ()),
        LuaValue::UserData(ud) => ud.call_method("get_size", ()),
        _ => Ok(0.0),
    }
}

fn font_get_width(font: &LuaValue, s: &str) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_width", s),
        LuaValue::UserData(ud) => ud.call_method("get_width", s),
        _ => Ok(0.0),
    }
}

/// Returns 1-based (start, end inclusive) spans of `ch` in `text`.
fn find_byte_runs(text: &[u8], ch: u8) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if text[i] == ch {
            let start = i + 1;
            while i < text.len() && text[i] == ch {
                i += 1;
            }
            runs.push((start, i)); // end is 1-based inclusive
        } else {
            i += 1;
        }
    }
    runs
}

fn set_config_defaults(lua: &Lua) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let common = require_table(lua, "core.common")?;
    let style = require_table(lua, "core.style")?;

    let syntax: LuaTable = style.get("syntax")?;
    let ws_color: LuaValue = syntax
        .get::<LuaValue>("whitespace")
        .ok()
        .filter(|v| !matches!(v, LuaValue::Nil))
        .unwrap_or_else(|| syntax.get::<LuaValue>("comment").unwrap_or(LuaValue::Nil));

    let defaults = lua.create_table()?;
    defaults.set("enabled", false)?;
    defaults.set("show_leading", true)?;
    defaults.set("show_trailing", true)?;
    defaults.set("show_middle", true)?;
    defaults.set("show_selected_only", false)?;
    defaults.set("show_middle_min", 1)?;
    defaults.set("color", ws_color)?;
    defaults.set("leading_color", LuaValue::Nil)?;
    defaults.set("middle_color", LuaValue::Nil)?;
    defaults.set("trailing_color", LuaValue::Nil)?;

    let substitutions = lua.create_table()?;
    let space_sub = lua.create_table()?;
    space_sub.set("char", " ")?;
    space_sub.set("sub", "\u{00B7}")?; // ·
    substitutions.push(space_sub)?;
    let tab_sub = lua.create_table()?;
    tab_sub.set("char", "\t")?;
    tab_sub.set("sub", "\u{00BB}")?; // »
    substitutions.push(tab_sub)?;
    defaults.set("substitutions", substitutions)?;

    let spec = lua.create_table()?;
    spec.set("name", "Draw Whitespace")?;

    let mk_toggle = |lua: &Lua, label: &str, desc: &str, path: &str, default: bool| -> LuaResult<LuaTable> {
        let e = lua.create_table()?;
        e.set("label", label)?;
        e.set("description", desc)?;
        e.set("path", path)?;
        e.set("type", "toggle")?;
        e.set("default", default)?;
        Ok(e)
    };

    spec.push(mk_toggle(lua, "Enabled", "Disable or enable the drawing of white spaces.", "enabled", false)?)?;
    spec.push(mk_toggle(lua, "Show Leading", "Draw whitespaces starting at the beginning of a line.", "show_leading", true)?)?;
    spec.push(mk_toggle(lua, "Show Middle", "Draw whitespaces on the middle of a line.", "show_middle", true)?)?;
    spec.push(mk_toggle(lua, "Show Trailing", "Draw whitespaces on the end of a line.", "show_trailing", true)?)?;
    spec.push(mk_toggle(lua, "Show Selected Only", "Only draw whitespaces if it is within a selection.", "show_selected_only", false)?)?;

    let trailing_error_entry = lua.create_table()?;
    trailing_error_entry.set("label", "Show Trailing as Error")?;
    trailing_error_entry.set(
        "description",
        "Uses an error square to spot them easily, requires 'Show Trailing' enabled.",
    )?;
    trailing_error_entry.set("path", "show_trailing_error")?;
    trailing_error_entry.set("type", "toggle")?;
    trailing_error_entry.set("default", false)?;
    trailing_error_entry.set(
        "on_apply",
        lua.create_function(|lua, enabled: bool| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let dw: LuaTable = plugins.get("drawwhitespace")?;
            let substitutions: LuaTable = dw.get("substitutions")?;
            let n = substitutions.raw_len() as i64;
            let mut found_idx: Option<i64> = None;
            for i in 1..=n {
                let sub: LuaTable = substitutions.get(i)?;
                let te: LuaValue = sub.get("trailing_error")?;
                if !matches!(te, LuaValue::Nil | LuaValue::Boolean(false)) {
                    found_idx = Some(i);
                }
            }
            if found_idx.is_none() && enabled {
                let style = require_table(lua, "core.style")?;
                let error_color: LuaValue = style.get("error")?;
                let new_sub = lua.create_table()?;
                new_sub.set("char", " ")?;
                new_sub.set("sub", "\u{2588}")?; // █
                new_sub.set("show_leading", false)?;
                new_sub.set("show_middle", false)?;
                new_sub.set("show_trailing", true)?;
                new_sub.set("trailing_color", error_color)?;
                new_sub.set("trailing_error", true)?;
                substitutions.set(n + 1, new_sub)?;
            } else if let (Some(idx), false) = (found_idx, enabled) {
                let n = substitutions.raw_len() as i64;
                for i in idx..n {
                    let next: LuaValue = substitutions.get(i + 1)?;
                    substitutions.set(i, next)?;
                }
                substitutions.set(n, LuaValue::Nil)?;
            }
            Ok(())
        })?,
    )?;
    spec.push(trailing_error_entry)?;
    defaults.set("config_spec", spec)?;

    let merged: LuaTable =
        common.call_function("merge", (defaults, plugins.get::<LuaValue>("drawwhitespace")?))?;
    plugins.set("drawwhitespace", merged)?;
    Ok(())
}

fn patch_highlighter_methods(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let highlighter = require_table(lua, "core.doc.highlighter")?;

    {
        let old: LuaFunction = highlighter.get("insert_notify")?;
        let old_key = lua.create_registry_value(old)?;
        let sk = state_key.clone();
        highlighter.set(
            "insert_notify",
            lua.create_function(
                move |lua, (this, line, n, rest): (LuaTable, i64, i64, LuaMultiValue)| {
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    let mut args = LuaMultiValue::new();
                    args.push_back(LuaValue::Table(this.clone()));
                    args.push_back(LuaValue::Integer(line));
                    args.push_back(LuaValue::Integer(n));
                    args.extend(rest);
                    old.call::<()>(args)?;

                    let state: LuaTable = lua.registry_value(&sk)?;
                    let ws_cache: LuaTable = state.get("ws_cache")?;
                    let hl_cache = get_or_create_hl_cache(lua, &ws_cache, &this)?;

                    let doc: LuaTable = this.get("doc")?;
                    let lines: LuaTable = doc.get("lines")?;
                    let num_lines = lines.raw_len() as i64;
                    let to = (line + n).min(num_lines);

                    let mut i = num_lines + n;
                    while i >= to {
                        let val: LuaValue = hl_cache.get(i - n)?;
                        hl_cache.set(i, val)?;
                        i -= 1;
                    }
                    let mut i = line;
                    while i <= to {
                        hl_cache.set(i, LuaValue::Nil)?;
                        i += 1;
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let old: LuaFunction = highlighter.get("remove_notify")?;
        let old_key = lua.create_registry_value(old)?;
        let sk = state_key.clone();
        highlighter.set(
            "remove_notify",
            lua.create_function(
                move |lua, (this, line, n, rest): (LuaTable, i64, i64, LuaMultiValue)| {
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    let mut args = LuaMultiValue::new();
                    args.push_back(LuaValue::Table(this.clone()));
                    args.push_back(LuaValue::Integer(line));
                    args.push_back(LuaValue::Integer(n));
                    args.extend(rest);
                    old.call::<()>(args)?;

                    let state: LuaTable = lua.registry_value(&sk)?;
                    let ws_cache: LuaTable = state.get("ws_cache")?;
                    let hl_cache = get_or_create_hl_cache(lua, &ws_cache, &this)?;

                    let doc: LuaTable = this.get("doc")?;
                    let lines: LuaTable = doc.get("lines")?;
                    let num_lines = lines.raw_len() as i64;
                    let to = (line + n).max(num_lines);

                    let mut i = line;
                    while i <= to {
                        let val: LuaValue = hl_cache.get(i + n)?;
                        hl_cache.set(i, val)?;
                        i += 1;
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let old: LuaFunction = highlighter.get("update_notify")?;
        let old_key = lua.create_registry_value(old)?;
        let sk = state_key.clone();
        highlighter.set(
            "update_notify",
            lua.create_function(
                move |lua, (this, line, n, rest): (LuaTable, i64, i64, LuaMultiValue)| {
                    let old: LuaFunction = lua.registry_value(&old_key)?;
                    let mut args = LuaMultiValue::new();
                    args.push_back(LuaValue::Table(this.clone()));
                    args.push_back(LuaValue::Integer(line));
                    args.push_back(LuaValue::Integer(n));
                    args.extend(rest);
                    old.call::<()>(args)?;

                    let state: LuaTable = lua.registry_value(&sk)?;
                    let ws_cache: LuaTable = state.get("ws_cache")?;
                    let hl_cache = get_or_create_hl_cache(lua, &ws_cache, &this)?;

                    let mut i = line;
                    while i <= line + n {
                        hl_cache.set(i, LuaValue::Nil)?;
                        i += 1;
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    Ok(())
}

fn patch_draw_line_text(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let docview = require_table(lua, "core.docview")?;
    let old: LuaFunction = docview.get("draw_line_text")?;
    let old_key = Arc::new(lua.create_registry_value(old)?);
    let sk = state_key;

    docview.set(
        "draw_line_text",
        lua.create_function(move |lua, (this, idx, x, y): (LuaTable, i64, f64, f64)| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let dw: LuaTable = plugins.get("drawwhitespace")?;
            let enabled: bool = dw.get("enabled").unwrap_or(false);

            // Only draw for exact DocView instances (not subclasses like TerminalView).
            let docview_class = require_table(lua, "core.docview")?;
            let getmetatable: LuaFunction = lua.globals().get("getmetatable")?;
            let mt: LuaValue = getmetatable.call(this.clone())?;
            let is_exact_dv = match &mt {
                LuaValue::Table(t) => *t == docview_class,
                _ => false,
            };

            if !enabled || !is_exact_dv {
                let old: LuaFunction = lua.registry_value(&old_key)?;
                return old.call::<f64>((this, idx, x, y));
            }

            // Get font (with fallbacks).
            let raw_font: LuaValue = this.call_method("get_font", ())?;
            let font = if matches!(raw_font, LuaValue::Nil | LuaValue::Boolean(false)) {
                let style = require_table(lua, "core.style")?;
                let sf: LuaTable = style.get("syntax_fonts")?;
                let wf: LuaValue = sf.get("whitespace")?;
                if matches!(wf, LuaValue::Nil | LuaValue::Boolean(false)) {
                    sf.get::<LuaValue>("comment")?
                } else {
                    wf
                }
            } else {
                raw_font
            };
            let font_size = font_get_size(&font)?;

            let doc: LuaTable = this.get("doc")?;
            let (_, indent_size): (LuaValue, i64) = doc.call_method("get_indent_info", ())?;

            let state: LuaTable = lua.registry_value(&sk)?;
            if settings_changed(lua, &state)? {
                reset_cache(lua, &state)?;
            }

            let ws_cache: LuaTable = state.get("ws_cache")?;
            let hl: LuaTable = doc.get("highlighter")?;

            // Invalidate per-highlighter cache if font/indent changed.
            let needs_new_hl = match ws_cache.raw_get::<LuaValue>(hl.clone())? {
                LuaValue::Table(t) => {
                    let cf: LuaValue = t.get("font")?;
                    let cs: LuaValue = t.get("font_size")?;
                    let ci: LuaValue = t.get("indent_size")?;
                    !lua_eq(&cf, &font)
                        || !lua_eq(&cs, &LuaValue::Number(font_size))
                        || !lua_eq(&ci, &LuaValue::Integer(indent_size))
                }
                _ => true,
            };
            let hl_cache = if needs_new_hl {
                let t = make_weak_table(lua)?;
                t.set("font", font.clone())?;
                t.set("font_size", font_size)?;
                t.set("indent_size", indent_size)?;
                ws_cache.raw_set(hl.clone(), t.clone())?;
                t
            } else {
                match ws_cache.raw_get::<LuaValue>(hl.clone())? {
                    LuaValue::Table(t) => t,
                    _ => unreachable!(),
                }
            };

            // Build cache entry for this line if missing.
            if matches!(hl_cache.get::<LuaValue>(idx)?, LuaValue::Nil) {
                let cache = lua.create_table()?;
                let doc_lines: LuaTable = doc.get("lines")?;
                let line_str: mlua::String = doc_lines.get(idx)?;
                let text = line_str.as_bytes().to_vec();
                let text_len = text.len();

                let substitutions: LuaTable = dw.get("substitutions")?;
                let mut cache_idx = 1i64;

                for pair in substitutions.sequence_values::<LuaTable>() {
                    let sub_entry = pair?;
                    let char_str: String = sub_entry.get("char")?;
                    let sub_str: String = sub_entry.get("sub")?;
                    let ch = char_str.bytes().next().unwrap_or(b' ');

                    let show_leading = get_option_bool(&dw, &sub_entry, "show_leading")?;
                    let show_middle = get_option_bool(&dw, &sub_entry, "show_middle")?;
                    let show_trailing = get_option_bool(&dw, &sub_entry, "show_trailing")?;
                    let show_middle_min = get_option_i64(&dw, &sub_entry, "show_middle_min")?;
                    let base_color = get_option_value(&dw, &sub_entry, "color")?;
                    let leading_color = get_option_or(&dw, &sub_entry, "leading_color", &base_color)?;
                    let middle_color = get_option_or(&dw, &sub_entry, "middle_color", &base_color)?;
                    let trailing_color = get_option_or(&dw, &sub_entry, "trailing_color", &base_color)?;

                    for (s, e) in find_byte_runs(&text, ch) {
                        let (should_draw, color) = if e >= text_len.saturating_sub(1) {
                            (show_trailing, trailing_color.clone())
                        } else if s == 1 {
                            (show_leading, leading_color.clone())
                        } else {
                            (show_middle && (e - s + 1) as i64 >= show_middle_min, middle_color.clone())
                        };

                        if !should_draw {
                            continue;
                        }

                        let tx: f64 = this.call_method("get_col_x_offset", (idx, s as i64))?;
                        if ch == b'\t' {
                            for i in s..=e {
                                let itx: f64 = this.call_method("get_col_x_offset", (idx, i as i64))?;
                                let tw = font_get_width(&font, &sub_str)?;
                                cache.set(cache_idx, sub_str.as_str())?;
                                cache.set(cache_idx + 1, itx)?;
                                cache.set(cache_idx + 2, tw)?;
                                cache.set(cache_idx + 3, color.clone())?;
                                cache_idx += 4;
                            }
                        } else {
                            let count = e - s + 1;
                            let repeated = sub_str.repeat(count);
                            let tw = font_get_width(&font, &repeated)?;
                            cache.set(cache_idx, repeated.as_str())?;
                            cache.set(cache_idx + 1, tx)?;
                            cache.set(cache_idx + 2, tw)?;
                            cache.set(cache_idx + 3, color)?;
                            cache_idx += 4;
                        }
                    }
                }

                hl_cache.set(idx, cache)?;
            }

            // Draw from cache.
            let bounds: LuaMultiValue = this.call_method("get_content_bounds", ())?;
            let x1 = bounds.iter().next().map(|v| match v {
                LuaValue::Number(n) => *n,
                LuaValue::Integer(n) => *n as f64,
                _ => 0.0,
            }).unwrap_or(0.0) + x;
            let x2 = bounds.iter().nth(2).map(|v| match v {
                LuaValue::Number(n) => *n,
                LuaValue::Integer(n) => *n as f64,
                _ => 0.0,
            }).unwrap_or(0.0) + x;
            let ty_offset: f64 = this.call_method("get_line_text_y_offset", ())?;
            let ty = y + ty_offset;

            let cache: LuaTable = hl_cache.get(idx)?;
            let cache_len = cache.raw_len() as i64;
            let show_selected_only: bool = dw.get("show_selected_only").unwrap_or(false);
            let renderer: LuaTable = lua.globals().get("renderer")?;
            let core = require_table(lua, "core")?;

            let mut i = 1i64;
            while i <= cache_len - 3 {
                let sub: String = cache.get(i)?;
                let tx: f64 = cache.get(i + 1)?;
                let tw: f64 = cache.get(i + 2)?;
                let color: LuaValue = cache.get(i + 3)?;
                let _ = (x1, x2); // bounds available for visibility culling if needed

                let tx = tx + x;

                // Collect clip rects for show_selected_only mode.
                // None = draw without clip (full range selected), Some = clip rect.
                let mut partials: Vec<Option<(f64, f64, f64, f64)>> = Vec::new();
                if show_selected_only {
                    let has_sel: bool = doc.call_method("has_any_selection", ())?;
                    if has_sel {
                        let iter_mv: LuaMultiValue = doc.call_method("get_selections", true)?;
                        let mut iter_vals = iter_mv.into_iter();
                        if let Some(LuaValue::Function(iter_fn)) = iter_vals.next() {
                            let iter_state = iter_vals.next().unwrap_or(LuaValue::Nil);
                            let mut control = iter_vals.next().unwrap_or(LuaValue::Nil);
                            loop {
                                let rv: LuaMultiValue =
                                    iter_fn.call((iter_state.clone(), control.clone()))?;
                                if matches!(rv.front(), Some(LuaValue::Nil) | None) {
                                    break;
                                }
                                control = rv.front().cloned().unwrap_or(LuaValue::Nil);
                                let mut rv_iter = rv.into_iter();
                                let _ = rv_iter.next(); // discard first value
                                let l1 = get_i64_mv(&mut rv_iter);
                                let c1 = get_i64_mv(&mut rv_iter);
                                let l2 = get_i64_mv(&mut rv_iter);
                                let c2 = get_i64_mv(&mut rv_iter);

                                if idx > l1 && idx < l2 {
                                    partials.push(None); // entire line selected
                                } else if idx == l1 && idx == l2 {
                                    let col_x1: f64 = this
                                        .call_method("get_col_x_offset", (idx, c1))?;
                                    let col_x2: f64 = this
                                        .call_method("get_col_x_offset", (idx, c2))?;
                                    let rx1 = tx.max(col_x1 + x);
                                    let rx2 = (tx + tw).min(col_x2 + x);
                                    if rx1 < rx2 {
                                        partials.push(Some((rx1, 0.0, rx2 - rx1, f64::INFINITY)));
                                    }
                                } else if idx >= l1 && idx <= l2 {
                                    if idx == l1 {
                                        let col_x: f64 =
                                            this.call_method("get_col_x_offset", (idx, c1))?;
                                        let rx = tx.max(col_x + x);
                                        partials.push(Some((rx, 0.0, f64::INFINITY, f64::INFINITY)));
                                    } else {
                                        let col_x: f64 =
                                            this.call_method("get_col_x_offset", (idx, c2))?;
                                        let rx = (tx + tw).min(col_x + x);
                                        partials.push(Some((0.0, 0.0, rx, f64::INFINITY)));
                                    }
                                }
                            }
                        }
                    }
                }

                if partials.is_empty() && !show_selected_only {
                    renderer.call_function::<LuaValue>(
                        "draw_text",
                        (font.clone(), sub.as_str(), tx, ty, color),
                    )?;
                } else {
                    for p in &partials {
                        if let Some((px, py, pw, ph)) = p {
                            core.call_function::<()>(
                                "push_clip_rect",
                                (*px, *py, *pw, *ph),
                            )?;
                        }
                        renderer.call_function::<LuaValue>(
                            "draw_text",
                            (font.clone(), sub.as_str(), tx, ty, color.clone()),
                        )?;
                        if p.is_some() {
                            core.call_function::<()>("pop_clip_rect", ())?;
                        }
                    }
                }

                i += 4;
            }

            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<f64>((this, idx, x, y))
        })?,
    )?;
    Ok(())
}

fn get_option_value(dw: &LuaTable, sub: &LuaTable, key: &str) -> LuaResult<LuaValue> {
    let v: LuaValue = sub.get(key)?;
    if matches!(v, LuaValue::Nil) { dw.get(key) } else { Ok(v) }
}

fn get_option_bool(dw: &LuaTable, sub: &LuaTable, key: &str) -> LuaResult<bool> {
    let v: LuaValue = sub.get(key)?;
    match v {
        LuaValue::Nil => Ok(dw.get::<bool>(key).unwrap_or(false)),
        LuaValue::Boolean(b) => Ok(b),
        _ => Ok(true),
    }
}

fn get_option_i64(dw: &LuaTable, sub: &LuaTable, key: &str) -> LuaResult<i64> {
    let v: LuaValue = sub.get(key)?;
    match v {
        LuaValue::Nil => Ok(dw.get::<i64>(key).unwrap_or(0)),
        LuaValue::Integer(n) => Ok(n),
        LuaValue::Number(n) => Ok(n as i64),
        _ => Ok(0),
    }
}

fn get_option_or(dw: &LuaTable, sub: &LuaTable, key: &str, default: &LuaValue) -> LuaResult<LuaValue> {
    let v = get_option_value(dw, sub, key)?;
    if matches!(v, LuaValue::Nil) {
        Ok(default.clone())
    } else {
        Ok(v)
    }
}

fn get_i64_mv(iter: &mut impl Iterator<Item = LuaValue>) -> i64 {
    match iter.next() {
        Some(LuaValue::Integer(n)) => n,
        Some(LuaValue::Number(n)) => n as i64,
        _ => 0,
    }
}

fn register_commands(lua: &Lua) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;
    let cmds = lua.create_table()?;
    cmds.set(
        "draw-whitespace:toggle",
        lua.create_function(|lua, ()| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let dw: LuaTable = plugins.get("drawwhitespace")?;
            let enabled: bool = dw.get("enabled").unwrap_or(false);
            dw.set("enabled", !enabled)?;
            Ok(())
        })?,
    )?;
    cmds.set(
        "draw-whitespace:disable",
        lua.create_function(|lua, ()| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let dw: LuaTable = plugins.get("drawwhitespace")?;
            dw.set("enabled", false)?;
            Ok(())
        })?,
    )?;
    cmds.set(
        "draw-whitespace:enable",
        lua.create_function(|lua, ()| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let dw: LuaTable = plugins.get("drawwhitespace")?;
            dw.set("enabled", true)?;
            Ok(())
        })?,
    )?;
    command.call_function::<()>("add", (LuaValue::Nil, cmds))?;
    Ok(())
}

/// Registers `plugins.drawwhitespace`: config defaults, Highlighter cache patches,
/// `DocView.draw_line_text` patch for whitespace rendering, and 3 commands.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.drawwhitespace",
        lua.create_function(|lua, ()| {
            set_config_defaults(lua)?;

            let state = lua.create_table()?;
            reset_cache(lua, &state)?;
            let state_key = Arc::new(lua.create_registry_value(state)?);

            patch_highlighter_methods(lua, state_key.clone())?;
            patch_draw_line_text(lua, state_key)?;
            register_commands(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )
}
