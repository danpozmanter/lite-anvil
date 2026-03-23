use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn current_mode(lua: &Lua) -> LuaResult<String> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let tt: LuaTable = plugins.get("theme_toggle")?;
    let mode: String = tt.get::<Option<String>>("mode")?.unwrap_or_default();
    Ok(if mode == "light" {
        "light".to_owned()
    } else {
        "dark".to_owned()
    })
}

fn theme_for_mode(mode: &str) -> &'static str {
    if mode == "light" {
        "light_default"
    } else {
        "dark_default"
    }
}

fn apply_mode(lua: &Lua, mode: &str) -> LuaResult<()> {
    let mode = if mode == "light" { "light" } else { "dark" };
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let tt: LuaTable = plugins.get("theme_toggle")?;
    tt.set("mode", mode)?;
    config.set("theme", theme_for_mode(mode))?;

    let style = require_table(lua, "core.style")?;
    style.call_function::<()>("apply_theme", ())?;

    let storage = require_table(lua, "core.storage")?;
    storage.call_function::<()>("save", ("theme_toggle", "mode", mode))?;

    let core = require_table(lua, "core")?;
    core.set("redraw", true)?;
    Ok(())
}

fn install(lua: &Lua) -> LuaResult<LuaTable> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let common = require_table(lua, "core.common")?;

    let defaults = lua.create_table()?;
    defaults.set("mode", "dark")?;
    let merged: LuaTable = common.call_function(
        "merge",
        (defaults, plugins.get::<LuaValue>("theme_toggle")?),
    )?;
    plugins.set("theme_toggle", merged)?;

    // Restore saved mode or reconcile with current theme.
    let storage = require_table(lua, "core.storage")?;
    let saved_mode: Option<String> = storage.call_function("load", ("theme_toggle", "mode"))?;

    let style = require_table(lua, "core.style")?;

    if let Some(ref saved) = saved_mode {
        if saved == "light" || saved == "dark" {
            let tt: LuaTable = plugins.get("theme_toggle")?;
            tt.set("mode", saved.as_str())?;
            config.set("theme", theme_for_mode(saved))?;
            style.call_function::<()>("apply_theme", ())?;
        }
    } else {
        let current_theme: Option<String> = config.get("theme")?;
        let light_theme = theme_for_mode("light");
        let dark_theme = theme_for_mode("dark");
        match current_theme.as_deref() {
            Some(t) if t == light_theme => {
                let tt: LuaTable = plugins.get("theme_toggle")?;
                tt.set("mode", "light")?;
            }
            Some(t) if t == dark_theme => {
                let tt: LuaTable = plugins.get("theme_toggle")?;
                tt.set("mode", "dark")?;
            }
            _ => {
                let mode = current_mode(lua)?;
                apply_mode(lua, &mode)?;
            }
        }
    }

    // Register toggle command.
    let command = require_table(lua, "core.command")?;
    let cmds = lua.create_table()?;
    cmds.set(
        "theme:toggle-mode",
        lua.create_function(|lua, ()| {
            let mode = current_mode(lua)?;
            let next = if mode == "dark" { "light" } else { "dark" };
            apply_mode(lua, next)
        })?,
    )?;
    command.call_function::<()>("add", (LuaValue::Nil, cmds))?;

    // Status bar item.
    let core = require_table(lua, "core")?;
    let status_view: LuaTable = core.get("status_view")?;
    let item = lua.create_table()?;
    item.set("name", "theme:mode")?;
    let right: LuaValue = status_view.get::<LuaTable>("Item")?.get("RIGHT")?;
    item.set("alignment", right)?;
    item.set("position", 1)?;
    item.set(
        "get_item",
        lua.create_function(|lua, ()| {
            let style = require_table(lua, "core.style")?;
            let mode = current_mode(lua)?;
            let glyph = if mode == "dark" { "o" } else { "*" };
            let color: LuaValue = if mode == "dark" {
                style.get("text")?
            } else {
                let warn: LuaValue = style.get("warn")?;
                if matches!(warn, LuaValue::Nil) {
                    style.get("text")?
                } else {
                    warn
                }
            };
            let result = lua.create_table()?;
            result.push(style.get::<LuaValue>("font")?)?;
            result.push(color)?;
            result.push(glyph)?;
            result.push(style.get::<LuaValue>("text")?)?;
            result.push(" ")?;
            Ok(result)
        })?,
    )?;
    item.set("command", "theme:toggle-mode")?;
    item.set("tooltip", "Toggle light and dark mode")?;
    let sep2: LuaValue = status_view.get("separator2")?;
    item.set("separator", sep2)?;
    status_view.call_method::<()>("add_item", item)?;

    let result = lua.create_table()?;
    result.set(
        "apply_mode",
        lua.create_function(|lua, mode: String| apply_mode(lua, &mode))?,
    )?;
    Ok(result)
}

/// Registers `plugins.theme_toggle`: dark/light theme toggle with persistence and status bar item.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.theme_toggle",
        lua.create_function(|lua, ()| install(lua).map(LuaValue::Table))?,
    )
}
