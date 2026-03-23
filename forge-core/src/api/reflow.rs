use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Word-wraps text to the given column limit.
fn wordwrap_text(text: &str, limit: usize) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut n: usize = 0;

    for word in text.split_whitespace() {
        if n + word.len() > limit && !parts.is_empty() {
            parts.push("\n");
            n = 0;
        } else if !parts.is_empty() {
            parts.push(" ");
        }
        parts.push(word);
        n = n + word.len() + 1;
    }

    parts.concat()
}

fn install(lua: &Lua) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;
    let keymap = require_table(lua, "core.keymap")?;

    let reflow_cmd = lua.create_function(|lua, dv: LuaTable| {
        let doc: LuaTable = dv.get("doc")?;
        let config = require_table(lua, "core.config")?;

        let replace_fn = lua.create_function(move |lua, text: String| {
            let config = require_table(lua, "core.config")?;
            let prefix_set = "[^%w\n%[%](){}`'\"]*";

            // Use Lua pattern matching via string.match for compatibility.
            let string_tbl: LuaTable = lua.globals().get("string")?;
            let match_fn: LuaFunction = string_tbl.get("match")?;
            let gsub_fn: LuaFunction = string_tbl.get("gsub")?;

            // Get prefix1: text:match("^\n*" .. prefix_set)
            let ptn1 = format!("^\n*{prefix_set}");
            let prefix1: String =
                match_fn.call::<Option<String>>((text.as_str(), ptn1))?.unwrap_or_default();

            // Get prefix2: text:match("\n(" .. prefix_set .. ")", #prefix1+1)
            let ptn2 = format!("\n({prefix_set})");
            let prefix2: Option<String> =
                match_fn.call((text.as_str(), ptn2, prefix1.len() + 1))?;
            let prefix2 = match prefix2 {
                Some(ref s) if !s.is_empty() => s.clone(),
                _ => prefix1.clone(),
            };

            // Get trailing whitespace.
            let trailing: String =
                match_fn.call::<Option<String>>((text.as_str(), "%s*$"))?.unwrap_or_default();

            // Strip all line prefixes and trailing whitespace.
            let body_start = prefix1.len();
            let body_end = text.len().saturating_sub(trailing.len());
            let body = if body_start < body_end {
                &text[body_start..body_end]
            } else {
                ""
            };
            let strip_ptn = format!("\n{prefix_set}");
            let stripped: String = gsub_fn.call((body, strip_ptn.as_str(), "\n"))?;

            // Split into blocks on double newlines, wordwrap, and join.
            let line_limit_val: LuaValue = config.get("line_limit")?;
            let line_limit = match line_limit_val {
                LuaValue::Integer(n) => n as usize,
                LuaValue::Number(n) => n as usize,
                _ => 80,
            };
            let effective_limit = line_limit.saturating_sub(prefix1.len());

            // Replace \n\n with NUL, split on NUL, wordwrap each block.
            let with_nul = stripped.replace("\n\n", "\0");
            let blocks: Vec<String> = with_nul
                .split('\0')
                .filter(|b| !b.is_empty())
                .map(|block| wordwrap_text(block, effective_limit))
                .collect();
            let joined = blocks.join("\n\n");

            // Re-add prefixes.
            let with_prefix = joined.replace('\n', &format!("\n{prefix2}"));
            let result = format!("{prefix1}{with_prefix}{trailing}");
            Ok(result)
        })?;

        doc.call_method::<()>("replace", replace_fn)?;
        drop(config);
        Ok(())
    })?;

    let cmds = lua.create_table()?;
    cmds.set("reflow:reflow", reflow_cmd)?;
    command.call_function::<()>("add", ("core.docview", cmds))?;

    let bindings = lua.create_table()?;
    bindings.set("ctrl+shift+q", "reflow:reflow")?;
    keymap.call_function::<()>("add_direct", bindings)?;

    Ok(())
}

/// Registers `plugins.reflow`: paragraph word-wrapping command with keybinding.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.reflow",
        lua.create_function(|lua, ()| {
            install(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )
}
