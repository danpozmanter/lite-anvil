use mlua::prelude::*;

use std::sync::Arc;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn set_config_defaults(lua: &Lua) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let common = require_table(lua, "core.common")?;

    let defaults = lua.create_table()?;
    defaults.set("mode", "code")?;
    defaults.set("default_scale", "autodetect")?;
    defaults.set("use_mousewheel", true)?;

    let merged: LuaTable =
        common.call_function("merge", (defaults, plugins.get::<LuaValue>("scale")?))?;
    plugins.set("scale", merged)?;
    Ok(())
}

fn build_set_scale(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<LuaFunction> {
    let sk = state_key;
    lua.create_function(move |lua, scale_arg: f64| {
        let common = require_table(lua, "core.common")?;
        let config = require_table(lua, "core.config")?;
        let style = require_table(lua, "core.style")?;
        let core = require_table(lua, "core")?;
        let storage = require_table(lua, "core.storage")?;

        let scale: f64 = common.call_function("clamp", (scale_arg, 0.2, 6.0))?;
        let state: LuaTable = lua.registry_value(&sk)?;
        let current_scale: f64 = state.get("current_scale")?;

        // Save scroll positions
        let v_scrolls = lua.create_table()?;
        let h_scrolls = lua.create_table()?;
        let root_view: LuaTable = core.get("root_view")?;
        let root_node: LuaTable = root_view.get("root_node")?;
        let children: LuaTable = root_node.call_method("get_children", ())?;
        for pair in children.sequence_values::<LuaTable>() {
            let view = pair?;
            let n: f64 = view.call_method("get_scrollable_size", ())?;
            let size: LuaTable = view.get("size")?;
            let size_y: f64 = size.get("y")?;
            let size_x: f64 = size.get("x")?;
            if n != f64::INFINITY && n > size_y {
                let scroll: LuaTable = view.get("scroll")?;
                let scroll_y: f64 = scroll.get("y")?;
                v_scrolls.set(view.clone(), scroll_y / (n - size_y))?;
            }
            let hn: f64 = view.call_method("get_h_scrollable_size", ())?;
            if hn != f64::INFINITY && hn > size_x {
                let scroll: LuaTable = view.get("scroll")?;
                let scroll_x: f64 = scroll.get("x")?;
                h_scrolls.set(view.clone(), scroll_x / (hn - size_x))?;
            }
        }

        let s = scale / current_scale;
        state.set("current_scale", scale)?;

        let plugins: LuaTable = config.get("plugins")?;
        let scale_cfg: LuaTable = plugins.get("scale")?;
        let mode: String = scale_cfg.get("mode")?;

        if mode == "ui" {
            lua.globals().set("SCALE", scale)?;
            let padding: LuaTable = style.get("padding")?;
            let px: f64 = padding.get("x")?;
            let py: f64 = padding.get("y")?;
            padding.set("x", px * s)?;
            padding.set("y", py * s)?;

            let ds: f64 = style.get("divider_size")?;
            style.set("divider_size", ds * s)?;
            let ss: f64 = style.get("scrollbar_size")?;
            style.set("scrollbar_size", ss * s)?;
            let ess: f64 = style.get("expanded_scrollbar_size")?;
            style.set("expanded_scrollbar_size", ess * s)?;
            let cw: f64 = style.get("caret_width")?;
            style.set("caret_width", cw * s)?;
            let tw: f64 = style.get("tab_width")?;
            style.set("tab_width", tw * s)?;
        }

        // Scale fonts
        let font_names = ["font", "icon_font", "code_font"];
        for name in &font_names {
            let font: LuaValue = style.get(*name)?;
            let font_size: f64 = match &font {
                LuaValue::Table(t) => t.call_method("get_size", ())?,
                LuaValue::UserData(ud) => ud.call_method("get_size", ())?,
                _ => continue,
            };
            match &font {
                LuaValue::Table(t) => t.call_method::<()>("set_size", s * font_size)?,
                LuaValue::UserData(ud) => ud.call_method::<()>("set_size", s * font_size)?,
                _ => {}
            }
        }
        // big_font and icon_big_font are getters
        let big_font: LuaValue = style.call_function("get_big_font", ())?;
        let big_font_size: f64 = match &big_font {
            LuaValue::Table(t) => t.call_method("get_size", ())?,
            LuaValue::UserData(ud) => ud.call_method("get_size", ())?,
            _ => 0.0,
        };
        if big_font_size > 0.0 {
            match &big_font {
                LuaValue::Table(t) => t.call_method::<()>("set_size", s * big_font_size)?,
                LuaValue::UserData(ud) => ud.call_method::<()>("set_size", s * big_font_size)?,
                _ => {}
            }
        }
        let icon_big_font: LuaValue = style.call_function("get_icon_big_font", ())?;
        let ibf_size: f64 = match &icon_big_font {
            LuaValue::Table(t) => t.call_method("get_size", ())?,
            LuaValue::UserData(ud) => ud.call_method("get_size", ())?,
            _ => 0.0,
        };
        if ibf_size > 0.0 {
            match &icon_big_font {
                LuaValue::Table(t) => t.call_method::<()>("set_size", s * ibf_size)?,
                LuaValue::UserData(ud) => ud.call_method::<()>("set_size", s * ibf_size)?,
                _ => {}
            }
        }

        // syntax_fonts
        let syntax_fonts: LuaValue = style.get("syntax_fonts")?;
        if let LuaValue::Table(ref sf) = syntax_fonts {
            for pair in sf.pairs::<LuaValue, LuaValue>() {
                let (_key, font) = pair?;
                let font_size: f64 = match &font {
                    LuaValue::Table(t) => t.call_method("get_size", ())?,
                    LuaValue::UserData(ud) => ud.call_method("get_size", ())?,
                    _ => continue,
                };
                match &font {
                    LuaValue::Table(t) => t.call_method::<()>("set_size", s * font_size)?,
                    LuaValue::UserData(ud) => ud.call_method::<()>("set_size", s * font_size)?,
                    _ => {}
                }
            }
        }

        // Restore scroll positions
        for pair in v_scrolls.pairs::<LuaTable, f64>() {
            let (view, n) = pair?;
            let scrollable: f64 = view.call_method("get_scrollable_size", ())?;
            let size: LuaTable = view.get("size")?;
            let size_y: f64 = size.get("y")?;
            let new_y = n * (scrollable - size_y);
            let scroll: LuaTable = view.get("scroll")?;
            scroll.set("y", new_y)?;
            let to: LuaTable = scroll.get("to")?;
            to.set("y", new_y)?;
        }
        for pair in h_scrolls.pairs::<LuaTable, f64>() {
            let (view, hn) = pair?;
            let scrollable: f64 = view.call_method("get_h_scrollable_size", ())?;
            let size: LuaTable = view.get("size")?;
            let size_x: f64 = size.get("x")?;
            let new_x = hn * (scrollable - size_x);
            let scroll: LuaTable = view.get("scroll")?;
            scroll.set("x", new_x)?;
            let to: LuaTable = scroll.get("to")?;
            to.set("x", new_x)?;
        }

        core.set("redraw", true)?;
        storage.call_function::<()>("save", ("scale", "scale", scale))?;
        Ok(())
    })
}

fn register_session_hooks(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let core = require_table(lua, "core")?;

    // restore_scale helper
    let sk = state_key.clone();
    let restore = lua.create_function(move |lua, data: LuaValue| {
        let value = match &data {
            LuaValue::Number(n) => Some(*n),
            LuaValue::Integer(n) => Some(*n as f64),
            LuaValue::Table(t) => {
                let s: Option<f64> = t.get("scale").ok();
                s
            }
            _ => None,
        };
        if let Some(v) = value {
            let state: LuaTable = lua.registry_value(&sk)?;
            let set_scale: LuaFunction = state.get("set_scale")?;
            set_scale.call::<()>(v)?;
            Ok(true)
        } else {
            Ok(false)
        }
    })?;
    let restore_key = Arc::new(lua.create_registry_value(restore)?);

    // Try session data first
    let sk2 = state_key.clone();
    let rk = Arc::clone(&restore_key);
    let session: LuaValue = core.get("session")?;
    let mut restored = false;
    if let LuaValue::Table(ref sess) = session {
        let plugin_data: LuaValue = sess.get("plugin_data")?;
        if let LuaValue::Table(ref pd) = plugin_data {
            let scale_data: LuaValue = pd.get("scale")?;
            if !matches!(scale_data, LuaValue::Nil) {
                let restore_fn: LuaFunction = lua.registry_value(&rk)?;
                let res: bool = restore_fn.call(scale_data)?;
                restored = res;
            }
        }
    }

    // Fall back to storage
    if !restored {
        let storage = require_table(lua, "core.storage")?;
        let saved: LuaValue = storage.call_function("load", ("scale", "scale"))?;
        if !matches!(saved, LuaValue::Nil) {
            let restore_fn: LuaFunction = lua.registry_value(&rk)?;
            let _: bool = restore_fn.call(saved)?;
        }
    }

    // Session load hook
    let rk2 = Arc::clone(&restore_key);
    core.call_function::<()>(
        "register_session_load_hook",
        (
            "scale",
            lua.create_function(move |lua, data: LuaValue| {
                let restore_fn: LuaFunction = lua.registry_value(&rk2)?;
                let _: bool = restore_fn.call(data)?;
                Ok(())
            })?,
        ),
    )?;

    // Session save hook
    let sk3 = sk2;
    core.call_function::<()>(
        "register_session_save_hook",
        (
            "scale",
            lua.create_function(move |lua, ()| {
                let state: LuaTable = lua.registry_value(&sk3)?;
                let current_scale: f64 = state.get("current_scale")?;
                let result = lua.create_table()?;
                result.set("scale", current_scale)?;
                Ok(result)
            })?,
        ),
    )?;

    Ok(())
}

fn build_config_spec(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let scale_cfg: LuaTable = plugins.get("scale")?;

    let spec = lua.create_table()?;
    spec.set("name", "Scale")?;

    // Mode entry
    let mode_entry = lua.create_table()?;
    mode_entry.set("label", "Mode")?;
    mode_entry.set("description", "The method used to apply the scaling.")?;
    mode_entry.set("path", "mode")?;
    mode_entry.set("type", "selection")?;
    mode_entry.set("default", "code")?;
    let mode_values = lua.create_table()?;
    let v1 = lua.create_sequence_from(["Everything", "ui"])?;
    let v2 = lua.create_sequence_from(["Code Only", "code"])?;
    mode_values.push(v1)?;
    mode_values.push(v2)?;
    mode_entry.set("values", mode_values)?;
    spec.push(mode_entry)?;

    // Default Scale entry
    let ds_entry = lua.create_table()?;
    ds_entry.set("label", "Default Scale")?;
    ds_entry.set("description", "The scaling factor applied to lite-anvil.")?;
    ds_entry.set("path", "default_scale")?;
    ds_entry.set("type", "selection")?;
    ds_entry.set("default", "autodetect")?;
    let ds_values = lua.create_table()?;
    let auto = lua.create_table()?;
    auto.push("Autodetect")?;
    auto.push("autodetect")?;
    ds_values.push(auto)?;
    let scale_options: &[(&str, f64)] = &[
        ("80%", 0.80),
        ("90%", 0.90),
        ("100%", 1.00),
        ("110%", 1.10),
        ("120%", 1.20),
        ("125%", 1.25),
        ("130%", 1.30),
        ("140%", 1.40),
        ("150%", 1.50),
        ("175%", 1.75),
        ("200%", 2.00),
        ("250%", 2.50),
        ("300%", 3.00),
    ];
    for (label, val) in scale_options {
        let entry = lua.create_table()?;
        entry.push(*label)?;
        entry.push(*val)?;
        ds_values.push(entry)?;
    }
    ds_entry.set("values", ds_values)?;

    let sk = state_key.clone();
    ds_entry.set(
        "on_apply",
        lua.create_function(move |lua, value: LuaValue| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let current_scale: f64 = state.get("current_scale")?;
            let default_scale: f64 = state.get("default_scale")?;
            let num_value = match &value {
                LuaValue::Number(n) => *n,
                LuaValue::Integer(n) => *n as f64,
                _ => default_scale,
            };
            if (num_value - current_scale).abs() > f64::EPSILON {
                let set_scale: LuaFunction = state.get("set_scale")?;
                set_scale.call::<()>(num_value)?;
            }
            Ok(())
        })?,
    )?;
    spec.push(ds_entry)?;

    // Use MouseWheel entry
    let mw_entry = lua.create_table()?;
    mw_entry.set("label", "Use MouseWheel")?;
    mw_entry.set(
        "description",
        "Allow using CTRL + MouseWheel for changing the scale.",
    )?;
    mw_entry.set("path", "use_mousewheel")?;
    mw_entry.set("type", "toggle")?;
    mw_entry.set("default", true)?;
    mw_entry.set(
        "on_apply",
        lua.create_function(|lua, enabled: bool| {
            let keymap = require_table(lua, "core.keymap")?;
            if enabled {
                let bindings = lua.create_table()?;
                bindings.set("ctrl+wheelup", "scale:increase")?;
                bindings.set("ctrl+wheeldown", "scale:decrease")?;
                keymap.call_function::<()>("add", bindings)?;
            } else {
                keymap.call_function::<()>("unbind", ("ctrl+wheelup", "scale:increase"))?;
                keymap.call_function::<()>("unbind", ("ctrl+wheeldown", "scale:decrease"))?;
            }
            Ok(())
        })?,
    )?;
    spec.push(mw_entry)?;

    scale_cfg.set("config_spec", spec)?;
    Ok(())
}

