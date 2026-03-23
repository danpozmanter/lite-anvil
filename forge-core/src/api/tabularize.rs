use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Splits text by a delimiter pattern and aligns columns with padding.
fn tabularize_lines(lines: &mut [String], delim: &str) {
    let escaped = regex_escape_first_char(delim);
    let split_char = delim.chars().next().unwrap_or(' ');

    // Split each line into columns.
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(lines.len());
    let mut col_widths: Vec<usize> = Vec::new();

    for line in lines.iter() {
        let cols: Vec<String> = split_by_delim(line, split_char);
        for (j, col) in cols.iter().enumerate() {
            if j >= col_widths.len() {
                col_widths.push(col.len());
            } else if col.len() > col_widths[j] {
                col_widths[j] = col.len();
            }
        }
        rows.push(cols);
    }

    // Pad all columns except the last in each row.
    for row in &mut rows {
        let last = row.len().saturating_sub(1);
        for i in 0..last {
            let pad = col_widths[i].saturating_sub(row[i].len());
            row[i].extend(std::iter::repeat_n(' ', pad));
        }
    }

    // Rejoin rows with the delimiter.
    for (i, row) in rows.iter().enumerate() {
        lines[i] = row.join(delim);
    }

    drop(escaped);
}

fn regex_escape_first_char(delim: &str) -> String {
    let ch = delim.chars().next().unwrap_or(' ');
    if "^$()%.[]*+-?".contains(ch) {
        format!("%{ch}")
    } else {
        ch.to_string()
    }
}

fn split_by_delim(text: &str, delim: char) -> Vec<String> {
    text.split(delim).map(String::from).collect()
}

fn install(lua: &Lua) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;

    let tabularize_cmd = lua.create_function(|lua, dv: LuaTable| {
        let core = require_table(lua, "core")?;
        let command_view: LuaTable = core.get("command_view")?;
        let translate = require_table(lua, "core.doc.translate")?;
        let dv_key = lua.create_registry_value(dv)?;

        let start_of_line: LuaValue = translate.get("start_of_line")?;
        let end_of_line: LuaValue = translate.get("end_of_line")?;
        let sol_key = lua.create_registry_value(start_of_line)?;
        let eol_key = lua.create_registry_value(end_of_line)?;

        let opts = lua.create_table()?;
        opts.set(
            "submit",
            lua.create_function(move |lua, delim: String| {
                let delim = if delim.is_empty() {
                    " ".to_owned()
                } else {
                    delim
                };
                let dv: LuaTable = lua.registry_value(&dv_key)?;
                let doc: LuaTable = dv.get("doc")?;

                let sel: LuaMultiValue = doc.call_method("get_selection", true)?;
                let vals: Vec<&LuaValue> = sel.iter().collect();
                let to_i64 = |v: &LuaValue| -> i64 {
                    match v {
                        LuaValue::Integer(n) => *n,
                        LuaValue::Number(n) => *n as i64,
                        _ => 1,
                    }
                };
                let line1 = to_i64(vals.first().unwrap_or(&&LuaValue::Integer(1)));
                let col1 = to_i64(vals.get(1).unwrap_or(&&LuaValue::Integer(1)));
                let line2 = to_i64(vals.get(2).unwrap_or(&&LuaValue::Integer(1)));
                let col2 = to_i64(vals.get(3).unwrap_or(&&LuaValue::Integer(1)));
                let swap = vals
                    .get(4)
                    .and_then(|v| match v {
                        LuaValue::Boolean(b) => Some(*b),
                        _ => None,
                    })
                    .unwrap_or(false);

                let sol: LuaValue = lua.registry_value(&sol_key)?;
                let eol: LuaValue = lua.registry_value(&eol_key)?;

                let pos1: LuaMultiValue =
                    doc.call_method("position_offset", (line1, col1, sol))?;
                let l1 = to_i64(pos1.front().unwrap_or(&LuaValue::Integer(1)));
                let c1 = to_i64(pos1.iter().nth(1).unwrap_or(&LuaValue::Integer(1)));

                let pos2: LuaMultiValue =
                    doc.call_method("position_offset", (line2, col2, eol))?;
                let l2 = to_i64(pos2.front().unwrap_or(&LuaValue::Integer(1)));
                let c2 = to_i64(pos2.iter().nth(1).unwrap_or(&LuaValue::Integer(1)));

                doc.call_method::<()>("set_selection", (l1, c1, l2, c2, swap))?;

                let replace_fn = lua.create_function(move |_lua, text: String| {
                    let mut lines: Vec<String> = Vec::new();
                    let mut rest = text.as_str();
                    while let Some(pos) = rest.find('\n') {
                        lines.push(rest[..=pos].to_owned());
                        rest = &rest[pos + 1..];
                    }
                    if !rest.is_empty() {
                        lines.push(rest.to_owned());
                    }
                    tabularize_lines(&mut lines, &delim);
                    Ok(lines.concat())
                })?;
                doc.call_method::<()>("replace", replace_fn)?;
                Ok(())
            })?,
        )?;

        command_view.call_method::<()>("enter", ("Tabularize On Delimiter", opts))
    })?;

    let cmds = lua.create_table()?;
    cmds.set("tabularize:tabularize", tabularize_cmd)?;
    command.call_function::<()>("add", ("core.docview", cmds))?;

    Ok(())
}

/// Registers `plugins.tabularize`: column-alignment command via delimiter.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.tabularize",
        lua.create_function(|lua, ()| {
            install(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )
}
