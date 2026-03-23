use mlua::prelude::*;

/// Minimal shim for `core.tokenizer`.
///
/// The native tokenizer handles all tokenization. Only `each_token` is needed
/// by `core.doc.highlighter` to iterate token arrays.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "core.tokenizer",
        lua.create_function(|lua, ()| {
            let tokenizer = lua.create_table()?;

            // each_token(t) -> iterator that yields (i, token_type, text) pairs
            tokenizer.set(
                "each_token",
                lua.create_function(|lua, t: LuaTable| {
                    let iter = lua.create_function(|_lua, (t, i): (LuaTable, i64)| {
                        let i = i + 2;
                        let token_type: LuaValue = t.raw_get(i)?;
                        if token_type == LuaValue::Nil {
                            return Ok(LuaMultiValue::new());
                        }
                        let text: LuaValue = t.raw_get(i + 1)?;
                        Ok(LuaMultiValue::from_vec(vec![
                            LuaValue::Integer(i),
                            token_type,
                            text,
                        ]))
                    })?;
                    Ok((iter, t, -1i64))
                })?,
            )?;

            Ok(LuaValue::Table(tokenizer))
        })?,
    )
}
