use std::sync::Arc;

use mlua::prelude::*;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Compute normalised insertion index matching the Lua normalize_position local.
fn normalize_position(items: &LuaTable, position: i64, alignment: i64) -> LuaResult<i64> {
    let mut left_count = 0i64;
    let mut right_count = 0i64;
    for pair in items.clone().sequence_values::<LuaTable>() {
        let item = pair?;
        let align: i64 = item.get::<Option<i64>>("alignment")?.unwrap_or(1);
        if align == 1 {
            left_count += 1;
        } else {
            right_count += 1;
        }
    }
    let (offset, items_count) = if alignment == 2 {
        (left_count, right_count)
    } else {
        (0i64, left_count)
    };
    let total = left_count + right_count;
    let mut pos = if position == 0 {
        offset + 1
    } else if position < 0 {
        offset + items_count + position + 2
    } else {
        offset + position
    };
    if pos < 1 {
        pos = offset + 1;
    }
    if pos > total + 1 {
        pos = offset + items_count + 1;
    }
    Ok(pos.max(1))
}

/// Initialise all mutable state fields on the StatusView Lua table.
fn init(lua: &Lua, self_table: LuaTable) -> LuaResult<()> {
    self_table.set("message_timeout", 0.0f64)?;
    self_table.set("message", lua.create_table()?)?;
    self_table.set("tooltip_mode", false)?;
    self_table.set("tooltip", lua.create_table()?)?;
    self_table.set("items", lua.create_table()?)?;
    self_table.set("active_items", lua.create_table()?)?;
    self_table.set("hovered_item", lua.create_table()?)?;
    let pointer = lua.create_table()?;
    pointer.set("x", 0.0f64)?;
    pointer.set("y", 0.0f64)?;
    self_table.set("pointer", pointer)?;
    self_table.set("left_width", 0.0f64)?;
    self_table.set("right_width", 0.0f64)?;
    self_table.set("r_left_width", 0.0f64)?;
    self_table.set("r_right_width", 0.0f64)?;
    self_table.set("left_xoffset", 0.0f64)?;
    self_table.set("right_xoffset", 0.0f64)?;
    self_table.set("dragged_panel", "")?;
    self_table.set("hovered_panel", "")?;
    self_table.set("hide_messages", false)?;
    self_table.set("visible", true)?;
    self_table.set("_separator_width", LuaValue::Nil)?;
    self_table.set("_separator2_width", LuaValue::Nil)?;
    Ok(())
}

/// Find the first item whose `name` field matches, return it or nil.
fn get_item(_lua: &Lua, (self_table, name): (LuaTable, String)) -> LuaResult<LuaValue> {
    let items: LuaTable = self_table.get("items")?;
    for item in items.sequence_values::<LuaTable>() {
        let item = item?;
        let item_name: String = item.get::<Option<String>>("name")?.unwrap_or_default();
        if item_name == name {
            return Ok(LuaValue::Table(item));
        }
    }
    Ok(LuaValue::Nil)
}

/// Remove the item with the given name from self.items; return it or nil.
fn remove_item(_lua: &Lua, (self_table, name): (LuaTable, String)) -> LuaResult<LuaValue> {
    let items: LuaTable = self_table.get("items")?;
    let len = items.raw_len();
    for i in 1..=len {
        let item: LuaTable = items.raw_get(i as i64)?;
        let item_name: String = item.get::<Option<String>>("name")?.unwrap_or_default();
        if item_name == name {
            for j in i..len {
                let next: LuaValue = items.raw_get((j + 1) as i64)?;
                items.raw_set(j as i64, next)?;
            }
            items.raw_set(len as i64, LuaValue::Nil)?;
            return Ok(LuaValue::Table(item));
        }
    }
    Ok(LuaValue::Nil)
}

/// Move a named item to a new position and optional alignment.
fn move_item(
    lua: &Lua,
    (self_table, name, position, alignment): (LuaTable, String, i64, Option<i64>),
) -> LuaResult<bool> {
    let removed = remove_item(lua, (self_table.clone(), name))?;
    let item = match removed {
        LuaValue::Table(t) => t,
        _ => return Ok(false),
    };
    if let Some(align) = alignment {
        item.set("alignment", align)?;
    }
    let item_align: i64 = item.get::<Option<i64>>("alignment")?.unwrap_or(1);
    let items: LuaTable = self_table.get("items")?;
    let pos = normalize_position(&items, position, item_align)?;
    let len = items.raw_len();
    for i in (pos..=len as i64).rev() {
        let val: LuaValue = items.raw_get(i)?;
        items.raw_set(i + 1, val)?;
    }
    items.raw_set(pos, item)?;
    Ok(true)
}

/// Reorder items so that `names[i]` appears at position `i`.
fn order_items(
    lua: &Lua,
    (self_table, names): (LuaTable, LuaTable),
) -> LuaResult<()> {
    let mut removed: Vec<LuaTable> = Vec::new();
    for name in names.sequence_values::<String>() {
        let name = name?;
        if let LuaValue::Table(item) = remove_item(lua, (self_table.clone(), name))? {
            removed.push(item);
        }
    }
    let items: LuaTable = self_table.get("items")?;
    let existing_len = items.raw_len();
    let insert_count = removed.len() as i64;
    for i in (1..=existing_len as i64).rev() {
        let val: LuaValue = items.raw_get(i)?;
        items.raw_set(i + insert_count, val)?;
    }
    for (idx, item) in removed.into_iter().enumerate() {
        items.raw_set(idx as i64 + 1, item)?;
    }
    Ok(())
}

/// Return a new table containing only items matching `alignment` (or all if nil).
fn get_items_list(
    lua: &Lua,
    (self_table, alignment): (LuaTable, Option<i64>),
) -> LuaResult<LuaTable> {
    let items: LuaTable = self_table.get("items")?;
    if let Some(align) = alignment {
        let result = lua.create_table()?;
        let mut idx = 1i64;
        for item in items.sequence_values::<LuaTable>() {
            let item = item?;
            let item_align: i64 = item.get::<Option<i64>>("alignment")?.unwrap_or(1);
            if item_align == align {
                result.raw_set(idx, item)?;
                idx += 1;
            }
        }
        Ok(result)
    } else {
        Ok(items)
    }
}

/// Set the timed message fields; no-op if hide_messages is set.
fn show_message(
    lua: &Lua,
    (self_table, icon, icon_color, text): (LuaTable, String, LuaValue, String),
) -> LuaResult<()> {
    if !self_table.get::<bool>("visible")?
        || self_table.get::<bool>("hide_messages")?
    {
        return Ok(());
    }
    let style = require_table(lua, "core.style")?;
    let system = require_table(lua, "system")?;
    let get_time: LuaFunction = system.get("get_time")?;
    let config = require_table(lua, "core.config")?;
    let message_timeout: f64 = config.get("message_timeout")?;
    let now: f64 = get_time.call(())?;

    let msg = lua.create_table()?;
    msg.raw_set(1, icon_color)?;
    msg.raw_set(2, style.get::<LuaValue>("icon_font")?)?;
    msg.raw_set(3, icon)?;
    msg.raw_set(4, style.get::<LuaValue>("dim")?)?;
    msg.raw_set(5, style.get::<LuaValue>("font")?)?;
    msg.raw_set(6, "   |   ")?;
    msg.raw_set(7, style.get::<LuaValue>("text")?)?;
    msg.raw_set(8, text)?;
    self_table.set("message", msg)?;
    self_table.set("message_timeout", now + message_timeout)?;
    Ok(())
}

/// Apply panel layout using status_model.fit_panels; write results back to self.
fn apply_panel_layout(
    lua: &Lua,
    (self_table, raw_left, raw_right): (LuaTable, f64, f64),
) -> LuaResult<()> {
    let status_model = require_table(lua, "status_model")?;
    let fit_panels: LuaFunction = status_model.get("fit_panels")?;
    let style = require_table(lua, "core.style")?;
    let padding: LuaTable = style.get("padding")?;
    let padding_x: f64 = padding.get("x")?;
    let size: LuaTable = self_table.get("size")?;
    let total_width: f64 = size.get("x")?;
    let left_xoffset: f64 = self_table.get("left_xoffset")?;
    let right_xoffset: f64 = self_table.get("right_xoffset")?;

    let fit: LuaTable = fit_panels.call((
        total_width, raw_left, raw_right, padding_x, left_xoffset, right_xoffset,
    ))?;
    self_table.set("left_width", fit.get::<f64>("left_width")?)?;
    self_table.set("right_width", fit.get::<f64>("right_width")?)?;
    self_table.set("left_xoffset", fit.get::<f64>("left_offset")?)?;
    self_table.set("right_xoffset", fit.get::<f64>("right_offset")?)?;
    Ok(())
}

