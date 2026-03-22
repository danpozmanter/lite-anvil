use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn get_f64(v: &LuaValue) -> f64 {
    match v {
        LuaValue::Number(n) => *n,
        LuaValue::Integer(n) => *n as f64,
        _ => 0.0,
    }
}

fn set_config_defaults(lua: &Lua) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let common = require_table(lua, "core.common")?;
    let style = require_table(lua, "core.style")?;

    let line_limit: LuaValue = config.get("line_limit")?;
    let selection: LuaValue = style.get("selection")?;
    let line_limit_str = match &line_limit {
        LuaValue::Integer(n) => n.to_string(),
        LuaValue::Number(n) => n.to_string(),
        _ => "80".to_owned(),
    };

    let defaults = lua.create_table()?;
    defaults.set("enabled", false)?;
    defaults.set("width", 2)?;

    let rulers = lua.create_table()?;
    rulers.push(line_limit.clone())?;
    defaults.set("rulers", rulers)?;
    defaults.set("use_custom_color", false)?;
    defaults.set("custom_color", selection.clone())?;

    // config_spec
    let spec = lua.create_table()?;
    spec.set("name", "Line Guide")?;

    let enabled_entry = lua.create_table()?;
    enabled_entry.set("label", "Enabled")?;
    enabled_entry.set("description", "Disable or enable drawing of the line guide.")?;
    enabled_entry.set("path", "enabled")?;
    enabled_entry.set("type", "toggle")?;
    enabled_entry.set("default", true)?;
    spec.push(enabled_entry)?;

    let width_entry = lua.create_table()?;
    width_entry.set("label", "Width")?;
    width_entry.set("description", "Width in pixels of the line guide.")?;
    width_entry.set("path", "width")?;
    width_entry.set("type", "number")?;
    width_entry.set("default", 2)?;
    width_entry.set("min", 1)?;
    spec.push(width_entry)?;

    let rulers_default = lua.create_sequence_from([line_limit_str.clone()])?;
    let rulers_entry = lua.create_table()?;
    rulers_entry.set("label", "Ruler Positions")?;
    rulers_entry.set(
        "description",
        "The different column numbers for the line guides to draw.",
    )?;
    rulers_entry.set("path", "rulers")?;
    rulers_entry.set("type", "list_strings")?;
    rulers_entry.set("default", rulers_default)?;

    let ll_for_get = line_limit_str.clone();
    rulers_entry.set(
        "get_value",
        lua.create_function(move |lua, rulers: LuaValue| match rulers {
            LuaValue::Table(t) => {
                let out = lua.create_table()?;
                let mut i = 1usize;
                for pair in t.sequence_values::<LuaValue>() {
                    let s = match pair? {
                        LuaValue::Integer(n) => n.to_string(),
                        LuaValue::Number(n) => n.to_string(),
                        LuaValue::String(s) => s.to_str()?.to_owned(),
                        _ => continue,
                    };
                    out.set(i, s)?;
                    i += 1;
                }
                Ok(LuaValue::Table(out))
            }
            _ => Ok(LuaValue::Table(
                lua.create_sequence_from([ll_for_get.clone()])?,
            )),
        })?,
    )?;

    let ll_for_set = line_limit.clone();
    rulers_entry.set(
        "set_value",
        lua.create_function(move |lua, rulers: LuaTable| {
            let out = lua.create_table()?;
            let mut i = 1usize;
            for pair in rulers.sequence_values::<LuaValue>() {
                let s = match pair? {
                    LuaValue::Integer(n) => n.to_string(),
                    LuaValue::Number(n) => n.to_string(),
                    LuaValue::String(s) => s.to_str()?.to_owned(),
                    _ => continue,
                };
                if let Ok(n) = s.parse::<f64>() {
                    out.set(i, n)?;
                    i += 1;
                }
            }
            if i == 1 {
                out.set(1usize, ll_for_set.clone())?;
            }
            Ok(out)
        })?,
    )?;
    spec.push(rulers_entry)?;

    let use_custom_entry = lua.create_table()?;
    use_custom_entry.set("label", "Use Custom Color")?;
    use_custom_entry.set("description", "Enable the utilization of a custom line color.")?;
    use_custom_entry.set("path", "use_custom_color")?;
    use_custom_entry.set("type", "toggle")?;
    use_custom_entry.set("default", false)?;
    spec.push(use_custom_entry)?;

    let custom_color_entry = lua.create_table()?;
    custom_color_entry.set("label", "Custom Color")?;
    custom_color_entry.set("description", "Applied when the above toggle is enabled.")?;
    custom_color_entry.set("path", "custom_color")?;
    custom_color_entry.set("type", "color")?;
    custom_color_entry.set("default", selection)?;
    spec.push(custom_color_entry)?;

    defaults.set("config_spec", spec)?;

    let merged: LuaTable =
        common.call_function("merge", (defaults, plugins.get::<LuaValue>("lineguide")?))?;
    plugins.set("lineguide", merged)?;
    Ok(())
}

