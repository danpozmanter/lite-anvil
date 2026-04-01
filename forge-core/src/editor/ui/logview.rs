use mlua::prelude::*;

use std::sync::Arc;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Registers `core.logview` — log message display with copy and expand.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "core.logview",
        lua.create_function(|lua, ()| {
            let view_class: LuaTable = require_table(lua, "core.view")?;
            let log_view = view_class.call_method::<LuaTable>("extend", ())?;

            log_view.set(
                "__tostring",
                lua.create_function(|_lua, _self: LuaTable| Ok("LogView"))?,
            )?;

            log_view.set("context", "session")?;

            let class_key = Arc::new(lua.create_registry_value(log_view.clone())?);

            // Store item height cache on the class table
            log_view.set("_item_height_result", lua.create_table()?)?;

            // lines(text) helper
            let lines_fn = lua.create_function(|_lua, text: String| {
                if text.is_empty() {
                    return Ok(0i64);
                }
                let l = 1 + text.matches('\n').count() as i64;
                Ok(l)
            })?;
            let lines_key = Arc::new(lua.create_registry_value(lines_fn)?);

            // get_item_height(item, font) helper
            let get_item_height = {
                let k = Arc::clone(&class_key);
                let lines_k = Arc::clone(&lines_key);
                lua.create_function(move |lua, (item, font): (LuaTable, LuaValue)| {
                    let class: LuaTable = lua.registry_value(&k)?;
                    let ihr_tbl: LuaTable = class.get("_item_height_result")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let padding: LuaTable = style.get("padding")?;
                    let pad_y: f64 = padding.get("y")?;

                    // Check cache
                    let item_key = lua.create_registry_value(item.clone())?;
                    let cached: LuaValue = ihr_tbl.get(item.clone())?;
                    let font_size: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_size", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_size", ())?,
                        _ => 14.0,
                    };
                    if let LuaValue::Table(ref cache) = cached {
                        let h: LuaValue = cache.get(font_size)?;
                        if let LuaValue::Table(h) = h {
                            lua.remove_registry_value(item_key)?;
                            return Ok(h);
                        }
                    }

                    let lines_fn: LuaFunction = lua.registry_value(&lines_k)?;
                    let text: String = item.get("text")?;
                    let info: LuaValue = item.get("info")?;
                    let info_str = match &info {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => String::new(),
                    };
                    let at: String = item.get::<String>("at").unwrap_or_default();
                    let text_lines: i64 = lines_fn.call(text)?;
                    let info_lines: i64 = if info_str.is_empty() {
                        0
                    } else {
                        lines_fn.call(info_str)?
                    };
                    let at_lines: i64 = if at.is_empty() { 0 } else { 1 };
                    let l = at_lines + text_lines + info_lines;

                    let font_h: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_height", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                        _ => 14.0,
                    };

                    let h = lua.create_table()?;
                    h.set("normal", font_h + pad_y)?;
                    h.set("expanded", l as f64 * font_h + pad_y)?;
                    h.set("current", font_h + pad_y)?;
                    h.set("target", font_h + pad_y)?;

                    let cache = match cached {
                        LuaValue::Table(t) => t,
                        _ => lua.create_table()?,
                    };
                    cache.set(font_size, h.clone())?;
                    ihr_tbl.set(item, cache)?;
                    lua.remove_registry_value(item_key)?;
                    Ok(h)
                })?
            };
            let gih_key = Arc::new(lua.create_registry_value(get_item_height)?);

            // is_expanded(item, font)
            let is_expanded = {
                let gih = Arc::clone(&gih_key);
                lua.create_function(move |lua, (item, font): (LuaTable, LuaValue)| {
                    let gih_fn: LuaFunction = lua.registry_value(&gih)?;
                    let h: LuaTable = gih_fn.call((item, font))?;
                    let target: f64 = h.get("target")?;
                    let expanded: f64 = h.get("expanded")?;
                    Ok(target == expanded)
                })?
            };
            let is_expanded_key = Arc::new(lua.create_registry_value(is_expanded)?);

            // LogView:new()
            log_view.set("new", {
                let k = Arc::clone(&class_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_new: LuaFunction = super_tbl.get("new")?;
                    super_new.call::<()>(this.clone())?;

                    let core: LuaTable = require_table(lua, "core")?;
                    let log_items: LuaTable = core.get("log_items")?;
                    let len = log_items.raw_len() as i64;
                    let last: LuaValue = log_items.raw_get(len)?;
                    this.set("last_item", last)?;
                    this.set("expanding", lua.create_table()?)?;
                    this.set("scrollable", true)?;
                    this.set("yoffset", 0.0)?;
                    this.set("selected", LuaValue::Nil)?;
                    this.set("font_delta", 0.0)?;

                    let style: LuaTable = require_table(lua, "core.style")?;
                    let status_view: LuaTable = core.get("status_view")?;
                    let text_color: LuaValue = style.get("text")?;
                    status_view.call_method::<()>(
                        "show_message",
                        ("i", text_color, "click to select  \u{b7}  ctrl+c to copy entry  \u{b7}  ctrl+a to copy all"),
                    )?;
                    Ok(())
                })?
            })?;

            // LogView:get_name()
            log_view.set(
                "get_name",
                lua.create_function(|_lua, _this: LuaTable| Ok("Log"))?,
            )?;

            // LogView:get_log_font()
            log_view.set(
                "get_log_font",
                lua.create_function(|lua, this: LuaTable| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let font_delta: f64 = this.get("font_delta")?;
                    let font: LuaValue = style.get("font")?;
                    if font_delta == 0.0 {
                        return Ok(font);
                    }
                    let size: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_size", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_size", ())?,
                        _ => 14.0,
                    };
                    let copied: LuaValue = match &font {
                        LuaValue::Table(t) => t.call_method("copy", size + font_delta)?,
                        LuaValue::UserData(ud) => ud.call_method("copy", size + font_delta)?,
                        _ => font,
                    };
                    Ok(copied)
                })?,
            )?;

            // LogView:get_log_icon_font()
            log_view.set(
                "get_log_icon_font",
                lua.create_function(|lua, this: LuaTable| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let font_delta: f64 = this.get("font_delta")?;
                    let font: LuaValue = style.get("icon_font")?;
                    if font_delta == 0.0 {
                        return Ok(font);
                    }
                    let size: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_size", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_size", ())?,
                        _ => 14.0,
                    };
                    let copied: LuaValue = match &font {
                        LuaValue::Table(t) => t.call_method("copy", size + font_delta)?,
                        LuaValue::UserData(ud) => ud.call_method("copy", size + font_delta)?,
                        _ => font,
                    };
                    Ok(copied)
                })?,
            )?;

            // LogView:invalidate_item_heights()
            log_view.set("invalidate_item_heights", {
                let k = Arc::clone(&class_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let class: LuaTable = lua.registry_value(&k)?;
                    class.set("_item_height_result", lua.create_table()?)?;
                    this.set("expanding", lua.create_table()?)
                })?
            })?;

            // LogView:change_font_size(delta)
            log_view.set(
                "change_font_size",
                lua.create_function(|_lua, (this, delta): (LuaTable, f64)| {
                    let current: f64 = this.get("font_delta")?;
                    let new_val = (current + delta).clamp(-8.0, 24.0);
                    this.set("font_delta", new_val)?;
                    this.call_method::<()>("invalidate_item_heights", ())
                })?,
            )?;

            // LogView:reset_font_size()
            log_view.set(
                "reset_font_size",
                lua.create_function(|_lua, this: LuaTable| {
                    this.set("font_delta", 0.0)?;
                    this.call_method::<()>("invalidate_item_heights", ())
                })?,
            )?;

            // LogView:expand_item(item)
            log_view.set("expand_item", {
                let gih = Arc::clone(&gih_key);
                lua.create_function(move |lua, (this, item): (LuaTable, LuaTable)| {
                    let font: LuaValue = this.call_method("get_log_font", ())?;
                    let gih_fn: LuaFunction = lua.registry_value(&gih)?;
                    let h: LuaTable = gih_fn.call((item, font))?;
                    let target: f64 = h.get("target")?;
                    let expanded: f64 = h.get("expanded")?;
                    let normal: f64 = h.get("normal")?;
                    let new_target = if target == expanded { normal } else { expanded };
                    h.set("target", new_target)?;
                    let expanding: LuaTable = this.get("expanding")?;
                    let table_insert: LuaFunction =
                        lua.globals().get::<LuaTable>("table")?.get("insert")?;
                    table_insert.call::<()>((expanding, h))?;
                    Ok(())
                })?
            })?;

            // LogView:each_item() - stateful iterator, no coroutine.yield.
            log_view.set("each_item", {
                let gih = Arc::clone(&gih_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let gih2 = Arc::clone(&gih);
                    let font: LuaValue = this.call_method("get_log_font", ())?;
                    let (x, mut y): (f64, f64) =
                        this.call_method("get_content_offset", ())?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let padding: LuaTable = style.get("padding")?;
                    let pad_y: f64 = padding.get("y")?;
                    let yoffset: f64 = this.get("yoffset")?;
                    y += pad_y + yoffset;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    let core: LuaTable = require_table(lua, "core")?;
                    let log_items: LuaTable = core.get("log_items")?;
                    let len = log_items.raw_len() as i64;
                    let gih_fn: LuaFunction = lua.registry_value(&gih2)?;

                    let results = lua.create_table()?;
                    let mut count = 0i64;
                    for i in (1..=len).rev() {
                        let item: LuaTable = log_items.raw_get(i)?;
                        let h_tbl: LuaTable = gih_fn.call((item.clone(), font.clone()))?;
                        let h: f64 = h_tbl.get("current")?;
                        count += 1;
                        let entry = lua.create_table()?;
                        entry.raw_set(1, i)?;
                        entry.raw_set(2, item)?;
                        entry.raw_set(3, x)?;
                        entry.raw_set(4, y)?;
                        entry.raw_set(5, size_x)?;
                        entry.raw_set(6, h)?;
                        results.raw_set(count, entry)?;
                        y += h;
                    }

                    let state = lua.create_table()?;
                    state.set("idx", 0i64)?;
                    state.set("len", count)?;
                    let results_key = lua.create_registry_value(results)?;

                    let iterator =
                        lua.create_function(move |lua, ()| -> LuaResult<LuaMultiValue> {
                            let idx: i64 = state.get("idx")?;
                            let len: i64 = state.get("len")?;
                            let next = idx + 1;
                            if next > len {
                                return Ok(LuaMultiValue::new());
                            }
                            state.set("idx", next)?;
                            let results: LuaTable = lua.registry_value(&results_key)?;
                            let entry: LuaTable = results.raw_get(next)?;
                            let i: LuaValue = entry.raw_get(1)?;
                            let item: LuaValue = entry.raw_get(2)?;
                            let ex: LuaValue = entry.raw_get(3)?;
                            let ey: LuaValue = entry.raw_get(4)?;
                            let ew: LuaValue = entry.raw_get(5)?;
                            let eh: LuaValue = entry.raw_get(6)?;
                            Ok(LuaMultiValue::from_vec(vec![i, item, ex, ey, ew, eh]))
                        })?;
                    Ok(iterator)
                })?
            })?;

            // LogView:get_scrollable_size()
            log_view.set(
                "get_scrollable_size",
                lua.create_function(|lua, this: LuaTable| {
                    let config: LuaTable = require_table(lua, "core.config")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let (_, y_off): (f64, f64) = this.call_method("get_content_offset", ())?;
                    let padding: LuaTable = style.get("padding")?;
                    let pad_y: f64 = padding.get("y")?;
                    let mut last_y = 0.0f64;
                    let mut last_h = 0.0f64;
                    let iter: LuaFunction = this.call_method("each_item", ())?;
                    loop {
                        let results: LuaMultiValue = iter.call(())?;
                        let mut vals = results.into_iter();
                        match vals.next() {
                            Some(v) if !matches!(v, LuaValue::Nil) => {}
                            _ => break,
                        }
                        let _ = vals.next(); // item
                        let _ = vals.next(); // x
                        let y: f64 = match vals.next() {
                            Some(LuaValue::Number(n)) => n,
                            _ => break,
                        };
                        let _ = vals.next(); // w
                        let h: f64 = match vals.next() {
                            Some(LuaValue::Number(n)) => n,
                            _ => break,
                        };
                        last_y = y;
                        last_h = h;
                    }
                    let scroll_past_end: bool = config
                        .get::<LuaValue>("scroll_past_end")?
                        .as_boolean()
                        .unwrap_or(false);
                    if !scroll_past_end {
                        return Ok(last_y + last_h - y_off + pad_y);
                    }
                    let size: LuaTable = this.get("size")?;
                    let size_y: f64 = size.get("y")?;
                    Ok(last_y + size_y - y_off)
                })?,
            )?;

            // LogView:on_mouse_pressed(button, px, py, clicks)
            log_view.set("on_mouse_pressed", {
                let k = Arc::clone(&class_key);
                lua.create_function(move |lua, (this, button, px, py, clicks): (LuaTable, LuaValue, f64, f64, LuaValue)| {
                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_fn: LuaFunction = super_tbl.get("on_mouse_pressed")?;
                    let caught: LuaValue = super_fn.call((this.clone(), button.clone(), px, py, clicks))?;
                    if !matches!(caught, LuaValue::Nil | LuaValue::Boolean(false)) {
                        return Ok(LuaValue::Boolean(true));
                    }

                    let mut selected: LuaValue = LuaValue::Nil;
                    let mut index: i64 = 0;
                    let iter: LuaFunction = this.call_method("each_item", ())?;
                    loop {
                        let results: LuaMultiValue = iter.call(())?;
                        let mut vals = results.into_iter();
                        let i_val = match vals.next() {
                            Some(v) if !matches!(v, LuaValue::Nil) => v,
                            _ => break,
                        };
                        let item = match vals.next() { Some(LuaValue::Table(t)) => t, _ => break };
                        let x: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
                        let y: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
                        let w: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
                        let h: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
                        if px >= x && py >= y && px < x + w && py < y + h {
                            index = match i_val {
                                LuaValue::Integer(n) => n,
                                LuaValue::Number(n) => n as i64,
                                _ => 0,
                            };
                            selected = LuaValue::Table(item);
                            break;
                        }
                    }

                    if let LuaValue::Table(ref sel) = selected {
                        this.set("selected", sel.clone())?;
                        let button_str = match &button {
                            LuaValue::String(s) => s.to_str()?.to_string(),
                            _ => String::new(),
                        };
                        if button_str == "right" {
                            let command: LuaTable = require_table(lua, "core.command")?;
                            command.call_function::<()>("perform", ("context-menu:show", px, py))?;
                        } else {
                            let keymap: LuaTable = require_table(lua, "core.keymap")?;
                            let modkeys: LuaTable = keymap.get("modkeys")?;
                            let ctrl: bool = modkeys.get::<LuaValue>("ctrl")?.as_boolean().unwrap_or(false);
                            if ctrl {
                                let system: LuaTable = lua.globals().get("system")?;
                                let core: LuaTable = require_table(lua, "core")?;
                                let log_text: String = core.call_function("get_log", sel.clone())?;
                                system.call_function::<()>("set_clipboard", log_text)?;
                                let style: LuaTable = require_table(lua, "core.style")?;
                                let status_view: LuaTable = core.get("status_view")?;
                                let text_color: LuaValue = style.get("text")?;
                                let msg = format!("copied entry #{} to clipboard", index);
                                status_view.call_method::<()>("show_message", ("i", text_color, msg))?;
                            } else {
                                this.call_method::<()>("expand_item", sel.clone())?;
                            }
                        }
                    } else {
                        let button_str = match &button {
                            LuaValue::String(s) => s.to_str()?.to_string(),
                            _ => String::new(),
                        };
                        if button_str == "right" {
                            let command: LuaTable = require_table(lua, "core.command")?;
                            command.call_function::<()>("perform", ("context-menu:show", px, py))?;
                        }
                    }
                    Ok(LuaValue::Boolean(true))
                })?
            })?;

            // LogView:update()
            log_view.set("update", {
                let k = Arc::clone(&class_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let core: LuaTable = require_table(lua, "core")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let log_items: LuaTable = core.get("log_items")?;
                    let len = log_items.raw_len() as i64;
                    let item: LuaValue = log_items.raw_get(len)?;
                    let last_item: LuaValue = this.get("last_item")?;
                    let same = match (&last_item, &item) {
                        (LuaValue::Table(a), LuaValue::Table(b)) => a == b,
                        _ => matches!((&last_item, &item), (LuaValue::Nil, LuaValue::Nil)),
                    };
                    if !same {
                        let font: LuaValue = style.get("font")?;
                        let fh: f64 = match &font {
                            LuaValue::Table(t) => t.call_method("get_height", ())?,
                            LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                            _ => 14.0,
                        };
                        let padding: LuaTable = style.get("padding")?;
                        let pad_y: f64 = padding.get("y")?;
                        let lh = fh + pad_y;
                        let scroll: LuaTable = this.get("scroll")?;
                        let scroll_to: LuaTable = scroll.get("to")?;
                        let to_y: f64 = scroll_to.get("y")?;
                        if to_y > 0.0 {
                            let mut idx = len;
                            while idx > 1 {
                                let li: LuaValue = log_items.raw_get(idx)?;
                                let same2 = match (&last_item, &li) {
                                    (LuaValue::Table(a), LuaValue::Table(b)) => a == b,
                                    _ => false,
                                };
                                if same2 { break; }
                                idx -= 1;
                            }
                            let diff_index = len - idx;
                            let new_y = to_y + diff_index as f64 * lh;
                            scroll_to.set("y", new_y)?;
                            scroll.set("y", new_y)?;
                        } else {
                            this.set("yoffset", -lh)?;
                        }
                        this.set("last_item", item)?;
                    }

                    let expanding: LuaTable = this.get("expanding")?;
                    let exp: LuaValue = expanding.raw_get(1)?;
                    if let LuaValue::Table(exp_tbl) = exp {
                        let target: f64 = exp_tbl.get("target")?;
                        this.call_method::<()>("move_towards", (exp_tbl.clone(), "current", target))?;
                        let current: f64 = exp_tbl.get("current")?;
                        if current == target {
                            let table_remove: LuaFunction =
                                lua.globals().get::<LuaTable>("table")?.get("remove")?;
                            table_remove.call::<LuaValue>((expanding, 1))?;
                        }
                    }

                    this.call_method::<()>("move_towards", (this.clone(), "yoffset", 0.0))?;

                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_update: LuaFunction = super_tbl.get("update")?;
                    super_update.call::<()>(this)
                })?
            })?;

            // LogView:draw()
            log_view.set("draw", {
                let k = Arc::clone(&class_key);
                let is_exp = Arc::clone(&is_expanded_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let core: LuaTable = require_table(lua, "core")?;
                    let renderer: LuaTable = lua.globals().get("renderer")?;
                    let draw_rect: LuaFunction = renderer.get("draw_rect")?;
                    let draw_text: LuaFunction = renderer.get("draw_text")?;
                    let bg: LuaValue = style.get("background")?;
                    this.call_method::<()>("draw_background", bg)?;

                    let font: LuaValue = this.call_method("get_log_font", ())?;
                    let icon_font: LuaValue = this.call_method("get_log_icon_font", ())?;
                    let fh: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_height", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                        _ => 14.0,
                    };
                    let padding: LuaTable = style.get("padding")?;
                    let pad_x: f64 = padding.get("x")?;
                    let pad_y: f64 = padding.get("y")?;
                    let lh = fh + pad_y;

                    let log_tbl: LuaTable = style.get("log")?;
                    let error_entry: LuaTable = log_tbl.get("ERROR")?;
                    let info_entry: LuaTable = log_tbl.get("INFO")?;
                    let error_icon: String = error_entry.get("icon")?;
                    let info_icon: String = info_entry.get("icon")?;
                    let iw_err: f64 = match &icon_font {
                        LuaValue::Table(t) => t.call_method("get_width", error_icon)?,
                        LuaValue::UserData(ud) => ud.call_method("get_width", error_icon)?,
                        _ => 14.0,
                    };
                    let iw_info: f64 = match &icon_font {
                        LuaValue::Table(t) => t.call_method("get_width", info_icon)?,
                        LuaValue::UserData(ud) => ud.call_method("get_width", info_icon)?,
                        _ => 14.0,
                    };
                    let iw = iw_err.max(iw_info);

                    let os: LuaTable = lua.globals().get("os")?;
                    let datestr: String = os.call_function("date", ())?;
                    let tw: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_width", datestr)?,
                        LuaValue::UserData(ud) => ud.call_method("get_width", datestr)?,
                        _ => 100.0,
                    };

                    let selected: LuaValue = this.get("selected")?;
                    let position: LuaTable = this.get("position")?;
                    let pos_y: f64 = position.get("y")?;
                    let size: LuaTable = this.get("size")?;
                    let size_y: f64 = size.get("y")?;
                    let line_highlight: LuaValue = style.get("line_highlight")?;
                    let style_text: LuaValue = style.get("text")?;
                    let style_dim: LuaValue = style.get("dim")?;
                    let is_expanded_fn: LuaFunction = lua.registry_value(&is_exp)?;

                    let push_clip: LuaFunction = core.get("push_clip_rect")?;
                    let pop_clip: LuaFunction = core.get("pop_clip_rect")?;

                    let iter: LuaFunction = this.call_method("each_item", ())?;
                    loop {
                        let results: LuaMultiValue = iter.call(())?;
                        let mut vals = results.into_iter();
                        match vals.next() {
                            Some(v) if !matches!(v, LuaValue::Nil) => {}
                            _ => break,
                        }
                        let item = match vals.next() { Some(LuaValue::Table(t)) => t, _ => break };
                        let mut x: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
                        let y: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
                        let w: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
                        let h: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };

                        if y + h < pos_y || y > pos_y + size_y {
                            continue;
                        }

                        let is_selected = match &selected {
                            LuaValue::Table(t) => *t == item,
                            _ => false,
                        };
                        if is_selected {
                            draw_rect.call::<()>((x, y, w, h, line_highlight.clone()))?;
                        }
                        push_clip.call::<()>((x, y, w, h))?;
                        x += pad_x;

                        let level: String = item.get("level")?;
                        let log_entry: LuaTable = log_tbl.get(level.as_str())?;
                        let icon_color: LuaValue = log_entry.get("color")?;
                        let icon: String = log_entry.get("icon")?;
                        let x_after: f64 = common.call_function(
                            "draw_text",
                            (icon_font.clone(), icon_color, icon, "center", x, y, iw, lh),
                        )?;
                        x = x_after + pad_x;

                        let time: LuaValue = item.get("time")?;
                        let time_str: String = os.call_function("date", (LuaValue::Nil, time))?;
                        common.call_function::<LuaValue>(
                            "draw_text",
                            (font.clone(), style_dim.clone(), time_str, "left", x, y, tw, lh),
                        )?;
                        x += tw + pad_x;

                        let (content_offset_x, _): (f64, f64) =
                            this.call_method("get_content_offset", ())?;
                        let remaining_w = w - (x - content_offset_x);

                        let expanded: bool = is_expanded_fn.call((item.clone(), font.clone()))?;
                        if expanded {
                            let mut draw_y = y + (pad_y / 2.0).round();
                            let text: String = item.get("text")?;
                            for line in text.split('\n') {
                                if line.is_empty() { continue; }
                                draw_text.call::<LuaValue>((font.clone(), line.to_string(), x, draw_y, style_text.clone()))?;
                                draw_y += fh;
                            }

                            let at: String = item.get("at")?;
                            if !at.is_empty() {
                                let at_str = format!("at {}", common.call_function::<String>("home_encode", at)?);
                                common.call_function::<LuaValue>(
                                    "draw_text",
                                    (font.clone(), style_dim.clone(), at_str, "left", x, draw_y, remaining_w, lh),
                                )?;
                                draw_y += lh;
                            }

                            let info: LuaValue = item.get("info")?;
                            if let LuaValue::String(info_str) = info {
                                let info_s = info_str.to_str()?.to_string();
                                for line in info_s.split('\n') {
                                    if line.is_empty() { continue; }
                                    draw_text.call::<LuaValue>((font.clone(), line.to_string(), x, draw_y, style_dim.clone()))?;
                                    draw_y += fh;
                                }
                            }
                        } else {
                            let text: String = item.get("text")?;
                            let first_line = text.split('\n').next().unwrap_or("");
                            let has_newline = text.contains('\n');
                            let display = if has_newline {
                                format!("{} ...", first_line)
                            } else {
                                first_line.to_string()
                            };
                            common.call_function::<LuaValue>(
                                "draw_text",
                                (font.clone(), style_text.clone(), display, "left", x, y, remaining_w, lh),
                            )?;
                        }

                        pop_clip.call::<()>(())?;
                    }

                    let class: LuaTable = lua.registry_value(&k)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_draw_scrollbar: LuaFunction = super_tbl.get("draw_scrollbar")?;
                    super_draw_scrollbar.call::<()>(this)?;
                    Ok(())
                })?
            })?;

            // LogView:on_context_menu()
            log_view.set(
                "on_context_menu",
                lua.create_function(|lua, this: LuaTable| {
                    let context_menu: LuaTable = require_table(lua, "core.contextmenu")?;
                    let divider: LuaValue = context_menu.get("DIVIDER")?;
                    let items = lua.create_table()?;
                    let copy = lua.create_table()?;
                    copy.set("text", "Copy")?;
                    copy.set("command", "log:copy-entry")?;
                    items.raw_set(1, copy)?;
                    items.raw_set(2, divider)?;
                    let inc = lua.create_table()?;
                    inc.set("text", "Font Size +")?;
                    inc.set("command", "log:font-size-increase")?;
                    items.raw_set(3, inc)?;
                    let dec = lua.create_table()?;
                    dec.set("text", "Font Size -")?;
                    dec.set("command", "log:font-size-decrease")?;
                    items.raw_set(4, dec)?;
                    let reset = lua.create_table()?;
                    reset.set("text", "Font Reset")?;
                    reset.set("command", "log:font-size-reset")?;
                    items.raw_set(5, reset)?;
                    let result = lua.create_table()?;
                    result.set("items", items)?;
                    Ok((result, this))
                })?,
            )?;

            // Register commands and keybindings
            let command: LuaTable = require_table(lua, "core.command")?;
            let keymap: LuaTable = require_table(lua, "core.keymap")?;

            let lv_key = lua.create_registry_value(log_view.clone())?;
            let predicate = lua.create_function(move |lua, ()| {
                let core: LuaTable = require_table(lua, "core")?;
                let active_view: LuaTable = core.get("active_view")?;
                let lv: LuaTable = lua.registry_value(&lv_key)?;
                let is: bool = active_view.call_method("is", lv)?;
                if is {
                    Ok((true, LuaValue::Table(active_view)))
                } else {
                    Ok((false, LuaValue::Nil))
                }
            })?;

            let cmds = lua.create_table()?;
            cmds.set(
                "log:copy-entry",
                lua.create_function(|lua, lv: LuaTable| {
                    let selected: LuaValue = lv.get("selected")?;
                    let core: LuaTable = require_table(lua, "core")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let status_view: LuaTable = core.get("status_view")?;
                    let text_color: LuaValue = style.get("text")?;
                    if let LuaValue::Table(sel) = selected {
                        let system: LuaTable = lua.globals().get("system")?;
                        let log_text: String = core.call_function("get_log", sel)?;
                        system.call_function::<()>("set_clipboard", log_text)?;
                        status_view.call_method::<()>("show_message", ("i", text_color, "copied entry to clipboard"))?;
                    } else {
                        status_view.call_method::<()>("show_message", ("i", text_color, "no entry selected"))?;
                    }
                    Ok(())
                })?,
            )?;
            cmds.set(
                "log:copy-all",
                lua.create_function(|lua, _lv: LuaValue| {
                    let core: LuaTable = require_table(lua, "core")?;
                    let system: LuaTable = lua.globals().get("system")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let log_text: String = core.call_function("get_log", ())?;
                    system.call_function::<()>("set_clipboard", log_text)?;
                    let status_view: LuaTable = core.get("status_view")?;
                    let text_color: LuaValue = style.get("text")?;
                    status_view.call_method::<()>("show_message", ("i", text_color, "copied all log entries to clipboard"))?;
                    Ok(())
                })?,
            )?;
            cmds.set(
                "log:font-size-increase",
                lua.create_function(|lua, lv: LuaTable| {
                    let scale: f64 = lua.globals().get("SCALE")?;
                    lv.call_method::<()>("change_font_size", 1.0 * scale)
                })?,
            )?;
            cmds.set(
                "log:font-size-decrease",
                lua.create_function(|lua, lv: LuaTable| {
                    let scale: f64 = lua.globals().get("SCALE")?;
                    lv.call_method::<()>("change_font_size", -scale)
                })?,
            )?;
            cmds.set(
                "log:font-size-reset",
                lua.create_function(|_lua, lv: LuaTable| {
                    lv.call_method::<()>("reset_font_size", ())
                })?,
            )?;
            command.call_function::<()>("add", (predicate, cmds))?;

            let keybinds = lua.create_table()?;
            keybinds.set("ctrl+c", "log:copy-entry")?;
            keybinds.set("ctrl+a", "log:copy-all")?;
            keymap.call_function::<()>("add", keybinds)?;

            Ok(LuaValue::Table(log_view))
        })?,
    )
}