/// Update dragged-panel offset via status_model.drag_panel_offset.
fn drag_panel(
    lua: &Lua,
    (self_table, panel, dx): (LuaTable, String, f64),
) -> LuaResult<()> {
    let status_model = require_table(lua, "status_model")?;
    let drag_fn: LuaFunction = status_model.get("drag_panel_offset")?;

    let r_left_width: f64 = self_table.get("r_left_width")?;
    let r_right_width: f64 = self_table.get("r_right_width")?;
    let left_width: f64 = self_table.get("left_width")?;
    let right_width: f64 = self_table.get("right_width")?;

    if panel == "left" && r_left_width > left_width {
        let left_xoffset: f64 = self_table.get("left_xoffset")?;
        let new_offset: f64 =
            drag_fn.call((left_xoffset, r_left_width, left_width, dx))?;
        self_table.set("left_xoffset", new_offset)?;
    } else if panel == "right" && r_right_width > right_width {
        let right_xoffset: f64 = self_table.get("right_xoffset")?;
        let new_offset: f64 =
            drag_fn.call((right_xoffset, r_right_width, right_width, dx))?;
        self_table.set("right_xoffset", new_offset)?;
    }
    Ok(())
}

/// Return "left" or "right" depending on cursor position.
fn get_hovered_panel(
    lua: &Lua,
    (self_table, x, y): (LuaTable, f64, f64),
) -> LuaResult<&'static str> {
    let position: LuaTable = self_table.get("position")?;
    let pos_y: f64 = position.get("y")?;
    let left_width: f64 = self_table.get("left_width")?;
    let padding_x = require_table(lua, "core.style")
        .ok()
        .and_then(|s| s.get::<Option<LuaTable>>("padding").ok().flatten())
        .and_then(|p| p.get::<Option<f64>>("x").ok().flatten())
        .unwrap_or(4.0);
    if y >= pos_y && x <= left_width + padding_x {
        Ok("left")
    } else {
        Ok("right")
    }
}

/// Compute the visible area of an item via status_model.item_visible_area.
fn get_item_visible_area(
    lua: &Lua,
    (self_table, item): (LuaTable, LuaTable),
) -> LuaResult<(f64, f64)> {
    let status_model = require_table(lua, "status_model")?;
    let item_visible_fn: LuaFunction = status_model.get("item_visible_area")?;
    let style = require_table(lua, "core.style")?;
    let padding: LuaTable = style.get("padding")?;
    let padding_x: f64 = padding.get("x")?;

    let alignment: i64 = item.get::<Option<i64>>("alignment")?.unwrap_or(1);
    let is_left = alignment == 1;

    let left_width: f64 = self_table.get("left_width")?;
    let right_width: f64 = self_table.get("right_width")?;
    let size: LuaTable = self_table.get("size")?;
    let size_x: f64 = size.get("x")?;

    let panel_width = if is_left {
        left_width
    } else {
        size_x - right_width
    };
    let item_ox: f64 = if is_left {
        self_table.get("left_xoffset")?
    } else {
        self_table.get("right_xoffset")?
    };
    let item_x: f64 = item.get("x")?;
    let item_w: f64 = item.get("w")?;

    item_visible_fn.call((is_left, panel_width, padding_x, item_ox, item_x, item_w))
}

/// Handle mouse press: activate view, check message timeout, start drag.
fn on_mouse_pressed(
    lua: &Lua,
    (self_table, button, x, y, clicks): (LuaTable, String, f64, f64, i64),
) -> LuaResult<bool> {
    let visible: bool = self_table.get("visible")?;
    if !visible {
        return Ok(false);
    }
    let core = require_table(lua, "core")?;
    let set_active: LuaFunction = core.get("set_active_view")?;
    let last_active: LuaValue = core.get("last_active_view")?;
    set_active.call::<()>(last_active)?;

    let system = require_table(lua, "system")?;
    let get_time: LuaFunction = system.get("get_time")?;
    let now: f64 = get_time.call(())?;
    let message_timeout: f64 = self_table.get("message_timeout")?;

    if now < message_timeout {
        let log_view_mod = require_table(lua, "core.logview")?;
        let active_view: LuaTable = core.get("active_view")?;
        let is_fn: LuaFunction = active_view.get("is")?;
        let is_log: bool = is_fn.call((active_view.clone(), log_view_mod))?;
        if !is_log {
            let command = require_table(lua, "core.command")?;
            let perform: LuaFunction = command.get("perform")?;
            perform.call::<()>("core:open-log")?;
        }
    } else {
        let position: LuaTable = self_table.get("position")?;
        let pos_y: f64 = position.get("y")?;
        if y >= pos_y && button == "left" && clicks == 1 {
            position.set("dx", x)?;
            let r_left: f64 = self_table.get("r_left_width")?;
            let r_right: f64 = self_table.get("r_right_width")?;
            let left_w: f64 = self_table.get("left_width")?;
            let right_w: f64 = self_table.get("right_width")?;
            if r_left > left_w || r_right > right_w {
                let panel = get_hovered_panel(lua, (self_table.clone(), x, y))?;
                self_table.set("dragged_panel", panel)?;
                self_table.set("cursor", "hand")?;
            }
        }
    }
    Ok(true)
}

/// Handle mouse move: update hovered panel/item, apply drag.
fn on_mouse_moved(
    lua: &Lua,
    (self_table, x, y, dx, _dy): (LuaTable, f64, f64, f64, f64),
) -> LuaResult<()> {
    let visible: bool = self_table.get("visible")?;
    if !visible {
        return Ok(());
    }

    let panel = get_hovered_panel(lua, (self_table.clone(), x, y))?;
    self_table.set("hovered_panel", panel)?;

    let dragged_panel: String = self_table.get("dragged_panel")?;
    if !dragged_panel.is_empty() {
        drag_panel(lua, (self_table, dragged_panel, dx))?;
        return Ok(());
    }

    let position: LuaTable = self_table.get("position")?;
    let pos_y: f64 = position.get("y")?;
    let system = require_table(lua, "system")?;
    let get_time: LuaFunction = system.get("get_time")?;
    let now: f64 = get_time.call(())?;
    let message_timeout: f64 = self_table.get("message_timeout")?;

    if y < pos_y || now <= message_timeout {
        self_table.set("cursor", "arrow")?;
        self_table.set("hovered_item", lua.create_table()?)?;
        return Ok(());
    }

    let active_items: LuaTable = self_table.get("active_items")?;
    for item in active_items.sequence_values::<LuaTable>() {
        let item = item?;
        let item_visible: bool = item.get::<Option<bool>>("visible")?.unwrap_or(true);
        let item_active: bool = item.get::<Option<bool>>("active")?.unwrap_or(false);
        let has_command: bool = item.get::<Option<LuaValue>>("command")?.is_some();
        let has_click: bool = item.get::<Option<LuaFunction>>("on_click")?.is_some();
        let tooltip: String = item.get::<Option<String>>("tooltip")?.unwrap_or_default();

        if item_visible && item_active && (has_command || has_click || !tooltip.is_empty()) {
            let (item_x, item_w) =
                get_item_visible_area(lua, (self_table.clone(), item.clone()))?;
            if x > item_x && (item_x + item_w) > x {
                let pointer: LuaTable = self_table.get("pointer")?;
                pointer.set("x", x)?;
                pointer.set("y", y)?;
                let hovered: LuaValue = self_table.get("hovered_item")?;
                if hovered != LuaValue::Table(item.clone()) {
                    self_table.set("hovered_item", item.clone())?;
                }
                if has_command || has_click {
                    self_table.set("cursor", "hand")?;
                }
                return Ok(());
            }
        }
    }
    self_table.set("cursor", "arrow")?;
    self_table.set("hovered_item", lua.create_table()?)?;
    Ok(())
}

/// Handle mouse release: execute item command or on_click callback.
fn on_mouse_released(
    lua: &Lua,
    (self_table, button, x, y): (LuaTable, String, f64, f64),
) -> LuaResult<()> {
    let visible: bool = self_table.get("visible")?;
    if !visible {
        return Ok(());
    }

    let dragged_panel: String = self_table.get("dragged_panel")?;
    if !dragged_panel.is_empty() {
        self_table.set("dragged_panel", "")?;
        self_table.set("cursor", "arrow")?;
        let position: LuaTable = self_table.get("position")?;
        let drag_start: f64 = position.get::<Option<f64>>("dx")?.unwrap_or(x);
        if (drag_start - x).abs() > f64::EPSILON {
            return Ok(());
        }
    }

    let position: LuaTable = self_table.get("position")?;
    let pos_y: f64 = position.get("y")?;
    if y < pos_y {
        return Ok(());
    }
    let hovered_item: LuaTable = self_table.get("hovered_item")?;
    let is_active: bool = hovered_item.get::<Option<bool>>("active")?.unwrap_or(false);
    if !is_active {
        return Ok(());
    }

    let (item_x, item_w) =
        get_item_visible_area(lua, (self_table.clone(), hovered_item.clone()))?;
    if x > item_x && (item_x + item_w) > x {
        if let Some(cmd) = hovered_item.get::<Option<String>>("command")? {
            let command = require_table(lua, "core.command")?;
            let perform: LuaFunction = command.get("perform")?;
            perform.call::<()>(cmd)?;
        } else if let Some(on_click) = hovered_item.get::<Option<LuaFunction>>("on_click")? {
            on_click.call::<()>((button, x, y))?;
        }
    }
    Ok(())
}

// ---- Lua helper functions reproduced from the Lua bootstrap ----

/// Measure the width of styled text items (font + color + string sequences).
fn text_width_fn(lua: &Lua) -> LuaResult<LuaFunction> {
    lua.create_function(
        |_lua, (font, _color, text, _nil, x): (LuaValue, LuaValue, String, LuaValue, f64)| {
            let w: f64 = match &font {
                LuaValue::Table(t) => t.call_method("get_width", text)?,
                LuaValue::UserData(ud) => ud.call_method("get_width", text)?,
                _ => 0.0,
            };
            Ok(x + w)
        },
    )
}

