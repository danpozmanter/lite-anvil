use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// One tick of font/color synchronization. Returns early so the caller can yield.
fn sync_tick(lua: &Lua, custom_fonts_key: &LuaRegistryKey) -> LuaResult<()> {
    let style = require_table(lua, "core.style")?;
    let custom_fonts: LuaTable = lua.registry_value(custom_fonts_key)?;
    let syntax_fonts: LuaTable = style.get("syntax_fonts")?;
    let syntax: LuaTable = style.get("syntax")?;

    let last_code_font: LuaValue = custom_fonts.get("__last_code_font")?;
    let code_font: LuaValue = style.get("code_font")?;
    let fonts_changed = last_code_font != code_font;

    if fonts_changed {
        custom_fonts.set("__last_code_font", code_font.clone())?;
        for attr in ["bold", "italic", "bold_italic"] {
            let entry: LuaTable = custom_fonts.get(attr)?;
            let cached_font: LuaValue = entry.get("font")?;
            let key = format!("markdown_{attr}");
            let current: LuaValue = syntax_fonts.get(key.as_str())?;
            if current == cached_font {
                set_font(lua, &style, &syntax_fonts, &entry, attr, &code_font)?;
            }
        }
    }

    let initial_color: LuaValue = custom_fonts.get("__initial_color")?;
    let keyword2: LuaValue = syntax.get("keyword2")?;
    let color_changed = initial_color != keyword2;

    if color_changed {
        custom_fonts.set("__initial_color", keyword2.clone())?;
        for attr in ["bold", "italic", "bold_italic"] {
            let entry: LuaTable = custom_fonts.get(attr)?;
            let cached_color: LuaValue = entry.get("color")?;
            let key = format!("markdown_{attr}");
            let current: LuaValue = syntax.get(key.as_str())?;
            if current == cached_color {
                syntax.set(key, keyword2.clone())?;
                entry.set("color", keyword2.clone())?;
            }
        }
    }

    Ok(())
}

fn set_font(
    _lua: &Lua,
    style: &LuaTable,
    syntax_fonts: &LuaTable,
    entry: &LuaTable,
    attr: &str,
    code_font: &LuaValue,
) -> LuaResult<()> {
    let attributes = match code_font {
        LuaValue::Table(t) => {
            let size: LuaValue = t.call_method("get_size", ())?;
            let attrs: LuaTable = match attr {
                "bold_italic" => {
                    let a = _lua.create_table()?;
                    a.set("bold", true)?;
                    a.set("italic", true)?;
                    a
                }
                _ => {
                    let a = _lua.create_table()?;
                    a.set(attr, true)?;
                    a
                }
            };
            let font: LuaValue = t.call_method("copy", (size, attrs))?;
            let key = format!("markdown_{attr}");
            syntax_fonts.set(key.as_str(), font.clone())?;
            entry.set("font", font)?;
            Ok(())
        }
        LuaValue::UserData(ud) => {
            let size: LuaValue = ud.call_method("get_size", ())?;
            let attrs: LuaTable = match attr {
                "bold_italic" => {
                    let a = _lua.create_table()?;
                    a.set("bold", true)?;
                    a.set("italic", true)?;
                    a
                }
                _ => {
                    let a = _lua.create_table()?;
                    a.set(attr, true)?;
                    a
                }
            };
            let font: LuaValue = ud.call_method("copy", (size, attrs))?;
            let key = format!("markdown_{attr}");
            syntax_fonts.set(key.as_str(), font.clone())?;
            entry.set("font", font)?;
            Ok(())
        }
        _ => Ok(()),
    };
    let _ = style;
    attributes
}

fn install(lua: &Lua) -> LuaResult<()> {
    let syntax = require_table(lua, "core.syntax")?;
    syntax.call_function::<()>("add_from_asset", "md")?;

    let style = require_table(lua, "core.style")?;
    let syntax_tbl: LuaTable = style.get("syntax")?;
    let syntax_fonts: LuaTable = style.get("syntax_fonts")?;
    let code_font: LuaValue = style.get("code_font")?;
    let keyword2: LuaValue = syntax_tbl.get("keyword2")?;

    // Build custom_fonts tracking table.
    let custom_fonts = lua.create_table()?;
    for attr in ["bold", "italic", "bold_italic"] {
        let entry = lua.create_table()?;
        entry.set("font", LuaValue::Nil)?;
        entry.set("color", LuaValue::Nil)?;
        custom_fonts.set(attr, entry)?;
    }
    custom_fonts.set("__last_code_font", LuaValue::Nil)?;
    custom_fonts.set("__initial_color", LuaValue::Nil)?;

    // Initial setup: set fonts and colors if not already set.
    for attr in ["bold", "italic", "bold_italic"] {
        let key = format!("markdown_{attr}");
        let existing_font: LuaValue = syntax_fonts.get(key.as_str())?;
        if matches!(existing_font, LuaValue::Nil) {
            let entry: LuaTable = custom_fonts.get(attr)?;
            set_font(lua, &style, &syntax_fonts, &entry, attr, &code_font)?;
        }
        let existing_color: LuaValue = syntax_tbl.get(key.as_str())?;
        if matches!(existing_color, LuaValue::Nil) {
            let entry: LuaTable = custom_fonts.get(attr)?;
            syntax_tbl.set(key.as_str(), keyword2.clone())?;
            entry.set("color", keyword2.clone())?;
        }
    }
    custom_fonts.set("__last_code_font", code_font)?;
    custom_fonts.set("__initial_color", keyword2)?;

    let cf_key = lua.create_registry_value(custom_fonts)?;

    // coroutine.yield cannot be called from Rust, so the loop+yield live in Lua.
    let tick = lua.create_function(move |lua, ()| sync_tick(lua, &cf_key))?;

    let thread_fn: LuaFunction = lua
        .load("local t = ...; return function() while true do t(); coroutine.yield(1) end end")
        .call::<LuaFunction>(tick)?;

    let core = require_table(lua, "core")?;
    core.get::<LuaFunction>("add_thread")?.call::<()>(thread_fn)?;

    Ok(())
}

/// Registers `plugins.language_md`: markdown syntax definition and emphasis font/color sync.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.language_md",
        lua.create_function(|lua, ()| {
            install(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )
}
