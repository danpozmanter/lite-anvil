use std::sync::Arc;

use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn set_config_defaults(lua: &Lua) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let common = require_table(lua, "core.common")?;

    let defaults = lua.create_table()?;
    defaults.set("enabled", false)?;
    defaults.set("trim_empty_end_lines", false)?;

    let spec = lua.create_table()?;
    spec.set("name", "Trim Whitespace")?;

    let enabled_entry = lua.create_table()?;
    enabled_entry.set("label", "Enabled")?;
    enabled_entry.set(
        "description",
        "Disable or enable the trimming of white spaces by default.",
    )?;
    enabled_entry.set("path", "enabled")?;
    enabled_entry.set("type", "toggle")?;
    enabled_entry.set("default", false)?;
    spec.push(enabled_entry)?;

    let trim_end_entry = lua.create_table()?;
    trim_end_entry.set("label", "Trim Empty End Lines")?;
    trim_end_entry.set(
        "description",
        "Remove any empty new lines at the end of documents.",
    )?;
    trim_end_entry.set("path", "trim_empty_end_lines")?;
    trim_end_entry.set("type", "toggle")?;
    trim_end_entry.set("default", false)?;
    spec.push(trim_end_entry)?;

    defaults.set("config_spec", spec)?;

    let merged: LuaTable = common.call_function(
        "merge",
        (defaults, plugins.get::<LuaValue>("trimwhitespace")?),
    )?;
    plugins.set("trimwhitespace", merged)?;
    Ok(())
}

fn trim_doc(lua: &Lua, doc: &LuaTable) -> LuaResult<()> {
    let affordance = require_table(lua, "affordance_model")?;
    let trim_line: LuaFunction = affordance.get("trim_line")?;
    let math_huge = f64::INFINITY;

    let sel: LuaMultiValue = doc.call_method("get_selection", ())?;
    let cline: i64 = match sel.front() {
        Some(LuaValue::Integer(n)) => *n,
        Some(LuaValue::Number(n)) => *n as i64,
        _ => 1,
    };
    let ccol_val: Option<&LuaValue> = sel.iter().nth(1);
    let ccol: i64 = match ccol_val {
        Some(LuaValue::Integer(n)) => *n,
        Some(LuaValue::Number(n)) => *n as i64,
        _ => 1,
    };

    let lines: LuaTable = doc.get("lines")?;
    let num_lines = lines.len()?;

    for i in 1..=num_lines {
        let old_text: LuaString =
            doc.call_method("get_text", (i, 1, i, LuaValue::Number(math_huge)))?;
        let trim_col: LuaValue = if cline == i {
            LuaValue::Integer(ccol)
        } else {
            LuaValue::Nil
        };
        let new_text: LuaString = trim_line.call((old_text.clone(), trim_col))?;
        if old_text.as_bytes() != new_text.as_bytes() {
            let new_len = new_text.as_bytes().len() as i64 + 1;
            doc.call_method::<()>("insert", (i, 1, new_text))?;
            doc.call_method::<()>("remove", (i, new_len, i, LuaValue::Number(math_huge)))?;
        }
    }
    Ok(())
}

fn trim_empty_end_lines(lua: &Lua, doc: &LuaTable, raw_remove: bool) -> LuaResult<()> {
    let affordance = require_table(lua, "affordance_model")?;
    let count_fn: LuaFunction = affordance.get("count_empty_end_lines")?;
    let lines: LuaTable = doc.get("lines")?;
    let count: i64 = count_fn.call(lines)?;
    let math_huge = f64::INFINITY;

    for _ in 0..count {
        let doc_lines: LuaTable = doc.get("lines")?;
        let l = doc_lines.len()?;
        if l <= 1 {
            break;
        }
        let line_text: String = doc_lines.get(l)?;
        if line_text != "\n" {
            break;
        }
        let current_line: i64 = match doc.call_method::<LuaValue>("get_selection", ())? {
            LuaValue::Integer(n) => n,
            LuaValue::Number(n) => n as i64,
            _ => 0,
        };
        if current_line == l {
            doc.call_method::<()>(
                "set_selection",
                (
                    l - 1,
                    LuaValue::Number(math_huge),
                    l - 1,
                    LuaValue::Number(math_huge),
                ),
            )?;
        }
        if !raw_remove {
            doc.call_method::<()>(
                "remove",
                (l - 1, LuaValue::Number(math_huge), l, LuaValue::Number(math_huge)),
            )?;
        } else {
            let table_remove: LuaFunction = lua.globals().get::<LuaTable>("table")?.get("remove")?;
            table_remove.call::<()>((doc_lines, l))?;
        }
    }
    Ok(())
}

fn register_commands(lua: &Lua) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;

    let trim_cmd = lua.create_function(|lua, dv: LuaTable| {
        let doc: LuaTable = dv.get("doc")?;
        trim_doc(lua, &doc)
    })?;

    let trim_end_cmd = lua.create_function(|lua, dv: LuaTable| {
        let doc: LuaTable = dv.get("doc")?;
        trim_empty_end_lines(lua, &doc, false)
    })?;

    let cmds = lua.create_table()?;
    cmds.set("trim-whitespace:trim-trailing-whitespace", trim_cmd)?;
    cmds.set("trim-whitespace:trim-empty-end-lines", trim_end_cmd)?;
    command.call_function::<()>("add", ("core.docview", cmds))?;
    Ok(())
}

fn patch_doc_save(lua: &Lua) -> LuaResult<()> {
    let doc_class = require_table(lua, "core.doc")?;
    let old_save: LuaFunction = doc_class.get("save")?;
    let old_key = Arc::new(lua.create_registry_value(old_save)?);

    doc_class.set(
        "save",
        lua.create_function(move |lua, (this, args): (LuaTable, LuaMultiValue)| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let tw_cfg: LuaTable = plugins.get("trimwhitespace")?;
            let enabled: bool = tw_cfg.get("enabled").unwrap_or(false);
            let disabled: bool = this.get("disable_trim_whitespace").unwrap_or(false);

            if enabled && !disabled {
                trim_doc(lua, &this)?;
                let trim_end: bool = tw_cfg.get("trim_empty_end_lines").unwrap_or(false);
                if trim_end {
                    trim_empty_end_lines(lua, &this, false)?;
                }
            }

            let old: LuaFunction = lua.registry_value(&old_key)?;
            let mut call_args = LuaMultiValue::new();
            call_args.push_back(LuaValue::Table(this));
            call_args.extend(args);
            old.call::<LuaMultiValue>(call_args)
        })?,
    )?;
    Ok(())
}

/// Registers `plugins.trimwhitespace`: config, commands, and Doc.save hook.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.trimwhitespace",
        lua.create_function(|lua, ()| {
            set_config_defaults(lua)?;
            register_commands(lua)?;
            patch_doc_save(lua)?;

            let result = lua.create_table()?;
            result.set(
                "disable",
                lua.create_function(|_, doc: LuaTable| {
                    doc.set("disable_trim_whitespace", true)
                })?,
            )?;
            result.set(
                "enable",
                lua.create_function(|_, doc: LuaTable| {
                    doc.set("disable_trim_whitespace", LuaValue::Nil)
                })?,
            )?;
            result.set(
                "trim",
                lua.create_function(|lua, doc: LuaTable| trim_doc(lua, &doc))?,
            )?;
            result.set(
                "trim_empty_end_lines",
                lua.create_function(|lua, (doc, raw): (LuaTable, Option<bool>)| {
                    trim_empty_end_lines(lua, &doc, raw.unwrap_or(false))
                })?,
            )?;
            Ok(LuaValue::Table(result))
        })?,
    )
}