/// Walk styled text items and apply draw_fn to each text element.
fn draw_items_with(
    lua: &Lua,
    self_table: &LuaTable,
    items: &LuaTable,
    x: f64,
    y: f64,
    draw_fn: &LuaFunction,
) -> LuaResult<f64> {
    let style = require_table(lua, "core.style")?;
    let object = require_table(lua, "core.object")?;
    let is_fn: LuaFunction = object.get("is")?;
    let renderer_mod: LuaTable = lua.globals().get("renderer")?;
    let font_class: LuaValue = renderer_mod.get("font")?;

    let mut font: LuaValue = style.get("font")?;
    let mut color: LuaValue = style.get("text")?;
    let mut cur_x = x;
    let size: LuaTable = self_table.get("size")?;
    let size_y: f64 = size.get("y")?;

    for val in items.clone().sequence_values::<LuaValue>() {
        let val = val?;
        match &val {
            LuaValue::String(_) => {
                let text_str = val.as_string().map(|s| s.to_string_lossy()).unwrap_or_default();
                cur_x = draw_fn.call((
                    font.clone(),
                    color.clone(),
                    text_str,
                    LuaValue::Nil,
                    cur_x,
                    y,
                    0.0,
                    size_y,
                ))?;
            }
            LuaValue::Integer(_) | LuaValue::Number(_) => {
                let text_str = match &val {
                    LuaValue::Integer(n) => n.to_string(),
                    LuaValue::Number(n) => n.to_string(),
                    _ => String::new(),
                };
                cur_x = draw_fn.call((
                    font.clone(),
                    color.clone(),
                    text_str,
                    LuaValue::Nil,
                    cur_x,
                    y,
                    0.0,
                    size_y,
                ))?;
            }
            LuaValue::Table(_) | LuaValue::UserData(_) => {
                let is_font: bool = is_fn.call((val.clone(), font_class.clone()))?;
                if is_font {
                    font = val;
                } else {
                    color = val;
                }
            }
            _ => {}
        }
    }
    Ok(cur_x)
}

/// Measure styled text width (uses text_width as the draw function).
fn measure_styled(lua: &Lua, self_table: &LuaTable, items: &LuaTable) -> LuaResult<f64> {
    let tw_fn = text_width_fn(lua)?;
    draw_items_with(lua, self_table, items, 0.0, 0.0, &tw_fn)
}

