use mlua::prelude::*;

/// Loads a JSON theme file from DATADIR/assets/themes/<name>.json, parses
/// the palette into a Lua table, and registers it with core.style.
fn load_json_theme(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let datadir: String = lua.globals().get("DATADIR")?;
    let pathsep: String = lua.globals().get("PATHSEP")?;
    let path = format!(
        "{}{}assets{}themes{}{}.json",
        datadir, pathsep, pathsep, pathsep, name
    );
    let content = std::fs::read_to_string(&path)
        .map_err(|e| LuaError::runtime(format!("cannot read theme {path}: {e}")))?;
    let json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| LuaError::runtime(format!("invalid JSON in {path}: {e}")))?;
    let palette = json
        .get("palette")
        .ok_or_else(|| LuaError::runtime(format!("theme {name} missing 'palette'")))?;
    json_object_to_table(lua, palette)
}

/// Recursively converts a serde_json::Value object into a Lua table.
fn json_object_to_table(lua: &Lua, val: &serde_json::Value) -> LuaResult<LuaTable> {
    let tbl = lua.create_table()?;
    if let serde_json::Value::Object(map) = val {
        for (k, v) in map {
            match v {
                serde_json::Value::String(s) => tbl.set(k.as_str(), s.as_str())?,
                serde_json::Value::Object(_) => {
                    tbl.set(k.as_str(), json_object_to_table(lua, v)?)?;
                }
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        tbl.set(k.as_str(), i)?;
                    } else if let Some(f) = n.as_f64() {
                        tbl.set(k.as_str(), f)?;
                    }
                }
                serde_json::Value::Bool(b) => tbl.set(k.as_str(), *b)?,
                _ => {}
            }
        }
    }
    Ok(tbl)
}

fn require_style(lua: &Lua) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call("core.style")
}

/// Registers all built-in color theme preloaders into `package.preload`.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;

    // `default` — registers and applies config immediately.
    preload.set(
        "colors.default",
        lua.create_function(|lua, ()| {
            let table = load_json_theme(lua, "default")?;
            let style = require_style(lua)?;
            let register: LuaFunction = style.get("register_theme")?;
            register.call::<()>(("default", table))?;
            let apply: LuaFunction = style.get("apply_config")?;
            apply.call::<LuaValue>(())
        })?,
    )?;

    // `dark_default` and `light_default` — register only, don't apply or set config.theme.
    for name in ["dark_default", "light_default"] {
        let n = name.to_string();
        preload.set(
            format!("colors.{name}"),
            lua.create_function(move |lua, ()| {
                let table = load_json_theme(lua, &n)?;
                let style = require_style(lua)?;
                let register: LuaFunction = style.get("register_theme")?;
                register.call::<()>((n.as_str(), table))?;
                Ok(LuaValue::Table(style))
            })?,
        )?;
    }

    // `fall`, `summer`, `textadept` — register and set config.theme.
    for name in ["fall", "summer", "textadept"] {
        let n = name.to_string();
        preload.set(
            format!("colors.{name}"),
            lua.create_function(move |lua, ()| {
                let table = load_json_theme(lua, &n)?;
                let style = require_style(lua)?;
                let register: LuaFunction = style.get("register_theme")?;
                register.call::<()>((n.as_str(), table))?;
                let require: LuaFunction = lua.globals().get("require")?;
                let config: LuaTable = require.call("core.config")?;
                config.set("theme", n.as_str())?;
                Ok(LuaValue::Table(style))
            })?,
        )?;
    }

    Ok(())
}
