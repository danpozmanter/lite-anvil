use mlua::prelude::*;

use std::sync::Arc;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Registers `core.commandview` — command palette input with suggestions.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "core.commandview",
        lua.create_function(|lua, ()| {
            let doc_class: LuaTable = require_table(lua, "core.doc")?;
            let docview_class: LuaTable = require_table(lua, "core.docview")?;
            let view_class: LuaTable = require_table(lua, "core.view")?;

            // SingleLineDoc extends Doc
            let single_line_doc = doc_class.call_method::<LuaTable>("extend", ())?;
            single_line_doc.set(
                "__tostring",
                lua.create_function(|_lua, _self: LuaTable| Ok("SingleLineDoc"))?,
            )?;
            let sld_key = Arc::new(lua.create_registry_value(single_line_doc.clone())?);
            single_line_doc.set("insert", {
                let k = Arc::clone(&sld_key);
                lua.create_function(move |lua, (this, line, col, text): (LuaTable, LuaValue, LuaValue, String)| {
                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_insert: LuaFunction = super_tbl.get("insert")?;
                    let cleaned = text.replace('\n', "");
                    super_insert.call::<()>((this, line, col, cleaned))
                })?
            })?;

            // CommandView extends DocView
            let command_view = docview_class.call_method::<LuaTable>("extend", ())?;
            command_view.set(
                "__tostring",
                lua.create_function(|_lua, _self: LuaTable| Ok("CommandView"))?,
            )?;
            command_view.set("context", "application")?;

            let noop = lua.create_function(|_lua, _args: LuaMultiValue| Ok(()))?;
            let noop_key = Arc::new(lua.create_registry_value(noop)?);

            let default_state = lua.create_table()?;
            let noop_fn: LuaFunction = lua.registry_value(&noop_key)?;
            default_state.set("submit", noop_fn.clone())?;
            default_state.set("suggest", noop_fn.clone())?;
            default_state.set("cancel", noop_fn)?;
            default_state.set(
                "validate",
                lua.create_function(|_lua, _args: LuaMultiValue| Ok(true))?,
            )?;
            default_state.set("text", "")?;
            default_state.set("select_text", false)?;
            default_state.set("show_suggestions", true)?;
            default_state.set("typeahead", true)?;
            default_state.set("wrap", true)?;
            let ds_key = Arc::new(lua.create_registry_value(default_state)?);

            let class_key = Arc::new(lua.create_registry_value(command_view.clone())?);

            // CommandView:new()
            command_view.set("new", {
                let k = Arc::clone(&class_key);
                let sld = Arc::clone(&sld_key);
                let ds = Arc::clone(&ds_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let sld_class: LuaTable = lua.registry_value(&sld)?;
                    let doc: LuaTable = sld_class.call(())?;
                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_new: LuaFunction = super_tbl.get("new")?;
                    super_new.call::<()>((this.clone(), doc))?;

                    this.set("suggestion_idx", 1)?;
                    this.set("suggestions_offset", 1)?;
                    this.set("suggestions", lua.create_table()?)?;
                    this.set("suggestions_height", 0.0)?;
                    this.set("last_change_id", 0)?;
                    this.set("last_text", "")?;
                    this.set("user_supplied_text", "")?;
                    this.set("last_change", "text")?;
                    this.set("gutter_width", 0.0)?;
                    this.set("gutter_text_brightness", 0.0)?;
                    this.set("selection_offset", 0.0)?;
                    let default: LuaTable = lua.registry_value(&ds)?;
                    this.set("state", default)?;
                    this.set("font", "font")?;
                    let size: LuaTable = this.get("size")?;
                    size.set("y", 0.0)?;
                    this.set("label", "")?;
                    this.set("suggestion_cache", lua.create_table()?)?;
                    this.set("suggestion_cache_count", 0)?;
                    this.set("suggestion_max_width", 0.0)?;
                    Ok(())
                })?
            })?;

            // CommandView:set_hidden_suggestions()
            command_view.set(
                "set_hidden_suggestions",
                lua.create_function(|_lua, this: LuaTable| {
                    let state: LuaTable = this.get("state")?;
                    state.set("show_suggestions", false)
                })?,
            )?;

            // CommandView:get_name()
            let view_get_name: LuaFunction = view_class.get("get_name")?;
            let vgn_key = Arc::new(lua.create_registry_value(view_get_name)?);
            command_view.set("get_name", {
                let vgn = Arc::clone(&vgn_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let get_name: LuaFunction = lua.registry_value(&vgn)?;
                    get_name.call::<String>(this)
                })?
            })?;

            // CommandView:get_line_screen_position(line, col)
            command_view.set("get_line_screen_position", {
                let k = Arc::clone(&class_key);
                lua.create_function(move |lua, (this, _line, col): (LuaTable, LuaValue, LuaValue)| {
                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_fn: LuaFunction = super_tbl.get("get_line_screen_position")?;
                    let x: f64 = super_fn.call((this.clone(), 1, col))?;
                    let (_, oy): (f64, f64) = this.call_method("get_content_offset", ())?;
                    let lh: f64 = this.call_method("get_line_height", ())?;
                    let size: LuaTable = this.get("size")?;
                    let size_y: f64 = size.get("y")?;
                    Ok((x, oy + (size_y - lh) / 2.0))
                })?
            })?;

            // CommandView:supports_text_input()
            command_view.set(
                "supports_text_input",
                lua.create_function(|_lua, _this: LuaTable| Ok(true))?,
            )?;

            // CommandView:get_scrollable_size()
            command_view.set(
                "get_scrollable_size",
                lua.create_function(|_lua, _this: LuaTable| Ok(0.0))?,
            )?;

            // CommandView:scroll_to_make_visible(line, col)
            command_view.set("scroll_to_make_visible", {
                let k = Arc::clone(&class_key);
                lua.create_function(move |lua, (this, line, col): (LuaTable, LuaValue, LuaValue)| {
                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_fn: LuaFunction = super_tbl.get("scroll_to_make_visible")?;
                    super_fn.call::<()>((this.clone(), line, col))?;
                    let scroll: LuaTable = this.get("scroll")?;
                    let scroll_to: LuaTable = scroll.get("to")?;
                    scroll_to.set("y", 0.0)
                })?
            })?;

            // CommandView:clamp_scroll_position()
            command_view.set(
                "clamp_scroll_position",
                lua.create_function(|_lua, this: LuaTable| {
                    let scroll: LuaTable = this.get("scroll")?;
                    let scroll_to: LuaTable = scroll.get("to")?;
                    scroll_to.set("y", 0.0)
                })?,
            )?;

            // CommandView:get_text()
            command_view.set(
                "get_text",
                lua.create_function(|_lua, this: LuaTable| {
                    let doc: LuaTable = this.get("doc")?;
                    let text: String = doc.call_method("get_text", (1, 1, 1, f64::INFINITY))?;
                    Ok(text)
                })?,
            )?;

            // CommandView:set_text(text, select)
            command_view.set(
                "set_text",
                lua.create_function(
                    |_lua, (this, text, select): (LuaTable, String, Option<bool>)| {
                        this.set("last_text", text.as_str())?;
                        let doc: LuaTable = this.get("doc")?;
                        doc.call_method::<()>("remove", (1, 1, f64::INFINITY, f64::INFINITY))?;
                        doc.call_method::<()>("text_input", text)?;
                        if select.unwrap_or(false) {
                            doc.call_method::<()>(
                                "set_selection",
                                (f64::INFINITY, f64::INFINITY, 1, 1),
                            )?;
                        }
                        Ok(())
                    },
                )?,
            )?;

            // CommandView:move_suggestion_idx(dir)
            command_view.set(
                "move_suggestion_idx",
                lua.create_function(|lua, (this, dir): (LuaTable, i64)| {
                    let config: LuaTable = require_table(lua, "core.config")?;
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let state: LuaTable = this.get("state")?;
                    let wrap: bool = state.get::<LuaValue>("wrap")?.as_boolean().unwrap_or(true);
                    let suggestions: LuaTable = this.get("suggestions")?;
                    let count = suggestions.raw_len() as i64;
                    let max_visible: i64 = {
                        let mv: f64 = config.get("max_visible_commands")?;
                        (mv as i64).min(count)
                    };

                    let overflow = |n: i64, c: i64| -> i64 {
                        if c == 0 { return 0; }
                        if wrap { (n - 1).rem_euclid(c) + 1 } else { n.clamp(1, c) }
                    };

                    this.set("last_change", "suggestion")?;
                    let mut suggestion_idx: i64 = this.get::<LuaValue>("suggestion_idx")?
                        .as_integer().or_else(|| this.get::<LuaValue>("suggestion_idx").ok()?.as_number().map(|n| n as i64))
                        .unwrap_or(1);

                    let show_suggestions: bool = state.get::<LuaValue>("show_suggestions")?.as_boolean().unwrap_or(true);
                    if show_suggestions {
                        let n = suggestion_idx + dir;
                        suggestion_idx = overflow(n, count);
                        this.set("suggestion_idx", suggestion_idx)?;
                        this.call_method::<()>("complete", ())?;
                        let doc: LuaTable = this.get("doc")?;
                        let change_id: LuaValue = doc.call_method("get_change_id", ())?;
                        this.set("last_change_id", change_id)?;
                    } else {
                        let current_item: LuaValue = suggestions.raw_get(suggestion_idx)?;
                        let current_suggestion = match &current_item {
                            LuaValue::Table(t) => {
                                let v: LuaValue = t.get("text")?;
                                match v { LuaValue::String(s) => Some(s.to_str()?.to_string()), _ => None }
                            }
                            _ => None,
                        };
                        let text: String = this.call_method("get_text", ())?;
                        if current_suggestion.as_deref() == Some(text.as_str()) {
                            let n = suggestion_idx + dir;
                            if n == 0 {
                                let save: LuaValue = this.get("save_suggestion")?;
                                if !matches!(save, LuaValue::Nil) {
                                    this.call_method::<()>("set_text", save)?;
                                }
                            } else {
                                suggestion_idx = overflow(n, count);
                                this.set("suggestion_idx", suggestion_idx)?;
                                this.call_method::<()>("complete", ())?;
                            }
                        } else {
                            this.set("save_suggestion", text)?;
                            this.call_method::<()>("complete", ())?;
                        }
                        let doc: LuaTable = this.get("doc")?;
                        let change_id: LuaValue = doc.call_method("get_change_id", ())?;
                        this.set("last_change_id", change_id)?;
                        let suggest_fn: LuaFunction = state.get("suggest")?;
                        let text: String = this.call_method("get_text", ())?;
                        suggest_fn.call::<()>(text)?;
                    }

                    // get_suggestions_offset
                    suggestion_idx = this.get("suggestion_idx")?;
                    let suggestions_offset: i64 = this.get("suggestions_offset")?;
                    let new_offset = if dir > 0 {
                        if suggestions_offset + max_visible < suggestion_idx + 1 {
                            suggestion_idx - max_visible + 1
                        } else if suggestions_offset > suggestion_idx {
                            suggestion_idx
                        } else {
                            suggestions_offset
                        }
                    } else if suggestions_offset > suggestion_idx {
                        suggestion_idx
                    } else if suggestions_offset + max_visible < suggestion_idx + 1 {
                        suggestion_idx - max_visible + 1
                    } else {
                        suggestions_offset
                    };
                    this.set("suggestions_offset", new_offset)?;
                    let _ = common;
                    Ok(())
                })?,
            )?;

            // CommandView:complete()
            command_view.set(
                "complete",
                lua.create_function(|_lua, this: LuaTable| {
                    let suggestions: LuaTable = this.get("suggestions")?;
                    let count = suggestions.raw_len() as i64;
                    if count > 0 {
                        let idx: i64 = this.get("suggestion_idx")?;
                        let item: LuaTable = suggestions.raw_get(idx)?;
                        let text: String = item.get("text")?;
                        this.call_method::<()>("set_text", text)?;
                    }
                    Ok(())
                })?,
            )?;

            // CommandView:submit()
            command_view.set(
                "submit",
                lua.create_function(|_lua, this: LuaTable| {
                    let suggestions: LuaTable = this.get("suggestions")?;
                    let idx: i64 = this.get("suggestion_idx")?;
                    let suggestion: LuaValue = suggestions.raw_get(idx)?;
                    let text: String = this.call_method("get_text", ())?;
                    let state: LuaTable = this.get("state")?;
                    let validate: LuaFunction = state.get("validate")?;
                    let valid: bool = validate.call((text.clone(), suggestion.clone()))?;
                    if valid {
                        let submit: LuaFunction = state.get("submit")?;
                        this.call_method::<()>("exit", true)?;
                        submit.call::<()>((text, suggestion))?;
                    }
                    Ok(())
                })?,
            )?;

            // CommandView:enter(label, options)
            command_view.set("enter", {
                let ds = Arc::clone(&ds_key);
                lua.create_function(move |lua, (this, label, options): (LuaTable, String, LuaTable)| {
                    let default: LuaTable = lua.registry_value(&ds)?;
                    let current_state: LuaValue = this.get("state")?;
                    if let LuaValue::Table(ref cs) = current_state {
                        if *cs != default {
                            return Ok(());
                        }
                    }
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let new_state: LuaTable = common.call_function("merge", (default, options.clone()))?;
                    this.set("state", new_state.clone())?;

                    let text: LuaValue = options.get("text")?;
                    let text_str = match &text {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => String::new(),
                    };
                    let select_text: bool = new_state.get::<LuaValue>("select_text")?.as_boolean().unwrap_or(false);
                    this.call_method::<()>("set_text", (text_str, select_text))?;

                    let core: LuaTable = require_table(lua, "core")?;
                    core.call_function::<()>("set_active_view", this.clone())?;
                    this.call_method::<()>("update_suggestions", ())?;
                    this.set("gutter_text_brightness", 100.0)?;
                    this.set("label", format!("{}: ", label))?;
                    Ok(())
                })?
            })?;

            // CommandView:exit(submitted, inexplicit)
            command_view.set("exit", {
                let ds = Arc::clone(&ds_key);
                lua.create_function(move |lua, (this, submitted, inexplicit): (LuaTable, Option<bool>, Option<bool>)| {
                    let submitted = submitted.unwrap_or(false);
                    let inexplicit = inexplicit.unwrap_or(false);
                    let core: LuaTable = require_table(lua, "core")?;
                    let active_view: LuaValue = core.get("active_view")?;
                    if let LuaValue::Table(ref av) = active_view {
                        if *av == this {
                            let last_av: LuaValue = core.get("last_active_view")?;
                            core.call_function::<()>("set_active_view", last_av)?;
                        }
                    }
                    let state: LuaTable = this.get("state")?;
                    let cancel: LuaFunction = state.get("cancel")?;
                    let default: LuaTable = lua.registry_value(&ds)?;
                    this.set("state", default)?;
                    let doc: LuaTable = this.get("doc")?;
                    doc.call_method::<()>("reset", ())?;
                    this.set("suggestions", lua.create_table()?)?;
                    if !submitted {
                        cancel.call::<()>(!inexplicit)?;
                    }
                    this.set("save_suggestion", LuaValue::Nil)?;
                    this.set("last_text", "")?;
                    this.set("suggestion_cache", lua.create_table()?)?;
                    this.set("suggestion_cache_count", 0)?;
                    this.set("suggestion_max_width", 0.0)?;
                    Ok(())
                })?
            })?;

            // CommandView:get_line_height()
            command_view.set(
                "get_line_height",
                lua.create_function(|_lua, this: LuaTable| {
                    let font: LuaValue = this.call_method("get_font", ())?;
                    let fh: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_height", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                        _ => 14.0,
                    };
                    Ok((fh * 1.2).floor())
                })?,
            )?;

            // CommandView:get_gutter_width()
            command_view.set(
                "get_gutter_width",
                lua.create_function(|_lua, this: LuaTable| {
                    let gw: f64 = this.get("gutter_width")?;
                    Ok(gw)
                })?,
            )?;

            // CommandView:get_suggestion_line_height()
            command_view.set(
                "get_suggestion_line_height",
                lua.create_function(|lua, this: LuaTable| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let padding: LuaTable = style.get("padding")?;
                    let py: f64 = padding.get("y")?;
                    let font: LuaValue = this.call_method("get_font", ())?;
                    let fh: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_height", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                        _ => 14.0,
                    };
                    Ok(fh + py)
                })?,
            )?;

            // CommandView:on_scale_change()
            command_view.set(
                "on_scale_change",
                lua.create_function(|lua, this: LuaTable| {
                    this.set("suggestion_cache", lua.create_table()?)?;
                    this.set("suggestion_cache_count", 0)?;
                    this.set("suggestion_max_width", 0.0)?;
                    Ok(())
                })?,
            )?;

            // CommandView:get_cached_suggestion(item)
            command_view.set(
                "get_cached_suggestion",
                lua.create_function(|_lua, (this, item): (LuaTable, LuaTable)| {
                    let text: String = item.get::<LuaValue>("text")?
                        .as_string().and_then(|s| s.to_str().ok()).map(|s| s.to_string()).unwrap_or_default();
                    let info: String = item.get::<LuaValue>("info")?
                        .as_string().and_then(|s| s.to_str().ok()).map(|s| s.to_string()).unwrap_or_default();
                    let key = format!("{}\0{}", text, info);
                    let cache: LuaTable = this.get("suggestion_cache")?;
                    let cached: LuaValue = cache.get(key.as_str())?;
                    if let LuaValue::Table(ref c) = cached {
                        let cached_font: LuaValue = c.get("font")?;
                        let current_font: LuaValue = this.call_method("get_font", ())?;
                        let same = match (&cached_font, &current_font) {
                            (LuaValue::Table(a), LuaValue::Table(b)) => a == b,
                            (LuaValue::UserData(a), LuaValue::UserData(b)) => a.to_pointer() == b.to_pointer(),
                            _ => false,
                        };
                        if same { return Ok(c.clone()); }
                    }
                    let font: LuaValue = this.call_method("get_font", ())?;
                    let tw: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_width", text)?,
                        LuaValue::UserData(ud) => ud.call_method("get_width", text)?,
                        _ => 40.0,
                    };
                    let iw: f64 = if info.is_empty() {
                        0.0
                    } else {
                        match &font {
                            LuaValue::Table(t) => t.call_method("get_width", info)?,
                            LuaValue::UserData(ud) => ud.call_method("get_width", info)?,
                            _ => 0.0,
                        }
                    };
                    let entry = _lua.create_table()?;
                    entry.set("font", font)?;
                    entry.set("text_width", tw)?;
                    entry.set("info_width", iw)?;

                    let count: i64 = this.get("suggestion_cache_count")?;
                    if !matches!(cached, LuaValue::Table(_)) && count >= 512 {
                        this.set("suggestion_cache", _lua.create_table()?)?;
                        this.set("suggestion_cache_count", 0)?;
                    }
                    if !matches!(cached, LuaValue::Table(_)) {
                        let new_count: i64 = this.get("suggestion_cache_count")?;
                        this.set("suggestion_cache_count", new_count + 1)?;
                    }
                    let cache: LuaTable = this.get("suggestion_cache")?;
                    cache.set(key.as_str(), entry.clone())?;
                    Ok(entry)
                })?,
            )?;

            // CommandView:update_suggestions()
            command_view.set(
                "update_suggestions",
                lua.create_function(|lua, this: LuaTable| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let padding: LuaTable = style.get("padding")?;
                    let px: f64 = padding.get("x")?;
                    let text: String = this.call_method("get_text", ())?;
                    let state: LuaTable = this.get("state")?;
                    let suggest_fn: LuaFunction = state.get("suggest")?;
                    let last_change: String = this.get("last_change")?;
                    let input = if last_change == "suggestion" {
                        let ust: String = this.get("user_supplied_text")?;
                        ust
                    } else {
                        text
                    };
                    let t: LuaValue = suggest_fn.call(input)?;
                    let t = match t {
                        LuaValue::Table(tbl) => tbl,
                        _ => lua.create_table()?,
                    };
                    let res = lua.create_table()?;
                    let len = t.raw_len() as i64;
                    for i in 1..=len {
                        let mut item: LuaValue = t.raw_get(i)?;
                        if let LuaValue::String(s) = item {
                            let tbl = lua.create_table()?;
                            tbl.set("text", s)?;
                            item = LuaValue::Table(tbl);
                        }
                        if let LuaValue::Table(ref item_tbl) = item {
                            let metrics: LuaTable = this.call_method("get_cached_suggestion", item_tbl.clone())?;
                            let tw: f64 = metrics.get("text_width")?;
                            let iw: f64 = metrics.get("info_width")?;
                            let info: LuaValue = item_tbl.get("info")?;
                            let cw = tw + if !matches!(info, LuaValue::Nil) { px * 2.0 + iw } else { 0.0 };
                            item_tbl.set("cached_width", cw)?;
                        }
                        res.raw_set(i, item)?;
                    }

                    let suggestions: LuaValue = this.get("suggestions")?;
                    if let LuaValue::Table(_) = suggestions {
                        if last_change == "suggestion" {
                            let mut new_idx: Option<i64> = None;
                            let suggestion_idx: i64 = this.get::<LuaValue>("suggestion_idx")?
                                .as_integer().or_else(|| this.get::<LuaValue>("suggestion_idx").ok()?.as_number().map(|n| n as i64))
                                .unwrap_or(1);
                            let old_sug: LuaTable = match &suggestions {
                                LuaValue::Table(t) => t.clone(),
                                _ => lua.create_table()?,
                            };
                            let current_item: LuaValue = old_sug.raw_get(suggestion_idx)?;
                            if let LuaValue::Table(ref ci) = current_item {
                                let ci_text: String = ci.get::<LuaValue>("text")?.as_string().and_then(|s| s.to_str().ok()).map(|s| s.to_string()).unwrap_or_default();
                                let res_len = res.raw_len() as i64;
                                for j in 1..=res_len {
                                    let v: LuaTable = res.raw_get(j)?;
                                    let v_text: String = v.get::<LuaValue>("text")?.as_string().and_then(|s| s.to_str().ok()).map(|s| s.to_string()).unwrap_or_default();
                                    if v_text == ci_text {
                                        new_idx = Some(j);
                                        break;
                                    }
                                }
                            }
                            let res_len = res.raw_len() as i64;
                            let idx = new_idx.unwrap_or(if res_len > 0 { 1 } else { 0 });
                            this.set("suggestion_idx", idx)?;
                            this.call_method::<()>("move_suggestion_idx", 0)?;
                        } else {
                            let res_len = res.raw_len() as i64;
                            this.set("suggestion_idx", if res_len > 0 { 1 } else { 0 })?;
                            this.set("suggestions_offset", 1)?;
                        }
                    }

                    this.set("suggestions", res.clone())?;
                    this.set("suggestion_max_width", 0.0)?;
                    let res_len = res.raw_len() as i64;
                    let mut max_w = 0.0f64;
                    for i in 1..=res_len {
                        let item: LuaTable = res.raw_get(i)?;
                        let cw: f64 = item.get::<LuaValue>("cached_width")?.as_number().unwrap_or(0.0);
                        if cw > max_w { max_w = cw; }
                    }
                    this.set("suggestion_max_width", max_w)?;
                    Ok(())
                })?,
            )?;

            // CommandView:update()
            command_view.set("update", {
                let k = Arc::clone(&class_key);
                let ds = Arc::clone(&ds_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_update: LuaFunction = super_tbl.get("update")?;
                    super_update.call::<()>(this.clone())?;

                    let core: LuaTable = require_table(lua, "core")?;
                    let active_view: LuaValue = core.get("active_view")?;
                    let default: LuaTable = lua.registry_value(&ds)?;
                    let state: LuaTable = this.get("state")?;
                    let is_active = match &active_view {
                        LuaValue::Table(t) => *t == this,
                        _ => false,
                    };
                    if !is_active && state != default {
                        this.call_method::<()>("exit", (false, true))?;
                    }

                    let doc: LuaTable = this.get("doc")?;
                    let change_id: i64 = doc.call_method("get_change_id", ())?;
                    let last_change_id: i64 = this.get("last_change_id")?;
                    if last_change_id != change_id {
                        this.set("last_change", "text")?;
                        let text: String = this.call_method("get_text", ())?;
                        this.set("user_supplied_text", text)?;
                        this.call_method::<()>("update_suggestions", ())?;
                        let state: LuaTable = this.get("state")?;
                        let typeahead: bool = state.get::<LuaValue>("typeahead")?.as_boolean().unwrap_or(true);
                        if typeahead {
                            let suggestions: LuaTable = this.get("suggestions")?;
                            let count = suggestions.raw_len() as i64;
                            let idx: i64 = this.get("suggestion_idx")?;
                            let item: LuaValue = suggestions.raw_get(idx)?;
                            if let LuaValue::Table(ref item_tbl) = item {
                                let current_text: String = this.call_method("get_text", ())?;
                                let suggested: String = item_tbl.get::<LuaValue>("text")?.as_string().and_then(|s| s.to_str().ok()).map(|s| s.to_string()).unwrap_or_default();
                                let last_text: String = this.get("last_text")?;
                                let ends_with_sep = current_text.ends_with('/') || current_text.ends_with('\\');
                                if count == 1 && last_text.len() < current_text.len() && current_text.starts_with(&last_text) && suggested.starts_with(&current_text) && !ends_with_sep {
                                    this.call_method::<()>("set_text", suggested)?;
                                    let ct_len = current_text.len() as i64;
                                    doc.call_method::<()>("set_selection", (1, ct_len + 1, 1, f64::INFINITY))?;
                                }
                                this.set("last_text", current_text)?;
                            }
                        }
                        this.set("last_change_id", change_id)?;
                    }

                    this.call_method::<()>("move_towards", (this.clone(), "gutter_text_brightness", 0.0, 0.1, "commandview"))?;

                    let font: LuaValue = this.call_method("get_font", ())?;
                    let label: String = this.get("label")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let padding: LuaTable = style.get("padding")?;
                    let px: f64 = padding.get("x")?;
                    let py: f64 = padding.get("y")?;
                    let lw: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_width", label)?,
                        LuaValue::UserData(ud) => ud.call_method("get_width", label)?,
                        _ => 40.0,
                    };
                    let dest = lw + px;
                    let size: LuaTable = this.get("size")?;
                    let size_y: f64 = size.get("y")?;
                    if size_y <= 0.0 {
                        this.set("gutter_width", dest)?;
                    } else {
                        this.call_method::<()>("move_towards", (this.clone(), "gutter_width", dest))?;
                    }

                    let config: LuaTable = require_table(lua, "core.config")?;
                    let lh: f64 = this.call_method("get_suggestion_line_height", ())?;
                    let state: LuaTable = this.get("state")?;
                    let show: bool = state.get::<LuaValue>("show_suggestions")?.as_boolean().unwrap_or(true);
                    let suggestions: LuaTable = this.get("suggestions")?;
                    let n_sug = suggestions.raw_len() as f64;
                    let max_vis: f64 = config.get("max_visible_commands")?;
                    let sug_dest = if show { n_sug.min(max_vis) * lh } else { 0.0 };
                    this.call_method::<()>("move_towards", (this.clone(), "suggestions_height", sug_dest))?;

                    let suggestion_idx: i64 = this.get("suggestion_idx")?;
                    let suggestions_offset: i64 = this.get("suggestions_offset")?;
                    let sel_dest = (suggestion_idx - suggestions_offset + 1) as f64 * lh;
                    this.call_method::<()>("move_towards", (this.clone(), "selection_offset", sel_dest))?;

                    let is_active2 = match &active_view {
                        LuaValue::Table(t) => *t == this,
                        _ => false,
                    };
                    let fh: f64 = match &style.get::<LuaValue>("font")? {
                        LuaValue::Table(t) => t.call_method("get_height", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                        _ => 14.0,
                    };
                    let size_dest = if is_active2 { fh + py * 2.0 } else { 0.0 };
                    this.call_method::<()>("move_towards", (size, "y", size_dest))?;
                    Ok(())
                })?
            })?;

            // CommandView:draw_line_highlight()
            command_view.set(
                "draw_line_highlight",
                lua.create_function(|_lua, _this: LuaTable| Ok(()))?,
            )?;

            // CommandView:draw_line_gutter(idx, x, y)
            command_view.set(
                "draw_line_gutter",
                lua.create_function(|lua, (this, _idx, x, y): (LuaTable, LuaValue, f64, f64)| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let core: LuaTable = require_table(lua, "core")?;
                    let renderer: LuaTable = lua.globals().get("renderer")?;
                    let yoffset: f64 = this.call_method("get_line_text_y_offset", ())?;
                    let position: LuaTable = this.get("position")?;
                    let pos_x: f64 = position.get("x")?;
                    let pos_y: f64 = position.get("y")?;
                    let gutter_width: f64 = this.call_method("get_gutter_width", ())?;
                    let size: LuaTable = this.get("size")?;
                    let size_y: f64 = size.get("y")?;
                    let brightness: f64 = this.get("gutter_text_brightness")?;
                    let style_text: LuaValue = style.get("text")?;
                    let accent: LuaValue = style.get("accent")?;
                    let color: LuaValue = common.call_function("lerp", (style_text, accent, brightness / 100.0))?;
                    let push_clip: LuaFunction = core.get("push_clip_rect")?;
                    push_clip.call::<()>((pos_x, pos_y, gutter_width, size_y))?;
                    let padding: LuaTable = style.get("padding")?;
                    let px: f64 = padding.get("x")?;
                    let x = x + px;
                    let font: LuaValue = this.call_method("get_font", ())?;
                    let label: String = this.get("label")?;
                    let draw_text: LuaFunction = renderer.get("draw_text")?;
                    draw_text.call::<()>((font, label, x, y + yoffset, color))?;
                    let pop_clip: LuaFunction = core.get("pop_clip_rect")?;
                    pop_clip.call::<()>(())?;
                    let lh: f64 = this.call_method("get_line_height", ())?;
                    Ok(lh)
                })?,
            )?;

            // CommandView:draw()
            command_view.set("draw", {
                let k = Arc::clone(&class_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_draw: LuaFunction = super_tbl.get("draw")?;
                    super_draw.call::<()>(this.clone())?;

                    let state: LuaTable = this.get("state")?;
                    let show: bool = state.get::<LuaValue>("show_suggestions")?.as_boolean().unwrap_or(true);
                    if show {
                        let core: LuaTable = require_table(lua, "core")?;
                        let root_view: LuaTable = core.get("root_view")?;

                        let this_key = lua.create_registry_value(this.clone())?;
                        let draw_box = lua.create_function(move |lua, this: LuaTable| {
                            let style: LuaTable = require_table(lua, "core.style")?;
                            let common: LuaTable = require_table(lua, "core.common")?;
                            let config: LuaTable = require_table(lua, "core.config")?;
                            let core: LuaTable = require_table(lua, "core")?;
                            let renderer: LuaTable = lua.globals().get("renderer")?;
                            let draw_rect: LuaFunction = renderer.get("draw_rect")?;
                            let padding: LuaTable = style.get("padding")?;
                            let px: f64 = padding.get("x")?;

                            let lh: f64 = this.call_method("get_suggestion_line_height", ())?;
                            let dh: f64 = style.get("divider_size")?;
                            let (x, _): (f64, f64) = this.call_method("get_line_screen_position", ())?;
                            let sug_h: f64 = this.get("suggestions_height")?;
                            let h = sug_h.ceil();
                            let gutter_width: f64 = this.get("gutter_width")?;
                            let sug_max_w: f64 = this.get("suggestion_max_width")?;
                            let size: LuaTable = this.get("size")?;
                            let size_x: f64 = size.get("x")?;
                            let rw = size_x.min((gutter_width + px * 2.0 + sug_max_w).max(size_x * 0.45));
                            let position: LuaTable = this.get("position")?;
                            let pos_x: f64 = position.get("x")?;
                            let pos_y: f64 = position.get("y")?;
                            let rx = pos_x;
                            let ry = pos_y - h - dh;

                            let push_clip: LuaFunction = core.get("push_clip_rect")?;
                            push_clip.call::<()>((rx, ry, rw, h))?;

                            let suggestions: LuaTable = this.get("suggestions")?;
                            let count = suggestions.raw_len() as i64;
                            if count > 0 {
                                let bg3: LuaValue = style.get("background3")?;
                                let divider: LuaValue = style.get("divider")?;
                                let line_hl: LuaValue = style.get("line_highlight")?;
                                draw_rect.call::<()>((rx, ry, rw, h, bg3))?;
                                draw_rect.call::<()>((rx, ry - dh, rw, dh, divider))?;
                                let sel_offset: f64 = this.get("selection_offset")?;
                                let y = pos_y - sel_offset - dh;
                                draw_rect.call::<()>((rx, y, rw, lh, line_hl))?;
                            }

                            let sug_offset: i64 = this.get("suggestions_offset")?;
                            let first = sug_offset.max(1);
                            let max_vis: f64 = config.get("max_visible_commands")?;
                            let last = (sug_offset + max_vis as i64).min(count);
                            let suggestion_idx: i64 = this.get("suggestion_idx")?;
                            let accent: LuaValue = style.get("accent")?;
                            let style_text: LuaValue = style.get("text")?;
                            let style_dim: LuaValue = style.get("dim")?;

                            for i in first..=last {
                                let item: LuaTable = suggestions.raw_get(i)?;
                                let color = if i == suggestion_idx { accent.clone() } else { style_text.clone() };
                                let y = pos_y - (i - first + 1) as f64 * lh - dh;
                                let text_w = (rw - x + rx - px).max(0.0);
                                let font: LuaValue = this.call_method("get_font", ())?;
                                let item_text: String = item.get::<LuaValue>("text")?.as_string().and_then(|s| s.to_str().ok()).map(|s| s.to_string()).unwrap_or_default();
                                common.call_function::<LuaValue>("draw_text", (font.clone(), color, item_text, LuaValue::Nil, x, y, text_w, lh))?;
                                let info: LuaValue = item.get("info")?;
                                if let LuaValue::String(_) = info {
                                    let w = rw - (x - rx) - px;
                                    common.call_function::<LuaValue>("draw_text", (font, style_dim.clone(), info, "right", x, y, w, lh))?;
                                }
                            }

                            let pop_clip: LuaFunction = core.get("pop_clip_rect")?;
                            pop_clip.call::<()>(())?;
                            Ok(())
                        })?;

                        let this2: LuaTable = lua.registry_value(&this_key)?;
                        root_view.call_method::<()>("defer_draw", (draw_box, this2))?;
                    }
                    Ok(())
                })?
            })?;

            Ok(LuaValue::Table(command_view))
        })?,
    )
}