/// Check if two styled-text tables are element-wise equal.
fn styled_text_equals(a: &LuaValue, b: &LuaValue) -> LuaResult<bool> {
    if a == b {
        return Ok(true);
    }
    let (a_t, b_t) = match (a, b) {
        (LuaValue::Table(a), LuaValue::Table(b)) => (a, b),
        _ => return Ok(false),
    };
    let a_len = a_t.raw_len();
    let b_len = b_t.raw_len();
    if a_len != b_len {
        return Ok(false);
    }
    for i in 1..=a_len as i64 {
        let va: LuaValue = a_t.raw_get(i)?;
        let vb: LuaValue = b_t.raw_get(i)?;
        if va != vb {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Strip leading/trailing separator+color pairs from styled text.
fn remove_spacing(self_table: &LuaTable, styled_text: &LuaTable, lua: &Lua) -> LuaResult<()> {
    let object = require_table(lua, "core.object")?;
    let is_fn: LuaFunction = object.get("is")?;
    let renderer_mod: LuaTable = lua.globals().get("renderer")?;
    let font_class: LuaValue = renderer_mod.get("font")?;
    let separator: String = self_table
        .get::<Option<String>>("separator")?
        .unwrap_or_else(|| "      ".to_string());
    let separator2: String = self_table
        .get::<Option<String>>("separator2")?
        .unwrap_or_else(|| "   |   ".to_string());

    let len = styled_text.raw_len() as i64;
    if len < 2 {
        return Ok(());
    }

    // Check front: if [1] is a color table (not font) and [2] is a separator string
    let first: LuaValue = styled_text.raw_get(1)?;
    let is_first_font: bool = is_fn.call((first.clone(), font_class.clone()))?;
    if !is_first_font {
        if let LuaValue::Table(_) = &first {
            let second: LuaValue = styled_text.raw_get(2)?;
            if let Some(s) = second.as_string().map(|s| s.to_string_lossy()) {
                if s == separator || s == separator2 {
                    lua_table_remove(styled_text, 1)?;
                    lua_table_remove(styled_text, 1)?;
                }
            }
        }
    }

    // Check back: if [n-1] is a color table (not font) and [n] is a separator string
    let new_len = styled_text.raw_len() as i64;
    if new_len >= 2 {
        let second_last: LuaValue = styled_text.raw_get(new_len - 1)?;
        let is_sl_font: bool = is_fn.call((second_last.clone(), font_class.clone()))?;
        if !is_sl_font {
            if let LuaValue::Table(_) = &second_last {
                let last: LuaValue = styled_text.raw_get(new_len)?;
                if let Some(s) = last.as_string().map(|s| s.to_string_lossy()) {
                    if s == separator || s == separator2 {
                        styled_text.raw_set(new_len, LuaValue::Nil)?;
                        styled_text.raw_set(new_len - 1, LuaValue::Nil)?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Remove element at index `pos` from a Lua sequence table, shifting remaining elements down.
fn lua_table_remove(t: &LuaTable, pos: i64) -> LuaResult<()> {
    let len = t.raw_len() as i64;
    for i in pos..len {
        let next: LuaValue = t.raw_get(i + 1)?;
        t.raw_set(i, next)?;
    }
    t.raw_set(len, LuaValue::Nil)?;
    Ok(())
}

/// Create a spacing item between status bar items.
fn add_spacing(
    lua: &Lua,
    self_table: &LuaTable,
    destination: &LuaTable,
    separator: &str,
    alignment: i64,
    x: f64,
    item_class_key: &Arc<mlua::RegistryKey>,
) -> LuaResult<LuaTable> {
    let item_class: LuaTable = lua.registry_value(item_class_key)?;
    let style = require_table(lua, "core.style")?;

    let opts = lua.create_table()?;
    opts.set("name", "space")?;
    opts.set("alignment", alignment)?;
    let space: LuaTable = lua
        .load("return function(cls, ...) return cls(...) end")
        .eval::<LuaFunction>()?
        .call((item_class, opts))?;

    let self_sep: String = self_table
        .get::<Option<String>>("separator")?
        .unwrap_or_else(|| "      ".to_string());

    let cached = lua.create_table()?;
    if separator == self_sep {
        cached.push(style.get::<LuaValue>("text")?)?;
    } else {
        cached.push(style.get::<LuaValue>("dim")?)?;
    }
    cached.push(separator)?;
    space.set("cached_item", cached.clone())?;
    space.set("x", x)?;

    if separator == self_sep {
        let existing: LuaValue = self_table.get("_separator_width")?;
        let w: f64 = if matches!(existing, LuaValue::Nil) {
            let w = measure_styled(lua, self_table, &cached)?;
            self_table.set("_separator_width", w)?;
            w
        } else {
            self_table.get("_separator_width")?
        };
        space.set("w", w)?;
    } else {
        let existing: LuaValue = self_table.get("_separator2_width")?;
        let w: f64 = if matches!(existing, LuaValue::Nil) {
            let w = measure_styled(lua, self_table, &cached)?;
            self_table.set("_separator2_width", w)?;
            w
        } else {
            self_table.get("_separator2_width")?
        };
        space.set("w", w)?;
    }

    destination.push(space.clone())?;
    Ok(space)
}

/// Registers `core.statusview` as a pure-Rust preloaded module.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let native_module = make_native_module(lua)?;
    let native_key = Arc::new(lua.create_registry_value(native_module)?);

    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;

    preload.set(
        "statusview_native",
        lua.create_function({
            let k = Arc::clone(&native_key);
            move |lua, ()| lua.registry_value::<LuaTable>(&k)
        })?,
    )?;

    preload.set(
        "core.statusview",
        lua.create_function(|lua, ()| build_statusview_class(lua))?,
    )?;

    Ok(())
}

/// Build the native helper module table.
fn make_native_module(lua: &Lua) -> LuaResult<LuaTable> {
    let m = lua.create_table()?;
    m.set(
        "init",
        lua.create_function(|lua, self_table: LuaTable| init(lua, self_table))?,
    )?;
    m.set(
        "normalize_position",
        lua.create_function(|_, (items, pos, align): (LuaTable, i64, i64)| {
            normalize_position(&items, pos, align)
        })?,
    )?;
    m.set("get_item", lua.create_function(get_item)?)?;
    m.set("remove_item", lua.create_function(remove_item)?)?;
    m.set("move_item", lua.create_function(move_item)?)?;
    m.set("order_items", lua.create_function(order_items)?)?;
    m.set("get_items_list", lua.create_function(get_items_list)?)?;
    m.set("show_message", lua.create_function(show_message)?)?;
    m.set(
        "apply_panel_layout",
        lua.create_function(apply_panel_layout)?,
    )?;
    m.set("drag_panel", lua.create_function(drag_panel)?)?;
    m.set(
        "get_hovered_panel",
        lua.create_function(get_hovered_panel)?,
    )?;
    m.set(
        "get_item_visible_area",
        lua.create_function(get_item_visible_area)?,
    )?;
    m.set(
        "on_mouse_pressed",
        lua.create_function(on_mouse_pressed)?,
    )?;
    m.set("on_mouse_moved", lua.create_function(on_mouse_moved)?)?;
    m.set(
        "on_mouse_released",
        lua.create_function(on_mouse_released)?,
    )?;
    Ok(m)
}

/// Build the StatusView class entirely in Rust -- replaces the Lua bootstrap.
fn build_statusview_class(lua: &Lua) -> LuaResult<LuaValue> {
    let view_class: LuaTable = require_table(lua, "core.view")?;
    let object_class: LuaTable = require_table(lua, "core.object")?;
    let native: LuaTable = require_table(lua, "statusview_native")?;

    let status_view = view_class.call_method::<LuaTable>("extend", ())?;

    status_view.set(
        "__tostring",
        lua.create_function(|_lua, _self: LuaTable| Ok("StatusView"))?,
    )?;

    status_view.set("separator", "      ")?;
    status_view.set("separator2", "   |   ")?;

    // StatusViewItem class
    let item_class = object_class.call_method::<LuaTable>("extend", ())?;

    item_class.set(
        "__tostring",
        lua.create_function(|_lua, _self: LuaTable| Ok("StatusViewItem"))?,
    )?;
    item_class.set("LEFT", 1i64)?;
    item_class.set("RIGHT", 2i64)?;

    // StatusViewItem:new(options)
    item_class.set("new", {
        let command_mod_fn = lua.create_function(|lua, predicate: LuaValue| {
            let command: LuaTable = require_table(lua, "core.command")?;
            let gen_pred: LuaFunction = command.get("generate_predicate")?;
            gen_pred.call::<LuaFunction>(predicate)
        })?;
        lua.create_function(move |lua, (this, options): (LuaTable, LuaTable)| {
            let predicate_val: LuaValue = options.get("predicate")?;
            let pred_fn: LuaFunction = command_mod_fn.call(predicate_val)?;
            this.set("predicate", pred_fn)?;

            let name: String = options.get("name")?;
            this.set("name", name)?;

            let alignment: i64 = options.get::<Option<i64>>("alignment")?.unwrap_or(1);
            this.set("alignment", alignment)?;

            let cmd_val: LuaValue = options.get("command")?;
            match &cmd_val {
                LuaValue::String(_) => {
                    this.set("command", cmd_val.clone())?;
                    this.set("on_click", LuaValue::Nil)?;
                }
                LuaValue::Function(_) => {
                    this.set("command", LuaValue::Nil)?;
                    this.set("on_click", cmd_val.clone())?;
                }
                _ => {
                    this.set("command", LuaValue::Nil)?;
                    this.set("on_click", LuaValue::Nil)?;
                }
            }

            let tooltip: String =
                options.get::<Option<String>>("tooltip")?.unwrap_or_default();
            this.set("tooltip", tooltip)?;

            this.set("on_draw", LuaValue::Nil)?;
            this.set("background_color", LuaValue::Nil)?;
            this.set("background_color_hover", LuaValue::Nil)?;

            let visible: LuaValue = options.get("visible")?;
            let vis = match visible {
                LuaValue::Nil => true,
                LuaValue::Boolean(b) => b,
                _ => true,
            };
            this.set("visible", vis)?;
            this.set("active", false)?;
            this.set("x", 0.0f64)?;
            this.set("w", 0.0f64)?;

            let sep: LuaValue = options.get("separator")?;
            if matches!(sep, LuaValue::Nil) {
                this.set("separator", "      ")?;
            } else {
                this.set("separator", sep)?;
            }

            let get_item_fn: LuaValue = options.get("get_item")?;
            if matches!(get_item_fn, LuaValue::Nil) {
                this.set(
                    "get_item",
                    lua.create_function(|lua, _: LuaTable| lua.create_table())?,
                )?;
            } else {
                this.set("get_item", get_item_fn)?;
            }

            Ok(())
        })?
    })?;

    item_class.set(
        "hide",
        lua.create_function(|_lua, this: LuaTable| this.set("visible", false))?,
    )?;
    item_class.set(
        "show",
        lua.create_function(|_lua, this: LuaTable| this.set("visible", true))?,
    )?;
    item_class.set(
        "set_predicate",
        lua.create_function(|lua, (this, predicate): (LuaTable, LuaValue)| {
            let command: LuaTable = require_table(lua, "core.command")?;
            let gen_pred: LuaFunction = command.get("generate_predicate")?;
            let pred_fn: LuaFunction = gen_pred.call(predicate)?;
            this.set("predicate", pred_fn)
        })?,
    )?;

    status_view.set("Item", item_class.clone())?;

    let class_key = Arc::new(lua.create_registry_value(status_view.clone())?);
    let item_class_key = Arc::new(lua.create_registry_value(item_class)?);
    let native_key = Arc::new(lua.create_registry_value(native)?);

    // StatusView:new()
    status_view.set("new", {
        let ck = Arc::clone(&class_key);
        let nk = Arc::clone(&native_key);
        lua.create_function(move |lua, this: LuaTable| {
            let class: LuaTable = lua.registry_value(&ck)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_new: LuaFunction = super_tbl.get("new")?;
            super_new.call::<()>(this.clone())?;

            let native: LuaTable = lua.registry_value(&nk)?;
            let init_fn: LuaFunction = native.get("init")?;
            init_fn.call::<()>(this.clone())?;

            this.call_method::<()>("register_docview_items", ())?;
            this.call_method::<()>("register_command_items", ())?;
            Ok(())
        })?
    })?;

    // StatusView:add_item(options)
    status_view.set("add_item", {
        let nk = Arc::clone(&native_key);
        let ick = Arc::clone(&item_class_key);
        lua.create_function(move |lua, (this, options): (LuaTable, LuaTable)| {
            let name: String = options.get("name")?;
            let existing: LuaValue = this.call_method("get_item", name.clone())?;
            if !matches!(existing, LuaValue::Nil) {
                return Err(LuaError::RuntimeError(format!(
                    "status item already exists: {name}"
                )));
            }

            let item_class: LuaTable = lua.registry_value(&ick)?;
            let item: LuaValue = lua
                .load("return function(cls, ...) return cls(...) end")
                .eval::<LuaFunction>()?
                .call((item_class, options.clone()))?;

            let items: LuaTable = this.get("items")?;
            let native: LuaTable = lua.registry_value(&nk)?;
            let norm_fn: LuaFunction = native.get("normalize_position")?;
            let position: i64 = options.get::<Option<i64>>("position")?.unwrap_or(-1);
            let alignment: i64 = options.get::<Option<i64>>("alignment")?.unwrap_or(1);
            let pos: i64 = norm_fn.call((items.clone(), position, alignment))?;

            let len = items.raw_len() as i64;
            for i in (pos..=len).rev() {
                let val: LuaValue = items.raw_get(i)?;
                items.raw_set(i + 1, val)?;
            }
            items.raw_set(pos, item.clone())?;

            Ok(item)
        })?
    })?;

    // StatusView:get_item(name)
    status_view.set("get_item", {
        let nk = Arc::clone(&native_key);
        lua.create_function(move |lua, (this, name): (LuaTable, String)| {
            let native: LuaTable = lua.registry_value(&nk)?;
            let f: LuaFunction = native.get("get_item")?;
            f.call::<LuaValue>((this, name))
        })?
    })?;

    // StatusView:remove_item(name)
    status_view.set("remove_item", {
        let nk = Arc::clone(&native_key);
        lua.create_function(move |lua, (this, name): (LuaTable, String)| {
            let native: LuaTable = lua.registry_value(&nk)?;
            let f: LuaFunction = native.get("remove_item")?;
            f.call::<LuaValue>((this, name))
        })?
    })?;

    // StatusView:move_item(name, position, alignment)
    status_view.set("move_item", {
        let nk = Arc::clone(&native_key);
        lua.create_function(
            move |lua,
                  (this, name, position, alignment): (LuaTable, String, i64, Option<i64>)| {
                let native: LuaTable = lua.registry_value(&nk)?;
                let f: LuaFunction = native.get("move_item")?;
                f.call::<bool>((this, name, position, alignment))
            },
        )?
    })?;

    // StatusView:order_items(names)
    status_view.set("order_items", {
        let nk = Arc::clone(&native_key);
        lua.create_function(move |lua, (this, names): (LuaTable, LuaTable)| {
            let native: LuaTable = lua.registry_value(&nk)?;
            let f: LuaFunction = native.get("order_items")?;
            f.call::<()>((this, names))
        })?
    })?;

    // StatusView:get_items_list(alignment)
    status_view.set("get_items_list", {
        let nk = Arc::clone(&native_key);
        lua.create_function(move |lua, (this, alignment): (LuaTable, Option<i64>)| {
            let native: LuaTable = lua.registry_value(&nk)?;
            let f: LuaFunction = native.get("get_items_list")?;
            f.call::<LuaTable>((this, alignment))
        })?
    })?;

    // StatusView:hide/show/toggle
    status_view.set(
        "hide",
        lua.create_function(|_lua, this: LuaTable| this.set("visible", false))?,
    )?;
    status_view.set(
        "show",
        lua.create_function(|_lua, this: LuaTable| this.set("visible", true))?,
    )?;
    status_view.set(
        "toggle",
        lua.create_function(|_lua, this: LuaTable| {
            let v: bool = this.get("visible")?;
            this.set("visible", !v)
        })?,
    )?;

    // StatusView:hide_items(names)
    status_view.set(
        "hide_items",
        lua.create_function(|lua, (this, names): (LuaTable, LuaValue)| {
            let names = match names {
                LuaValue::String(s) => {
                    let t = lua.create_table()?;
                    t.raw_set(1, s)?;
                    Some(t)
                }
                LuaValue::Table(t) => Some(t),
                _ => None,
            };
            let items: LuaTable = this.get("items")?;
            match names {
                None => {
                    for item in items.sequence_values::<LuaTable>() {
                        item?.set("visible", false)?;
                    }
                }
                Some(names) => {
                    for name in names.sequence_values::<String>() {
                        let name = name?;
                        let item: LuaValue = this.call_method("get_item", name)?;
                        if let LuaValue::Table(t) = item {
                            t.set("visible", false)?;
                        }
                    }
                }
            }
            Ok(())
        })?,
    )?;

    // StatusView:show_items(names)
    status_view.set(
        "show_items",
        lua.create_function(|lua, (this, names): (LuaTable, LuaValue)| {
            let names = match names {
                LuaValue::String(s) => {
                    let t = lua.create_table()?;
                    t.raw_set(1, s)?;
                    Some(t)
                }
                LuaValue::Table(t) => Some(t),
                _ => None,
            };
            let items: LuaTable = this.get("items")?;
            match names {
                None => {
                    for item in items.sequence_values::<LuaTable>() {
                        item?.set("visible", true)?;
                    }
                }
                Some(names) => {
                    for name in names.sequence_values::<String>() {
                        let name = name?;
                        let item: LuaValue = this.call_method("get_item", name)?;
                        if let LuaValue::Table(t) = item {
                            t.set("visible", true)?;
                        }
                    }
                }
            }
            Ok(())
        })?,
    )?;

    // StatusView:show_message(icon, icon_color, text)
    status_view.set("show_message", {
        let nk = Arc::clone(&native_key);
        lua.create_function(
            move |lua,
                  (this, icon, icon_color, text): (LuaTable, String, LuaValue, String)| {
                let native: LuaTable = lua.registry_value(&nk)?;
                let f: LuaFunction = native.get("show_message")?;
                f.call::<()>((this, icon, icon_color, text))
            },
        )?
    })?;

    // StatusView:display_messages(enable)
    status_view.set(
        "display_messages",
        lua.create_function(|_lua, (this, enable): (LuaTable, bool)| {
            this.set("hide_messages", !enable)
        })?,
    )?;

    // StatusView:show_tooltip(text)
    status_view.set(
        "show_tooltip",
        lua.create_function(|lua, (this, text): (LuaTable, LuaValue)| {
            let tooltip = match &text {
                LuaValue::Table(_) => text,
                _ => {
                    let t = lua.create_table()?;
                    t.raw_set(1, text)?;
                    LuaValue::Table(t)
                }
            };
            this.set("tooltip", tooltip)?;
            this.set("tooltip_mode", true)
        })?,
    )?;

    // StatusView:remove_tooltip()
    status_view.set(
        "remove_tooltip",
        lua.create_function(|_lua, this: LuaTable| this.set("tooltip_mode", false))?,
    )?;

    // StatusView:drag_panel(panel, dx)
    status_view.set("drag_panel", {
        let nk = Arc::clone(&native_key);
        lua.create_function(move |lua, (this, panel, dx): (LuaTable, String, f64)| {
            let native: LuaTable = lua.registry_value(&nk)?;
            let f: LuaFunction = native.get("drag_panel")?;
            f.call::<()>((this, panel, dx))
        })?
    })?;

    // StatusView:get_hovered_panel(x, y)
    status_view.set("get_hovered_panel", {
        let nk = Arc::clone(&native_key);
        lua.create_function(move |lua, (this, x, y): (LuaTable, f64, f64)| {
            let native: LuaTable = lua.registry_value(&nk)?;
            let f: LuaFunction = native.get("get_hovered_panel")?;
            f.call::<String>((this, x, y))
        })?
    })?;

    // StatusView:get_item_visible_area(item)
    status_view.set("get_item_visible_area", {
        let nk = Arc::clone(&native_key);
        lua.create_function(move |lua, (this, item): (LuaTable, LuaTable)| {
            let native: LuaTable = lua.registry_value(&nk)?;
            let f: LuaFunction = native.get("get_item_visible_area")?;
            f.call::<(f64, f64)>((this, item))
        })?
    })?;

    // StatusView:update_active_items()
    status_view.set("update_active_items", {
        let nk = Arc::clone(&native_key);
        let ick = Arc::clone(&item_class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let (x, _y): (f64, f64) = this.call_method("get_content_offset", ())?;
            let size: LuaTable = this.get("size")?;
            let size_x: f64 = size.get("x")?;
            let mut rx = x + size_x;
            let mut lx = x;
            let mut rw: f64 = 0.0;
            let mut lw: f64 = 0.0;

            let active_items = lua.create_table()?;
            this.set("active_items", active_items.clone())?;
            let mut lfirst = true;
            let mut rfirst = true;

            let items: LuaTable = this.get("items")?;
            let style = require_table(lua, "core.style")?;
            let hovered_item: LuaValue = this.get("hovered_item")?;

            for item_val in items.clone().sequence_values::<LuaTable>() {
                let item = item_val?;
                let previous_cached: LuaValue = item.get("cached_item")?;
                let visible: bool = item.get::<Option<bool>>("visible")?.unwrap_or(true);
                let pred: Option<LuaFunction> = item.get("predicate")?;

                let pred_result = if visible {
                    pred.map_or(Ok(false), |f| f.call::<bool>(()))?
                } else {
                    false
                };

                if visible && pred_result {
                    let get_item_val: LuaValue = item.get("get_item")?;
                    let styled_text: LuaTable = match &get_item_val {
                        LuaValue::Function(f) => f.call(item.clone())?,
                        LuaValue::Table(t) => t.clone(),
                        _ => lua.create_table()?,
                    };

                    let st_len = styled_text.raw_len() as i64;
                    if st_len > 0 {
                        remove_spacing(&this, &styled_text, lua)?;
                    }

                    let new_len = styled_text.raw_len() as i64;
                    let has_on_draw: bool = item.get::<LuaValue>("on_draw")?.is_function();

                    if new_len > 0 || has_on_draw {
                        item.set("active", true)?;
                        let is_hovered = hovered_item == LuaValue::Table(item.clone());
                        let alignment: i64 =
                            item.get::<Option<i64>>("alignment")?.unwrap_or(1);

                        if alignment == 1 {
                            if !lfirst {
                                let sep: String = item
                                    .get::<Option<String>>("separator")?
                                    .unwrap_or_else(|| "      ".to_string());
                                let space = add_spacing(
                                    lua,
                                    &this,
                                    &active_items,
                                    &sep,
                                    1,
                                    lx,
                                    &ick,
                                )?;
                                let sw: f64 = space.get("w")?;
                                lw += sw;
                                lx += sw;
                            } else {
                                lfirst = false;
                            }

                            if has_on_draw {
                                let on_draw: LuaFunction = item.get("on_draw")?;
                                let pos: LuaTable = this.get("position")?;
                                let pos_y: f64 = pos.get("y")?;
                                let sz: LuaTable = this.get("size")?;
                                let sz_y: f64 = sz.get("y")?;
                                let w: f64 =
                                    on_draw.call((lx, pos_y, sz_y, is_hovered, true))?;
                                item.set("w", w)?;
                            } else {
                                let cached_item_val =
                                    LuaValue::Table(styled_text.clone());
                                let cached_w: Option<f64> = item.get("cached_width")?;
                                if styled_text_equals(
                                    &previous_cached,
                                    &cached_item_val,
                                )? && cached_w.is_some()
                                {
                                    item.set("w", cached_w)?;
                                } else {
                                    let w =
                                        measure_styled(lua, &this, &styled_text)?;
                                    item.set("w", w)?;
                                }
                            }
                            let w: f64 = item.get("w")?;
                            item.set("x", lx)?;
                            lw += w;
                            lx += w;
                        } else {
                            if !rfirst {
                                let sep: String = item
                                    .get::<Option<String>>("separator")?
                                    .unwrap_or_else(|| "      ".to_string());
                                let space = add_spacing(
                                    lua,
                                    &this,
                                    &active_items,
                                    &sep,
                                    2,
                                    rx,
                                    &ick,
                                )?;
                                let sw: f64 = space.get("w")?;
                                rw += sw;
                                rx += sw;
                            } else {
                                rfirst = false;
                            }

                            if has_on_draw {
                                let on_draw: LuaFunction = item.get("on_draw")?;
                                let pos: LuaTable = this.get("position")?;
                                let pos_y: f64 = pos.get("y")?;
                                let sz: LuaTable = this.get("size")?;
                                let sz_y: f64 = sz.get("y")?;
                                let w: f64 =
                                    on_draw.call((rx, pos_y, sz_y, is_hovered, true))?;
                                item.set("w", w)?;
                            } else {
                                let cached_item_val =
                                    LuaValue::Table(styled_text.clone());
                                let cached_w: Option<f64> = item.get("cached_width")?;
                                if styled_text_equals(
                                    &previous_cached,
                                    &cached_item_val,
                                )? && cached_w.is_some()
                                {
                                    item.set("w", cached_w)?;
                                } else {
                                    let w =
                                        measure_styled(lua, &this, &styled_text)?;
                                    item.set("w", w)?;
                                }
                            }
                            let w: f64 = item.get("w")?;
                            item.set("x", rx)?;
                            rw += w;
                            rx += w;
                        }
                        item.set("cached_item", styled_text)?;
                        let w: f64 = item.get("w")?;
                        item.set("cached_width", w)?;
                        active_items.push(item)?;
                    } else {
                        item.set("active", false)?;
                        item.set("cached_item", lua.create_table()?)?;
                        item.set("cached_width", 0.0f64)?;
                    }
                } else {
                    item.set("active", false)?;
                    item.set("cached_item", lua.create_table()?)?;
                    item.set("cached_width", 0.0f64)?;
                }
            }

            this.set("r_left_width", lw)?;
            this.set("r_right_width", rw)?;

            let native: LuaTable = lua.registry_value(&nk)?;
            let apply_fn: LuaFunction = native.get("apply_panel_layout")?;
            apply_fn.call::<()>((this.clone(), lw, rw))?;

            let right_width: f64 = this.get("right_width")?;
            let padding: LuaTable = style.get("padding")?;
            let padding_x: f64 = padding.get("x")?;

            for item in active_items.sequence_values::<LuaTable>() {
                let item = item?;
                let alignment: i64 =
                    item.get::<Option<i64>>("alignment")?.unwrap_or(1);
                if alignment == 2 {
                    let ix: f64 = item.get("x")?;
                    item.set("x", ix - right_width - padding_x * 2.0)?;
                }
                let native2: LuaTable = lua.registry_value(&nk)?;
                let vis_fn: LuaFunction = native2.get("get_item_visible_area")?;
                let (vx, vw): (f64, f64) =
                    vis_fn.call((this.clone(), item.clone()))?;
                item.set("visible_x", vx)?;
                item.set("visible_w", vw)?;
            }

            Ok(())
        })?
    })?;

    // StatusView:on_mouse_pressed(button, x, y, clicks)
    status_view.set("on_mouse_pressed", {
        let nk = Arc::clone(&native_key);
        lua.create_function(
            move |lua,
                  (this, button, x, y, clicks): (
                LuaTable,
                String,
                f64,
                f64,
                i64,
            )| {
                let native: LuaTable = lua.registry_value(&nk)?;
                let f: LuaFunction = native.get("on_mouse_pressed")?;
                f.call::<bool>((this, button, x, y, clicks))
            },
        )?
    })?;

    // StatusView:on_mouse_left()
    status_view.set("on_mouse_left", {
        let ck = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let class: LuaTable = lua.registry_value(&ck)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_fn: LuaFunction = super_tbl.get("on_mouse_left")?;
            super_fn.call::<()>(this.clone())?;
            this.set("hovered_item", lua.create_table()?)
        })?
    })?;

    // StatusView:on_mouse_moved(x, y, dx, dy)
    status_view.set("on_mouse_moved", {
        let nk = Arc::clone(&native_key);
        lua.create_function(
            move |lua, (this, x, y, dx, dy): (LuaTable, f64, f64, f64, f64)| {
                let native: LuaTable = lua.registry_value(&nk)?;
                let f: LuaFunction = native.get("on_mouse_moved")?;
                f.call::<()>((this, x, y, dx, dy))
            },
        )?
    })?;

    // StatusView:on_mouse_released(button, x, y)
    status_view.set("on_mouse_released", {
        let nk = Arc::clone(&native_key);
        lua.create_function(
            move |lua, (this, button, x, y): (LuaTable, String, f64, f64)| {
                let native: LuaTable = lua.registry_value(&nk)?;
                let f: LuaFunction = native.get("on_mouse_released")?;
                f.call::<()>((this, button, x, y))
            },
        )?
    })?;

    // StatusView:on_mouse_wheel(y, x)
    status_view.set(
        "on_mouse_wheel",
        lua.create_function(|_lua, (this, y, x): (LuaTable, f64, f64)| {
            let visible: bool = this.get("visible")?;
            let hovered_panel: String = this.get("hovered_panel")?;
            if !visible || hovered_panel.is_empty() {
                return Ok(());
            }
            let left_width: f64 = this.get("left_width")?;
            let amount = if x != 0.0 {
                x * left_width / 10.0
            } else {
                y * left_width / 10.0
            };
            this.call_method("drag_panel", (hovered_panel, amount))
        })?,
    )?;

    // StatusView:update()
    status_view.set("update", {
        let ck = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let visible: bool = this.get("visible")?;
            let size: LuaTable = this.get("size")?;
            let size_y: f64 = size.get("y")?;

            if !visible && size_y <= 0.0 {
                return Ok(());
            }
            if !visible && size_y > 0.0 {
                this.call_method::<()>(
                    "move_towards",
                    (size.clone(), "y", 0.0, LuaValue::Nil, "statusbar"),
                )?;
                return Ok(());
            }

            let style = require_table(lua, "core.style")?;
            let font: LuaValue = style.get("font")?;
            let fh: f64 = match &font {
                LuaValue::Table(t) => t.call_method("get_height", ())?,
                LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                _ => 14.0,
            };
            let padding: LuaTable = style.get("padding")?;
            let py: f64 = padding.get("y")?;
            let height = fh + py * 2.0;

            if size_y + 1.0 < height {
                this.call_method::<()>(
                    "move_towards",
                    (size.clone(), "y", height, LuaValue::Nil, "statusbar"),
                )?;
            } else {
                size.set("y", height)?;
            }

            let system = require_table(lua, "system")?;
            let get_time: LuaFunction = system.get("get_time")?;
            let now: f64 = get_time.call(())?;
            let msg_timeout: f64 = this.get("message_timeout")?;
            let scroll: LuaTable = this.get("scroll")?;
            let scroll_to: LuaTable = scroll.get("to")?;
            let cur_size_y: f64 = size.get("y")?;
            if now < msg_timeout {
                scroll_to.set("y", cur_size_y)?;
            } else {
                scroll_to.set("y", 0.0f64)?;
            }

            let class: LuaTable = lua.registry_value(&ck)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_update: LuaFunction = super_tbl.get("update")?;
            super_update.call::<()>(this.clone())?;
            this.call_method::<()>("update_active_items", ())
        })?
    })?;

    // StatusView:draw_items(items, right_align, xoffset, yoffset)
    status_view.set(
        "draw_items",
        lua.create_function(
            |lua,
             (this, items, right_align, xoffset, yoffset): (
                LuaTable,
                LuaTable,
                Option<bool>,
                Option<f64>,
                Option<f64>,
            )| {
                let (mut x, y_base): (f64, f64) =
                    this.call_method("get_content_offset", ())?;
                x += xoffset.unwrap_or(0.0);
                let y = y_base + yoffset.unwrap_or(0.0);
                let style = require_table(lua, "core.style")?;
                let padding: LuaTable = style.get("padding")?;
                let padding_x: f64 = padding.get("x")?;
                let common: LuaTable = require_table(lua, "core.common")?;
                let draw_text_fn: LuaFunction = common.get("draw_text")?;
                let tw_fn = text_width_fn(lua)?;

                if right_align.unwrap_or(false) {
                    let w = draw_items_with(lua, &this, &items, 0.0, 0.0, &tw_fn)?;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    x = x + size_x - w - padding_x;
                    draw_items_with(lua, &this, &items, x, y, &draw_text_fn)?;
                } else {
                    x += padding_x;
                    draw_items_with(lua, &this, &items, x, y, &draw_text_fn)?;
                }
                Ok(())
            },
        )?,
    )?;

    // StatusView:draw_item_tooltip(item)
    status_view.set(
        "draw_item_tooltip",
        lua.create_function(|lua, (this, item): (LuaTable, LuaTable)| {
            let core = require_table(lua, "core")?;
            let root_view: LuaTable = core.get("root_view")?;
            let text: String = item.get("tooltip")?;
            let pos_y: f64 = {
                let position: LuaTable = this.get("position")?;
                position.get("y")?
            };
            let size_x: f64 = {
                let size: LuaTable = this.get("size")?;
                size.get("x")?
            };
            let pointer: LuaTable = this.get("pointer")?;
            let pointer_x: f64 = pointer.get("x")?;
            let style = require_table(lua, "core.style")?;
            let bg3: LuaValue = style.get("background3")?;
            let style_text: LuaValue = style.get("text")?;

            let draw_fn = lua.create_function(move |lua, ()| {
                let style = require_table(lua, "core.style")?;
                let renderer: LuaTable = lua.globals().get("renderer")?;
                let font: LuaValue = style.get("font")?;
                let padding: LuaTable = style.get("padding")?;
                let px: f64 = padding.get("x")?;
                let py: f64 = padding.get("y")?;

                let w: f64 = match &font {
                    LuaValue::Table(t) => t.call_method("get_width", text.clone())?,
                    LuaValue::UserData(ud) => {
                        ud.call_method("get_width", text.clone())?
                    }
                    _ => 0.0,
                };
                let h: f64 = match &font {
                    LuaValue::Table(t) => t.call_method("get_height", ())?,
                    LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                    _ => 14.0,
                };

                let mut x = pointer_x - (w / 2.0) - (px * 2.0);
                if x < 0.0 {
                    x = 0.0;
                }
                if (x + w + px * 3.0) > size_x {
                    x = size_x - w - px * 3.0;
                }

                renderer.call_function::<()>(
                    "draw_rect",
                    (
                        x + px,
                        pos_y - h - py * 2.0,
                        w + px * 2.0,
                        h + py * 2.0,
                        bg3.clone(),
                    ),
                )?;
                renderer.call_function::<()>(
                    "draw_text",
                    (
                        font,
                        text.clone(),
                        x + px * 2.0,
                        pos_y - h - py,
                        style_text.clone(),
                    ),
                )?;
                Ok(())
            })?;
            root_view.call_method::<()>("defer_draw", draw_fn)
        })?,
    )?;

    // StatusView:draw()
    status_view.set(
        "draw",
        lua.create_function(|lua, this: LuaTable| {
            let visible: bool = this.get("visible")?;
            let size: LuaTable = this.get("size")?;
            let size_y: f64 = size.get("y")?;
            if !visible && size_y <= 0.0 {
                return Ok(());
            }

            let style = require_table(lua, "core.style")?;
            let bg2: LuaValue = style.get("background2")?;
            this.call_method::<()>("draw_background", bg2)?;

            let system = require_table(lua, "system")?;
            let get_time: LuaFunction = system.get("get_time")?;
            let now: f64 = get_time.call(())?;
            let msg_timeout: f64 = this.get("message_timeout")?;
            let message: LuaValue = this.get("message")?;

            if message.is_table() && now <= msg_timeout {
                let msg_tbl: LuaTable = this.get("message")?;
                this.call_method::<()>("draw_items", (msg_tbl, false, 0.0, size_y))?;
            } else {
                let tooltip_mode: bool = this.get("tooltip_mode")?;
                if tooltip_mode {
                    let tooltip: LuaTable = this.get("tooltip")?;
                    this.call_method::<()>("draw_items", tooltip)?;
                }

                let active_items: LuaTable = this.get("active_items")?;
                if active_items.raw_len() > 0 {
                    let core = require_table(lua, "core")?;
                    let push_clip: LuaFunction = core.get("push_clip_rect")?;
                    let pop_clip: LuaFunction = core.get("pop_clip_rect")?;
                    let renderer: LuaTable = lua.globals().get("renderer")?;
                    let padding: LuaTable = style.get("padding")?;
                    let padding_x: f64 = padding.get("x")?;
                    let position: LuaTable = this.get("position")?;
                    let pos_y: f64 = position.get("y")?;
                    let left_width: f64 = this.get("left_width")?;
                    let right_width: f64 = this.get("right_width")?;
                    let left_xoffset: f64 = this.get("left_xoffset")?;
                    let right_xoffset: f64 = this.get("right_xoffset")?;
                    let size_x: f64 = size.get("x")?;
                    let hovered_item: LuaValue = this.get("hovered_item")?;

                    // Draw left panel items
                    push_clip.call::<()>((
                        0.0,
                        pos_y,
                        left_width + padding_x,
                        size_y,
                    ))?;
                    for item in active_items.clone().sequence_values::<LuaTable>() {
                        let item = item?;
                        let alignment: i64 =
                            item.get::<Option<i64>>("alignment")?.unwrap_or(1);
                        if alignment != 1 || tooltip_mode {
                            continue;
                        }
                        let item_x_raw: f64 = item.get("x")?;
                        let item_x = left_xoffset + item_x_raw + padding_x;
                        let is_hovered =
                            hovered_item == LuaValue::Table(item.clone());
                        let item_bg: LuaValue = if is_hovered {
                            item.get("background_color_hover")?
                        } else {
                            item.get("background_color")?
                        };
                        let item_w: f64 = item.get("w")?;
                        if let LuaValue::Table(_) = &item_bg {
                            renderer.call_function::<()>(
                                "draw_rect",
                                (item_x, pos_y, item_w, size_y, item_bg),
                            )?;
                        }
                        let has_on_draw: bool =
                            item.get::<LuaValue>("on_draw")?.is_function();
                        if has_on_draw {
                            let on_draw: LuaFunction = item.get("on_draw")?;
                            push_clip.call::<()>((
                                item_x, pos_y, item_w, size_y,
                            ))?;
                            on_draw.call::<()>((
                                item_x, pos_y, size_y, is_hovered,
                            ))?;
                            pop_clip.call::<()>(())?;
                        } else {
                            let cached: LuaTable = item.get("cached_item")?;
                            this.call_method::<()>(
                                "draw_items",
                                (cached, false, item_x - padding_x),
                            )?;
                        }
                    }
                    pop_clip.call::<()>(())?;

                    // Draw right panel items
                    push_clip.call::<()>((
                        size_x - (right_width + padding_x),
                        pos_y,
                        right_width + padding_x,
                        size_y,
                    ))?;
                    for item in active_items.clone().sequence_values::<LuaTable>() {
                        let item = item?;
                        let alignment: i64 =
                            item.get::<Option<i64>>("alignment")?.unwrap_or(1);
                        if alignment != 2 {
                            continue;
                        }
                        let item_x_raw: f64 = item.get("x")?;
                        let item_x = right_xoffset + item_x_raw + padding_x;
                        let is_hovered =
                            hovered_item == LuaValue::Table(item.clone());
                        let item_bg: LuaValue = if is_hovered {
                            item.get("background_color_hover")?
                        } else {
                            item.get("background_color")?
                        };
                        let item_w: f64 = item.get("w")?;
                        if let LuaValue::Table(_) = &item_bg {
                            renderer.call_function::<()>(
                                "draw_rect",
                                (item_x, pos_y, item_w, size_y, item_bg),
                            )?;
                        }
                        let has_on_draw: bool =
                            item.get::<LuaValue>("on_draw")?.is_function();
                        if has_on_draw {
                            let on_draw: LuaFunction = item.get("on_draw")?;
                            push_clip.call::<()>((
                                item_x, pos_y, item_w, size_y,
                            ))?;
                            on_draw.call::<()>((
                                item_x, pos_y, size_y, is_hovered,
                            ))?;
                            pop_clip.call::<()>(())?;
                        } else {
                            let cached: LuaTable = item.get("cached_item")?;
                            this.call_method::<()>(
                                "draw_items",
                                (cached, false, item_x - padding_x),
                            )?;
                        }
                    }
                    pop_clip.call::<()>(())?;

                    // Draw tooltip for hovered item
                    if let LuaValue::Table(ref hi) = hovered_item {
                        let tooltip: String = hi
                            .get::<Option<String>>("tooltip")?
                            .unwrap_or_default();
                        let active: bool =
                            hi.get::<Option<bool>>("active")?.unwrap_or(false);
                        if !tooltip.is_empty() && active {
                            this.call_method::<()>(
                                "draw_item_tooltip",
                                hi.clone(),
                            )?;
                        }
                    }
                }
            }
            Ok(())
        })?,
    )?;

    // StatusView:register_docview_items()
    status_view.set(
        "register_docview_items",
        lua.create_function(|lua, this: LuaTable| {
            let existing: LuaValue = this.call_method("get_item", "doc:file")?;
            if !matches!(existing, LuaValue::Nil) {
                return Ok(());
            }

            // doc:file
            let opts = lua.create_table()?;
            opts.set("predicate", "core.docview")?;
            opts.set("name", "doc:file")?;
            opts.set("alignment", 1i64)?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let core = require_table(lua, "core")?;
                    let style = require_table(lua, "core.style")?;
                    let common = require_table(lua, "core.common")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaTable = dv.get("doc")?;
                    let is_dirty: bool = doc.call_method("is_dirty", ())?;
                    let t = lua.create_table()?;
                    t.push(if is_dirty {
                        style.get::<LuaValue>("accent")?
                    } else {
                        style.get::<LuaValue>("text")?
                    })?;
                    t.push(style.get::<LuaValue>("icon_font")?)?;
                    t.push("f")?;
                    t.push(style.get::<LuaValue>("dim")?)?;
                    t.push(style.get::<LuaValue>("font")?)?;
                    t.push("   |   ")?;
                    t.push(style.get::<LuaValue>("text")?)?;
                    let filename: LuaValue = doc.get("filename")?;
                    t.push(if filename.is_nil() {
                        style.get::<LuaValue>("dim")?
                    } else {
                        style.get::<LuaValue>("text")?
                    })?;
                    let name: String = doc.call_method("get_name", ())?;
                    let encoded: String =
                        common.call_function("home_encode", name)?;
                    t.push(encoded)?;
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)?;

            // doc:position
            let opts = lua.create_table()?;
            opts.set("predicate", "core.docview")?;
            opts.set("name", "doc:position")?;
            opts.set("alignment", 1i64)?;
            opts.set("command", "doc:go-to-line")?;
            opts.set("tooltip", "line : column")?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let core = require_table(lua, "core")?;
                    let style = require_table(lua, "core.style")?;
                    let config = require_table(lua, "core.config")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaTable = dv.get("doc")?;
                    let sel: LuaMultiValue =
                        doc.call_method("get_selection", ())?;
                    let mut iter = sel.into_iter();
                    let line: i64 = match iter.next() {
                        Some(LuaValue::Integer(n)) => n,
                        _ => 1,
                    };
                    let mut col: i64 = match iter.next() {
                        Some(LuaValue::Integer(n)) => n,
                        _ => 1,
                    };

                    let indent_info: LuaMultiValue =
                        doc.call_method("get_indent_info", ())?;
                    let mut ii = indent_info.into_iter();
                    let _indent_type = ii.next();
                    let indent_size: i64 = match ii.next() {
                        Some(LuaValue::Integer(n)) => n,
                        _ => 4,
                    };

                    let lines: LuaTable = doc.get("lines")?;
                    let line_str: String =
                        lines.get::<Option<String>>(line)?.unwrap_or_default();
                    let mut ntabs: i64 = 0;
                    let mut last_idx = 0usize;
                    while last_idx < col as usize {
                        if let Some(pos) = line_str[last_idx..].find('\t') {
                            let s = last_idx + pos;
                            if (s + 1) < col as usize {
                                ntabs += 1;
                                last_idx = s + 1;
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    col += ntabs * (indent_size - 1);

                    let line_limit: i64 = config.get("line_limit")?;
                    let t = lua.create_table()?;
                    t.push(style.get::<LuaValue>("text")?)?;
                    t.push(line.to_string())?;
                    t.push(":")?;
                    t.push(if col > line_limit {
                        style.get::<LuaValue>("accent")?
                    } else {
                        style.get::<LuaValue>("text")?
                    })?;
                    t.push(col.to_string())?;
                    t.push(style.get::<LuaValue>("text")?)?;
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)?;

            // doc:position-percent
            let opts = lua.create_table()?;
            opts.set("predicate", "core.docview")?;
            opts.set("name", "doc:position-percent")?;
            opts.set("alignment", 1i64)?;
            opts.set("tooltip", "caret position")?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let core = require_table(lua, "core")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaTable = dv.get("doc")?;
                    let sel: LuaMultiValue =
                        doc.call_method("get_selection", ())?;
                    let line: f64 = match sel.into_iter().next() {
                        Some(LuaValue::Integer(n)) => n as f64,
                        Some(LuaValue::Number(n)) => n,
                        _ => 1.0,
                    };
                    let lines: LuaTable = doc.get("lines")?;
                    let total = lines.raw_len() as f64;
                    let pct = line / total * 100.0;
                    let t = lua.create_table()?;
                    t.push(format!("{:.0}%", pct))?;
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)?;

            // doc:selections
            let opts = lua.create_table()?;
            opts.set("predicate", "core.docview")?;
            opts.set("name", "doc:selections")?;
            opts.set("alignment", 1i64)?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let core = require_table(lua, "core")?;
                    let style = require_table(lua, "core.style")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaTable = dv.get("doc")?;
                    let selections: LuaTable = doc.get("selections")?;
                    let nsel = selections.raw_len() / 4;
                    let t = lua.create_table()?;
                    if nsel > 1 {
                        t.push(style.get::<LuaValue>("text")?)?;
                        t.push(nsel.to_string())?;
                        t.push(" selections")?;
                    }
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)?;

            // doc:indentation (RIGHT)
            let opts = lua.create_table()?;
            opts.set("predicate", "core.docview")?;
            opts.set("name", "doc:indentation")?;
            opts.set("alignment", 2i64)?;
            let sep2 = this
                .get::<Option<String>>("separator2")?
                .unwrap_or_else(|| "   |   ".to_string());
            opts.set("separator", sep2)?;
            opts.set(
                "command",
                lua.create_function(
                    |lua, (button, _x, _y): (String, f64, f64)| {
                        let command = require_table(lua, "core.command")?;
                        let perform: LuaFunction = command.get("perform")?;
                        if button == "left" {
                            perform
                                .call::<()>("indent:set-file-indent-size")?;
                        } else if button == "right" {
                            perform
                                .call::<()>("indent:set-file-indent-type")?;
                        }
                        Ok(())
                    },
                )?,
            )?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let core = require_table(lua, "core")?;
                    let style = require_table(lua, "core.style")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaTable = dv.get("doc")?;
                    let info: LuaMultiValue =
                        doc.call_method("get_indent_info", ())?;
                    let mut vals = info.into_iter();
                    let indent_type: String = match vals.next() {
                        Some(LuaValue::String(s)) => {
                            s.to_str()?.to_string()
                        }
                        _ => "soft".to_string(),
                    };
                    let indent_size: i64 = match vals.next() {
                        Some(LuaValue::Integer(n)) => n,
                        _ => 4,
                    };
                    let indent_confirmed: bool = match vals.next() {
                        Some(LuaValue::Boolean(b)) => b,
                        _ => false,
                    };
                    let label = if indent_type == "hard" {
                        "tabs: "
                    } else {
                        "spaces: "
                    };
                    let t = lua.create_table()?;
                    t.push(style.get::<LuaValue>("text")?)?;
                    t.push(label)?;
                    t.push(indent_size.to_string())?;
                    t.push(if indent_confirmed { "" } else { "*" })?;
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)?;

            // doc:lines (RIGHT)
            let opts = lua.create_table()?;
            opts.set("predicate", "core.docview")?;
            opts.set("name", "doc:lines")?;
            opts.set("alignment", 2i64)?;
            let sep2 = this
                .get::<Option<String>>("separator2")?
                .unwrap_or_else(|| "   |   ".to_string());
            opts.set("separator", sep2)?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let core = require_table(lua, "core")?;
                    let style = require_table(lua, "core.style")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaTable = dv.get("doc")?;
                    let lines: LuaTable = doc.get("lines")?;
                    let count = lines.raw_len();
                    let t = lua.create_table()?;
                    t.push(style.get::<LuaValue>("text")?)?;
                    t.push(count.to_string())?;
                    t.push(" lines")?;
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)?;

            // doc:line-ending (RIGHT)
            let opts = lua.create_table()?;
            opts.set("predicate", "core.docview")?;
            opts.set("name", "doc:line-ending")?;
            opts.set("alignment", 2i64)?;
            opts.set("command", "doc:toggle-line-ending")?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let core = require_table(lua, "core")?;
                    let style = require_table(lua, "core.style")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaTable = dv.get("doc")?;
                    let crlf: bool =
                        doc.get::<Option<bool>>("crlf")?.unwrap_or(false);
                    let t = lua.create_table()?;
                    t.push(style.get::<LuaValue>("text")?)?;
                    t.push(if crlf { "CRLF" } else { "LF" })?;
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)?;

            // doc:overwrite-mode (RIGHT)
            let opts = lua.create_table()?;
            opts.set("predicate", "core.docview")?;
            opts.set("name", "doc:overwrite-mode")?;
            opts.set("alignment", 2i64)?;
            opts.set("command", "doc:toggle-overwrite")?;
            opts.set("separator", "   |   ")?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let core = require_table(lua, "core")?;
                    let style = require_table(lua, "core.style")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaTable = dv.get("doc")?;
                    let overwrite: bool =
                        doc.get::<Option<bool>>("overwrite")?.unwrap_or(false);
                    let t = lua.create_table()?;
                    t.push(style.get::<LuaValue>("text")?)?;
                    t.push(if overwrite { "OVR" } else { "INS" })?;
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)?;

            // doc:mode (RIGHT)
            let opts = lua.create_table()?;
            opts.set("predicate", "core.docview")?;
            opts.set("name", "doc:mode")?;
            opts.set("alignment", 2i64)?;
            opts.set("separator", "   |   ")?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let core = require_table(lua, "core")?;
                    let style = require_table(lua, "core.style")?;
                    let dv: LuaTable = core.get("active_view")?;
                    let doc: LuaTable = dv.get("doc")?;
                    let large: bool = doc
                        .get::<Option<bool>>("large_file_mode")?
                        .unwrap_or(false);
                    let read_only: bool =
                        doc.get::<Option<bool>>("read_only")?.unwrap_or(false);
                    let t = lua.create_table()?;
                    if !large && !read_only {
                        return Ok(t);
                    }
                    if large {
                        t.push(style.get::<LuaValue>("warn")?)?;
                        t.push("LARGE")?;
                    }
                    if read_only {
                        if large {
                            t.push(style.get::<LuaValue>("dim")?)?;
                            t.push(" ")?;
                        }
                        t.push(style.get::<LuaValue>("accent")?)?;
                        t.push("RO")?;
                    }
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)?;

            Ok(())
        })?,
    )?;

    // StatusView:register_command_items()
    status_view.set(
        "register_command_items",
        lua.create_function(|lua, this: LuaTable| {
            let existing: LuaValue =
                this.call_method("get_item", "command:files")?;
            if !matches!(existing, LuaValue::Nil) {
                return Ok(());
            }
            let opts = lua.create_table()?;
            opts.set("predicate", "core.commandview")?;
            opts.set("name", "command:files")?;
            opts.set("alignment", 2i64)?;
            opts.set(
                "get_item",
                lua.create_function(|lua, _item: LuaTable| {
                    let style = require_table(lua, "core.style")?;
                    let t = lua.create_table()?;
                    t.push(style.get::<LuaValue>("icon_font")?)?;
                    t.push("g")?;
                    Ok(t)
                })?,
            )?;
            this.call_method::<()>("add_item", opts)
        })?,
    )?;

    Ok(LuaValue::Table(status_view))
}