fn register_commands(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;
    let cmds = lua.create_table()?;

    let sk = state_key.clone();
    cmds.set(
        "scale:reset",
        lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let default_scale: f64 = state.get("default_scale")?;
            let set_scale: LuaFunction = state.get("set_scale")?;
            set_scale.call::<()>(default_scale)
        })?,
    )?;

    let sk = state_key.clone();
    cmds.set(
        "scale:decrease",
        lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let current_scale: f64 = state.get("current_scale")?;
            let set_scale: LuaFunction = state.get("set_scale")?;
            set_scale.call::<()>(current_scale - 0.05)
        })?,
    )?;

    let sk = state_key.clone();
    cmds.set(
        "scale:increase",
        lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let current_scale: f64 = state.get("current_scale")?;
            let set_scale: LuaFunction = state.get("set_scale")?;
            set_scale.call::<()>(current_scale + 0.05)
        })?,
    )?;

    command.call_function::<()>("add", (LuaValue::Nil, cmds))?;
    Ok(())
}

fn register_keymaps(lua: &Lua) -> LuaResult<()> {
    let keymap = require_table(lua, "core.keymap")?;

    let bindings = lua.create_table()?;
    bindings.set("ctrl+0", "scale:reset")?;
    bindings.set("ctrl+-", "scale:decrease")?;
    bindings.set("ctrl+=", "scale:increase")?;
    bindings.set("ctrl+shift+=", "scale:increase")?;
    bindings.set("ctrl+shift+/", "core:show-shortcuts-help")?;
    keymap.call_function::<()>("add", bindings)?;

    let platform: String = lua.globals().get("PLATFORM")?;
    if platform == "Mac OS X" {
        let mac_bindings = lua.create_table()?;
        mac_bindings.set("cmd+0", "scale:reset")?;
        mac_bindings.set("cmd+-", "scale:decrease")?;
        mac_bindings.set("cmd+=", "scale:increase")?;
        mac_bindings.set("cmd+shift+=", "scale:increase")?;
        mac_bindings.set("cmd+shift+/", "core:show-shortcuts-help")?;
        keymap.call_function::<()>("add", mac_bindings)?;
    }

    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let scale_cfg: LuaTable = plugins.get("scale")?;
    let use_mousewheel: bool = scale_cfg.get("use_mousewheel").unwrap_or(true);
    if use_mousewheel {
        let mw = lua.create_table()?;
        mw.set("ctrl+wheelup", "scale:increase")?;
        mw.set("ctrl+wheeldown", "scale:decrease")?;
        keymap.call_function::<()>("add", mw)?;
        if platform == "Mac OS X" {
            let mac_mw = lua.create_table()?;
            mac_mw.set("cmd+wheelup", "scale:increase")?;
            mac_mw.set("cmd+wheeldown", "scale:decrease")?;
            keymap.call_function::<()>("add", mac_mw)?;
        }
    }
    Ok(())
}