fn patch_draw_overlay(lua: &Lua) -> LuaResult<()> {
    let doc_view = require_table(lua, "core.docview")?;
    let old: LuaFunction = doc_view.get("draw_overlay")?;
    let old_key = lua.create_registry_value(old)?;

    doc_view.set(
        "draw_overlay",
        lua.create_function(move |lua, (this, rest): (LuaTable, LuaMultiValue)| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let lg: LuaValue = plugins.get("lineguide")?;

            if let LuaValue::Table(ref conf) = lg {
                let enabled: bool = conf.get("enabled").unwrap_or(false);
                let dv_class = require_table(lua, "core.docview")?;
                let is_dv: bool = this.call_method("is", dv_class).unwrap_or(false);

                if enabled && is_dv {
                    let position: LuaTable = this.get("position")?;
                    let size: LuaTable = this.get("size")?;
                    let pos_y: f64 = position.get("y")?;
                    let size_y: f64 = size.get("y")?;

                    // get_line_screen_position(1) returns (x, y) — only x is needed.
                    let mvs: LuaMultiValue = this.call_method("get_line_screen_position", 1usize)?;
                    let line_x = get_f64(mvs.front().unwrap_or(&LuaValue::Nil));

                    let font: LuaValue = this.call_method("get_font", ())?;
                    let char_width: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_width", "n")?,
                        LuaValue::UserData(ud) => ud.call_method("get_width", "n")?,
                        _ => 0.0,
                    };

                    let ruler_width: f64 = conf.get::<f64>("width").unwrap_or(2.0);

                    let use_custom: bool = conf.get("use_custom_color").unwrap_or(false);
                    let ruler_color: LuaValue = if use_custom {
                        conf.get("custom_color")?
                    } else {
                        let style = require_table(lua, "core.style")?;
                        let guide: LuaValue = style.get("guide")?;
                        if !matches!(guide, LuaValue::Nil | LuaValue::Boolean(false)) {
                            guide
                        } else {
                            style.get("selection")?
                        }
                    };

                    let rulers: LuaTable = conf.get("rulers")?;
                    let renderer: LuaTable = lua.globals().get("renderer")?;

                    for pair in rulers.sequence_values::<LuaValue>() {
                        let v = pair?;
                        let (columns, row_color) = match &v {
                            LuaValue::Integer(n) => (*n as f64, None),
                            LuaValue::Number(n) => (*n, None),
                            LuaValue::Table(t) => {
                                let cols: f64 = t.get::<f64>("columns").unwrap_or(0.0);
                                let c: Option<LuaValue> = t.get("color")?;
                                (cols, c)
                            }
                            _ => continue,
                        };
                        let x = line_x + char_width * columns;
                        let color = row_color.unwrap_or_else(|| ruler_color.clone());
                        renderer.call_function::<()>(
                            "draw_rect",
                            (x, pos_y, ruler_width, size_y, color),
                        )?;
                    }
                }
            }

            // Chain to original.
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let mut args = LuaMultiValue::new();
            args.push_back(LuaValue::Table(this));
            args.extend(rest);
            old.call::<LuaMultiValue>(args)?;
            Ok(())
        })?,
    )?;
    Ok(())
}

fn register_commands(lua: &Lua) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;
    let cmds = lua.create_table()?;
    cmds.set(
        "lineguide:toggle",
        lua.create_function(|lua, ()| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let lg: LuaTable = plugins.get("lineguide")?;
            let enabled: bool = lg.get("enabled").unwrap_or(false);
            lg.set("enabled", !enabled)?;
            Ok(())
        })?,
    )?;
    command.call_function::<()>("add", (LuaValue::Nil, cmds))?;
    Ok(())
}

/// Registers `plugins.lineguide`: config defaults, vertical ruler drawing hook on DocView,
/// and `lineguide:toggle` command.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.lineguide",
        lua.create_function(|lua, ()| {
            set_config_defaults(lua)?;
            patch_draw_overlay(lua)?;
            register_commands(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )
}
