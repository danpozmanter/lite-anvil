use mlua::prelude::*;
use parking_lot::Mutex;
use std::sync::Arc;


fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Compare two LuaValues by reference (for tables/userdata) or by value.
fn lua_val_eq(a: &LuaValue, b: &LuaValue) -> bool {
    match (a, b) {
        (LuaValue::Nil, LuaValue::Nil) => true,
        (LuaValue::Boolean(x), LuaValue::Boolean(y)) => x == y,
        (LuaValue::Integer(x), LuaValue::Integer(y)) => x == y,
        (LuaValue::Number(x), LuaValue::Number(y)) => x == y,
        (LuaValue::String(x), LuaValue::String(y)) => x.as_bytes() == y.as_bytes(),
        (LuaValue::Table(x), LuaValue::Table(y)) => x == y,
        (LuaValue::UserData(x), LuaValue::UserData(y)) => x.to_pointer() == y.to_pointer(),
        _ => false,
    }
}

fn font_get_height(font: &LuaValue) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_height", ()),
        LuaValue::UserData(ud) => ud.call_method("get_height", ()),
        _ => Err(LuaError::RuntimeError("expected font table or userdata".into())),
    }
}

fn font_get_width(font: &LuaValue, text: &str) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_width", text.to_owned()),
        LuaValue::UserData(ud) => ud.call_method("get_width", text.to_owned()),
        _ => Err(LuaError::RuntimeError("expected font table or userdata".into())),
    }
}

/// Iterate the iterator function returned by `each_item()`, yielding parsed values.
fn each_item_loop<F>(iter_fn: &LuaFunction, mut f: F) -> LuaResult<()>
where
    F: FnMut(LuaTable, f64, f64, f64, f64) -> LuaResult<bool>,
{
    loop {
        let results: LuaMultiValue = iter_fn.call(())?;
        let mut vals = results.into_iter();
        let item_val = match vals.next() {
            Some(v) if !matches!(v, LuaValue::Nil) => v,
            _ => break,
        };
        let item = match item_val {
            LuaValue::Table(t) => t,
            _ => break,
        };
        let x = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
        let y = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
        let w = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
        let h = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
        if !f(item, x, y, w, h)? {
            break;
        }
    }
    Ok(())
}