fn patch_docview_context_menu(lua: &Lua) -> LuaResult<()> {
    let doc_view = require_table(lua, "core.docview")?;
    let old: LuaFunction = doc_view.get("on_context_menu")?;
    let old_key = lua.create_registry_value(old)?;

    doc_view.set(
        "on_context_menu",
        lua.create_function(move |lua, this: LuaTable| -> LuaResult<LuaMultiValue> {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let args: LuaMultiValue = old.call(this)?;
            let mut args_vec: Vec<LuaValue> = args.into_vec();
            if let Some(LuaValue::Table(cmds)) = args_vec.first() {
                let items: LuaTable = cmds.get("items")?;
                // Shift items 4..6 to 7..9
                let table_move: LuaFunction =
                    lua.globals().get::<LuaTable>("table")?.get("move")?;
                table_move.call::<()>((items.clone(), 4, 6, 7))?;
                // Insert scale commands at positions 4, 5, 6
                let item4 = lua.create_table()?;
                item4.set("text", "Font +")?;
                item4.set("command", "scale:increase")?;
                items.set(4, item4)?;
                let item5 = lua.create_table()?;
                item5.set("text", "Font -")?;
                item5.set("command", "scale:decrease")?;
                items.set(5, item5)?;
                let item6 = lua.create_table()?;
                item6.set("text", "Font Reset")?;
                item6.set("command", "scale:reset")?;
                items.set(6, item6)?;
            }
            let table_unpack: LuaFunction =
                lua.globals().get::<LuaTable>("table")?.get("unpack")?;
            // Re-pack into a sequence table for unpack
            let seq = lua.create_table()?;
            for (i, v) in args_vec.drain(..).enumerate() {
                seq.set(i + 1, v)?;
            }
            table_unpack.call(seq)
        })?,
    )?;
    Ok(())
}

