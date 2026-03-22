use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn escape_for_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for byte in text.bytes() {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            b'\x08' => out.push_str("\\b"),
            0x00..=0x1f | 0x7f => out.push_str(&format!("\\x{:02x}", byte)),
            _ => out.push(byte as char),
        }
    }
    out.push('"');
    out
}

/// Registers `plugins.quote`: adds the `quote:quote` command and `ctrl+'` keymap binding.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.quote",
        lua.create_function(|lua, ()| {
            let command = require_table(lua, "core.command")?;
            let doc_view = require_table(lua, "core.docview")?;

            let quote_fn = lua.create_function(|lua, dv: LuaTable| {
                let doc: LuaTable = dv.get("doc")?;
                let replace_fn = lua.create_function(|_lua, text: String| {
                    Ok(escape_for_quote(&text))
                })?;
                doc.call_method::<()>("replace", replace_fn)
            })?;

            let cmds = lua.create_table()?;
            cmds.set("quote:quote", quote_fn)?;
            command.call_function::<()>("add", (doc_view, cmds))?;

            let keymap = require_table(lua, "core.keymap")?;
            let bindings = lua.create_table()?;
            bindings.set("ctrl+'", "quote:quote")?;
            keymap.call_function::<()>("add", bindings)?;

            Ok(LuaValue::Boolean(true))
        })?,
    )
}