fn populate(lua: &Lua, class: LuaTable) -> LuaResult<()> {
    let class_key = Arc::new(lua.create_registry_value(class.clone())?);

    // new(self)
    class.set("new", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_new: LuaFunction = super_tbl.get("new")?;
            super_new.call::<()>(this.clone())?;

            this.set("visible", true)?;
            this.set("init_size", true)?;
            this.set("tooltip", false)?;

            let style: LuaTable = require_table(lua, "core.style")?;
            let toolbar_font: LuaValue = style.call_function("get_icon_big_font", ())?;
            this.set("toolbar_font", toolbar_font)?;

            let commands = lua.create_table()?;
            for (sym, cmd) in &[
                ("f", "core:new-doc"),
                ("D", "core:open-file"),
                ("S", "doc:save"),
                ("L", "find-replace:find"),
                ("B", "core:find-command"),
                ("P", "core:open-user-module"),
            ] {
                let e = lua.create_table()?;
                e.set("symbol", *sym)?;
                e.set("command", *cmd)?;
                commands.push(e)?;
            }
            this.set("toolbar_commands", commands)?;
            this.set("_icon_metrics", LuaValue::Nil)?;
            Ok(())
        })?
    })?;

    // update(self)
    class.set("update", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let visible: bool = this.get("visible").unwrap_or(false);
            let dest_size: f64 = if visible {
                let toolbar_font: LuaValue = this.get("toolbar_font")?;
                let h = font_get_height(&toolbar_font)?;
                let style: LuaTable = require_table(lua, "core.style")?;
                let padding: LuaTable = style.get("padding")?;
                let py: f64 = padding.get("y")?;
                h + py * 2.0
            } else {
                0.0
            };

            let init_size: LuaValue = this.get("init_size")?;
            if !matches!(init_size, LuaValue::Nil | LuaValue::Boolean(false)) {
                let size: LuaTable = this.get("size")?;
                size.set("y", dest_size)?;
                this.set("init_size", LuaValue::Nil)?;
            } else {
                let size: LuaTable = this.get("size")?;
                this.call_method::<()>("move_towards", (size, "y", dest_size))?;
            }

            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_update: LuaFunction = super_tbl.get("update")?;
            super_update.call::<()>(this)
        })?
    })?;

    // toggle_visible(self)
    class.set("toggle_visible", lua.create_function(|lua, this: LuaTable| {
        let visible: bool = this.get("visible").unwrap_or(false);
        this.set("visible", !visible)?;
        let tooltip: bool = this.get("tooltip").unwrap_or(false);
        if tooltip {
            let core: LuaTable = require_table(lua, "core")?;
            let sv: LuaTable = core.get("status_view")?;
            sv.call_method::<()>("remove_tooltip", ())?;
            this.set("tooltip", false)?;
        }
        this.set("hovered_item", LuaValue::Nil)
    })?)?;

    // get_icon_width(self)
    class.set("get_icon_width", lua.create_function(|_lua, this: LuaTable| {
        let metrics: LuaTable = this.call_method("get_icon_metrics", ())?;
        metrics.get::<f64>("width")
    })?)?;

    // get_icon_metrics(self) — caches on self._icon_metrics
    class.set("get_icon_metrics", lua.create_function(|lua, this: LuaTable| {
        let toolbar_font: LuaValue = this.get("toolbar_font")?;
        let cached: LuaValue = this.get("_icon_metrics")?;
        if let LuaValue::Table(m) = &cached {
            let cached_font: LuaValue = m.get("font")?;
            if lua_val_eq(&cached_font, &toolbar_font) {
                return Ok(m.clone());
            }
        }

        let commands: LuaTable = this.get("toolbar_commands")?;
        let mut max_width = 0.0f64;
        for i in 1..=commands.raw_len() {
            let cmd: LuaTable = commands.raw_get(i as i64)?;
            let font: LuaValue = cmd.get("font").unwrap_or(LuaValue::Nil);
            let font = if matches!(font, LuaValue::Nil) { toolbar_font.clone() } else { font };
            let symbol: String = cmd.get("symbol")?;
            let w = font_get_width(&font, &symbol)?;
            if w > max_width { max_width = w; }
        }

        let height = font_get_height(&toolbar_font)?;
        let metrics = lua.create_table()?;
        metrics.set("font", toolbar_font.clone())?;
        metrics.set("width", max_width)?;
        metrics.set("height", height)?;
        metrics.set("spacing", max_width / 2.0)?;
        this.set("_icon_metrics", metrics.clone())?;
        Ok(metrics)
    })?)?;

    // on_scale_change(self)
    class.set("on_scale_change", lua.create_function(|lua, this: LuaTable| {
        let style: LuaTable = require_table(lua, "core.style")?;
        let toolbar_font: LuaValue = style.call_function("get_icon_big_font", ())?;
        this.set("toolbar_font", toolbar_font)?;
        this.set("_icon_metrics", LuaValue::Nil)
    })?)?;

    // each_item(self) — returns an iterator closure
    class.set("each_item", lua.create_function(|lua, this: LuaTable| {
        let metrics: LuaTable = this.call_method("get_icon_metrics", ())?;
        let icon_h: f64 = metrics.get("height")?;
        let icon_w: f64 = metrics.get("width")?;
        let toolbar_spacing: f64 = metrics.get("spacing")?;
        let (ox, oy): (f64, f64) = this.call_method("get_content_offset", ())?;

        let style: LuaTable = require_table(lua, "core.style")?;
        let padding: LuaTable = style.get("padding")?;
        let padding_x: f64 = padding.get("x")?;
        let padding_y: f64 = padding.get("y")?;

        let this_key = Arc::new(lua.create_registry_value(this)?);
        let index = Arc::new(Mutex::new(0i64));

        let iter = lua.create_function(move |lua, ()| {
            let mut idx = index.lock();
            *idx += 1;
            let i = *idx;

            let this: LuaTable = lua.registry_value(&this_key)?;
            let commands: LuaTable = this.get("toolbar_commands")?;
            let len = commands.raw_len() as i64;
            if i > len {
                return Ok(LuaMultiValue::new());
            }

            let dx = padding_x + (icon_w + toolbar_spacing) * (i - 1) as f64;
            let size: LuaTable = this.get("size")?;
            let size_x: f64 = size.get("x")?;
            if dx + icon_w > size_x {
                return Ok(LuaMultiValue::new());
            }

            let item: LuaTable = commands.get(i)?;
            Ok(LuaMultiValue::from_vec(vec![
                LuaValue::Table(item),
                LuaValue::Number(ox + dx),
                LuaValue::Number(oy + padding_y),
                LuaValue::Number(icon_w),
                LuaValue::Number(icon_h),
            ]))
        })?;
        Ok(LuaValue::Function(iter))
    })?)?;

    // get_min_width(self)
    class.set("get_min_width", lua.create_function(|lua, this: LuaTable| {
        let metrics: LuaTable = this.call_method("get_icon_metrics", ())?;
        let icon_w: f64 = metrics.get("width")?;
        let space: f64 = metrics.get("spacing")?;
        let style: LuaTable = require_table(lua, "core.style")?;
        let padding: LuaTable = style.get("padding")?;
        let padding_x: f64 = padding.get("x")?;
        let commands: LuaTable = this.get("toolbar_commands")?;
        let n = commands.raw_len() as f64;
        Ok(2.0 * padding_x + (icon_w + space) * n - space)
    })?)?;

    // draw(self)
    class.set("draw", lua.create_function(|lua, this: LuaTable| {
        let visible: bool = this.get("visible").unwrap_or(false);
        if !visible { return Ok(()); }

        let style: LuaTable = require_table(lua, "core.style")?;
        let bg2: LuaValue = style.get("background2")?;
        this.call_method::<()>("draw_background", bg2)?;

        let hovered: LuaValue = this.get("hovered_item")?;
        let command: LuaTable = require_table(lua, "core.command")?;
        let common: LuaTable = require_table(lua, "core.common")?;
        let style_text: LuaValue = style.get("text")?;
        let style_dim: LuaValue = style.get("dim")?;
        let toolbar_font: LuaValue = this.get("toolbar_font")?;

        let iter_fn: LuaFunction = this.call_method("each_item", ())?;
        each_item_loop(&iter_fn, |item, x, y, _w, h| {
            let item_cmd: String = item.get("command")?;
            let is_hovered = lua_val_eq(&LuaValue::Table(item.clone()), &hovered);
            let is_valid: bool = if is_hovered {
                command.call_function("is_valid", item_cmd.clone())?
            } else {
                false
            };
            let color = if is_hovered && is_valid { style_text.clone() } else { style_dim.clone() };
            let font: LuaValue = item.get("font").unwrap_or(LuaValue::Nil);
            let font = if matches!(font, LuaValue::Nil) { toolbar_font.clone() } else { font };
            let symbol: String = item.get("symbol")?;
            common.call_function::<()>("draw_text", (font, color, symbol, LuaValue::Nil, x, y, 0.0, h))?;
            Ok(true)
        })
    })?)?;

    // on_mouse_pressed(self, button, x, y, clicks)
    class.set("on_mouse_pressed", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, (this, button, x, y, clicks): (LuaTable, LuaValue, f64, f64, LuaValue)| {
            let visible: bool = this.get("visible").unwrap_or(false);
            if !visible { return Ok(LuaValue::Nil); }

            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_fn: LuaFunction = super_tbl.get("on_mouse_pressed")?;
            let caught: LuaValue = super_fn.call((this.clone(), button, x, y, clicks))?;
            if !matches!(caught, LuaValue::Nil | LuaValue::Boolean(false)) {
                return Ok(caught);
            }

            let core: LuaTable = require_table(lua, "core")?;
            let last_av: LuaValue = core.get("last_active_view")?;
            core.call_function::<()>("set_active_view", last_av)?;

            let hovered: LuaValue = this.get("hovered_item")?;
            if let LuaValue::Table(item) = hovered {
                let cmd: String = item.get("command")?;
                let command: LuaTable = require_table(lua, "core.command")?;
                let is_valid: bool = command.call_function("is_valid", cmd.clone())?;
                if is_valid {
                    command.call_function::<()>("perform", cmd)?;
                }
            }
            Ok(LuaValue::Boolean(true))
        })?
    })?;

    // on_mouse_left(self)
    class.set("on_mouse_left", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_fn: LuaFunction = super_tbl.get("on_mouse_left")?;
            super_fn.call::<()>(this.clone())?;
            let tooltip: bool = this.get("tooltip").unwrap_or(false);
            if tooltip {
                let core: LuaTable = require_table(lua, "core")?;
                let sv: LuaTable = core.get("status_view")?;
                sv.call_method::<()>("remove_tooltip", ())?;
                this.set("tooltip", false)?;
            }
            this.set("hovered_item", LuaValue::Nil)
        })?
    })?;

    // on_mouse_moved(self, px, py, ...)
    class.set("on_mouse_moved", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, (this, px, py, rest): (LuaTable, f64, f64, LuaMultiValue)| {
            let visible: bool = this.get("visible").unwrap_or(false);
            if !visible { return Ok(()); }

            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_fn: LuaFunction = super_tbl.get("on_mouse_moved")?;
            let mut call_args = vec![
                LuaValue::Table(this.clone()),
                LuaValue::Number(px),
                LuaValue::Number(py),
            ];
            for v in rest.into_iter() { call_args.push(v); }
            super_fn.call::<()>(LuaMultiValue::from_vec(call_args))?;

            this.set("hovered_item", LuaValue::Nil)?;

            let size: LuaTable = this.get("size")?;
            let size_x: f64 = size.get("x")?;
            let size_y: f64 = size.get("y")?;
            let mut x_min = size_x;
            let mut x_max = 0.0f64;
            let mut y_min = size_y;
            let mut y_max = 0.0f64;
            let mut found = false;

            let style: LuaTable = require_table(lua, "core.style")?;
            let command: LuaTable = require_table(lua, "core.command")?;
            let keymap: LuaTable = require_table(lua, "core.keymap")?;
            let core: LuaTable = require_table(lua, "core")?;
            let status_view: LuaTable = core.get("status_view")?;

            let iter_fn: LuaFunction = this.call_method("each_item", ())?;
            each_item_loop(&iter_fn, |item, x, y, w, h| {
                x_min = x_min.min(x);
                x_max = x_max.max(x + w);
                y_min = y;
                y_max = y + h;

                if !found && px > x && py > y && px <= x + w && py <= y + h {
                    this.set("hovered_item", item.clone())?;
                    let cmd: String = item.get("command")?;
                    let binding: LuaValue = keymap.call_function("get_binding", cmd.clone())?;
                    let name: String = command.call_function("prettify_name", cmd)?;
                    let style_dim: LuaValue = style.get("dim")?;
                    let tooltip_arg = if !matches!(binding, LuaValue::Nil | LuaValue::Boolean(false)) {
                        let t = lua.create_sequence_from([
                            LuaValue::String(lua.create_string(&name)?),
                            style_dim,
                            LuaValue::String(lua.create_string("  ")?),
                            binding,
                        ])?;
                        LuaValue::Table(t)
                    } else {
                        let t = lua.create_sequence_from([
                            LuaValue::String(lua.create_string(&name)?)
                        ])?;
                        LuaValue::Table(t)
                    };
                    status_view.call_method::<()>("show_tooltip", tooltip_arg)?;
                    this.set("tooltip", true)?;
                    found = true;
                }
                Ok(true)
            })?;

            if found { return Ok(()); }

            let tooltip: bool = this.get("tooltip").unwrap_or(false);
            if tooltip && !(px > x_min && px <= x_max && py > y_min && py <= y_max) {
                status_view.call_method::<()>("remove_tooltip", ())?;
                this.set("tooltip", false)?;
            }
            Ok(())
        })?
    })?;

    Ok(())
}

/// Registers `plugins.toolbarview` as a pure-Rust preload module.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;

    preload.set(
        "plugins.toolbarview",
        lua.create_function(|lua, ()| {
            let view_class: LuaTable = require_table(lua, "core.view")?;
            let toolbar_view = view_class.call_method::<LuaTable>("extend", ())?;
            toolbar_view.set(
                "__tostring",
                lua.create_function(|_lua, _self: LuaTable| Ok("ToolbarView"))?,
            )?;
            populate(lua, toolbar_view.clone())?;
            Ok(LuaValue::Table(toolbar_view))
        })?,
    )
}