/// Registers `plugins.scale`: font/UI scaling with keyboard shortcuts, mouse wheel support,
/// session persistence, and DocView context menu integration.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.scale",
        lua.create_function(|lua, ()| {
            set_config_defaults(lua)?;

            let initial_scale: f64 = lua.globals().get("SCALE")?;

            // Shared state table
            let state = lua.create_table()?;
            state.set("current_scale", initial_scale)?;
            state.set("default_scale", initial_scale)?;
            let state_key = Arc::new(lua.create_registry_value(state.clone())?);

            let set_scale = build_set_scale(lua, state_key.clone())?;
            state.set("set_scale", set_scale.clone())?;

            // Apply non-autodetect default if configured
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let scale_cfg: LuaTable = plugins.get("scale")?;
            let default_scale_cfg: LuaValue = scale_cfg.get("default_scale")?;
            if let LuaValue::Number(n) = default_scale_cfg {
                if (n - initial_scale).abs() > f64::EPSILON {
                    set_scale.call::<()>(n)?;
                }
            } else if let LuaValue::Integer(n) = default_scale_cfg {
                let n = n as f64;
                if (n - initial_scale).abs() > f64::EPSILON {
                    set_scale.call::<()>(n)?;
                }
            }

            register_session_hooks(lua, state_key.clone())?;
            build_config_spec(lua, state_key.clone())?;
            register_commands(lua, state_key.clone())?;
            register_keymaps(lua)?;
            patch_docview_context_menu(lua)?;

            // Return module table
            let sk = state_key.clone();
            let module = lua.create_table()?;
            module.set("set", {
                let state_key = state_key.clone();
                lua.create_function(move |lua, scale: f64| {
                    let state: LuaTable = lua.registry_value(&state_key)?;
                    let f: LuaFunction = state.get("set_scale")?;
                    f.call::<()>(scale)
                })?
            })?;
            module.set("get", {
                let state_key = state_key.clone();
                lua.create_function(move |lua, ()| {
                    let state: LuaTable = lua.registry_value(&state_key)?;
                    let v: f64 = state.get("current_scale")?;
                    Ok(v)
                })?
            })?;
            module.set("increase", {
                let state_key = state_key.clone();
                lua.create_function(move |lua, ()| {
                    let state: LuaTable = lua.registry_value(&state_key)?;
                    let current: f64 = state.get("current_scale")?;
                    let f: LuaFunction = state.get("set_scale")?;
                    f.call::<()>(current + 0.05)
                })?
            })?;
            module.set("decrease", {
                let state_key = state_key.clone();
                lua.create_function(move |lua, ()| {
                    let state: LuaTable = lua.registry_value(&state_key)?;
                    let current: f64 = state.get("current_scale")?;
                    let f: LuaFunction = state.get("set_scale")?;
                    f.call::<()>(current - 0.05)
                })?
            })?;
            module.set("reset", {
                lua.create_function(move |lua, ()| {
                    let state: LuaTable = lua.registry_value(&sk)?;
                    let default: f64 = state.get("default_scale")?;
                    let f: LuaFunction = state.get("set_scale")?;
                    f.call::<()>(default)
                })?
            })?;

            // Persist scale to storage on set_scale.
            // (The set_scale function already exists; patch it to also save.)
            {
                let sk = state_key.clone();
                let save_scale = lua.create_function(move |lua, ()| {
                    let state: LuaTable = lua.registry_value(&sk)?;
                    let current: f64 = state.get("current_scale")?;
                    let storage = require_table(lua, "core.storage")?;
                    let save: LuaFunction = storage.get("save")?;
                    save.call::<()>(("scale", "scale", current))?;
                    Ok(())
                })?;
                state.set("save_scale", save_scale)?;
            }

            // Restore scale from session or storage on startup.
            {
                let storage = require_table(lua, "core.storage")?;
                let load_fn: LuaFunction = storage.get("load")?;
                let saved: LuaValue = load_fn.call(("scale", "scale"))?;
                let scale_val = match saved {
                    LuaValue::Number(n) => Some(n),
                    LuaValue::Integer(n) => Some(n as f64),
                    _ => None,
                };
                if let Some(s) = scale_val {
                    let set_scale: LuaFunction = state.get("set_scale")?;
                    let _ = set_scale.call::<()>(s);
                }
            }

            // Register session hooks for persistence.
            {
                let sk = state_key.clone();
                let core = require_table(lua, "core")?;
                let reg_save: LuaFunction = core.get("register_session_save_hook")?;
                let save_hook = lua.create_function(move |lua, ()| {
                    let state: LuaTable = lua.registry_value(&sk)?;
                    let current: f64 = state.get("current_scale")?;
                    let storage = require_table(lua, "core.storage")?;
                    let save: LuaFunction = storage.get("save")?;
                    save.call::<()>(("scale", "scale", current))?;
                    Ok(current)
                })?;
                reg_save.call::<()>(("scale", save_hook))?;

                let sk2 = state_key.clone();
                let reg_load: LuaFunction = core.get("register_session_load_hook")?;
                let load_hook = lua.create_function(move |lua, (data, _primary): (LuaValue, LuaValue)| {
                    if let LuaValue::Number(s) = data {
                        let state: LuaTable = lua.registry_value(&sk2)?;
                        let set_scale: LuaFunction = state.get("set_scale")?;
                        let _ = set_scale.call::<()>(s);
                    }
                    Ok(())
                })?;
                reg_load.call::<()>(("scale", load_hook))?;
            }

            // Register commands.
            let command = require_table(lua, "core.command")?;
            let add_fn: LuaFunction = command.get("add")?;
            let cmds = lua.create_table()?;
            cmds.set("scale:reset", module.get::<LuaFunction>("reset")?)?;
            cmds.set("scale:decrease", module.get::<LuaFunction>("decrease")?)?;
            cmds.set("scale:increase", module.get::<LuaFunction>("increase")?)?;
            add_fn.call::<()>((LuaValue::Nil, cmds))?;

            // Register keybindings.
            let keymap = require_table(lua, "core.keymap")?;
            let km_add: LuaFunction = keymap.get("add")?;
            let bindings = lua.create_table()?;
            bindings.set("ctrl+0", "scale:reset")?;
            bindings.set("ctrl+-", "scale:decrease")?;
            bindings.set("ctrl+=", "scale:increase")?;
            bindings.set("ctrl+shift+=", "scale:increase")?;
            km_add.call::<()>(bindings)?;

            let platform: String = lua.globals().get("PLATFORM")?;
            if platform == "Mac OS X" {
                let mac_bindings = lua.create_table()?;
                mac_bindings.set("cmd+0", "scale:reset")?;
                mac_bindings.set("cmd+-", "scale:decrease")?;
                mac_bindings.set("cmd+=", "scale:increase")?;
                mac_bindings.set("cmd+shift+=", "scale:increase")?;
                km_add.call::<()>(mac_bindings)?;
            }

            // Mousewheel bindings.
            let use_mousewheel: bool = config
                .get::<LuaTable>("plugins")?
                .get::<LuaTable>("scale")?
                .get::<Option<bool>>("use_mousewheel")?
                .unwrap_or(true);
            if use_mousewheel {
                let mw = lua.create_table()?;
                mw.set("ctrl+wheelup", "scale:increase")?;
                mw.set("ctrl+wheeldown", "scale:decrease")?;
                km_add.call::<()>(mw)?;
                if platform == "Mac OS X" {
                    let mw_mac = lua.create_table()?;
                    mw_mac.set("cmd+wheelup", "scale:increase")?;
                    mw_mac.set("cmd+wheeldown", "scale:decrease")?;
                    km_add.call::<()>(mw_mac)?;
                }
            }

            // Patch DocView context menu.
            {
                let docview: LuaTable = require_table(lua, "core.docview")?;
                let old_ctx: LuaValue = docview.get("on_context_menu")?;
                if let LuaValue::Function(old_fn) = old_ctx {
                    let old_key = Arc::new(lua.create_registry_value(old_fn)?);
                    docview.set("on_context_menu", lua.create_function(move |lua, this: LuaTable| {
                        let old: LuaFunction = lua.registry_value(&old_key)?;
                        let result: LuaMultiValue = old.call(this)?;
                        let vals: Vec<LuaValue> = result.into_vec();
                        if let Some(LuaValue::Table(cmds_tbl)) = vals.first() {
                            let items: LuaTable = cmds_tbl.get("items")?;
                            let len = items.raw_len();
                            // Shift items 4..6 to 7..9 and insert scale items
                            for i in (4..=len.min(6)).rev() {
                                let v: LuaValue = items.raw_get(i)?;
                                items.raw_set(i + 3, v)?;
                            }
                            let e1 = lua.create_table()?;
                            e1.set("text", "Font +")?;
                            e1.set("command", "scale:increase")?;
                            items.raw_set(4, e1)?;
                            let e2 = lua.create_table()?;
                            e2.set("text", "Font -")?;
                            e2.set("command", "scale:decrease")?;
                            items.raw_set(5, e2)?;
                            let e3 = lua.create_table()?;
                            e3.set("text", "Font Reset")?;
                            e3.set("command", "scale:reset")?;
                            items.raw_set(6, e3)?;
                        }
                        Ok(LuaMultiValue::from_vec(vals))
                    })?)?;
                }
            }

            Ok(LuaValue::Table(module))
        })?,
    )
}
