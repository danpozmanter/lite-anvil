use mlua::prelude::*;

/// Registers `core.command` — the command registry with predicate-based dispatch.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "core.command",
        lua.create_function(|lua, ()| {
            let command = lua.create_table()?;
            let map = lua.create_table()?;
            command.set("map", map)?;

            command.set(
                "generate_predicate",
                lua.create_function(|lua, predicate: LuaValue| generate_predicate(lua, predicate))?,
            )?;

            // command.add(predicate, map)
            command.set(
                "add",
                lua.create_function({
                    let cmd_ref = lua.create_registry_value(command.clone())?;
                    move |lua, (predicate, fn_map): (LuaValue, LuaTable)| {
                        let cmd: LuaTable = lua.registry_value(&cmd_ref)?;
                        let gen_pred: LuaFunction = cmd.get("generate_predicate")?;
                        let predicate_fn: LuaFunction = gen_pred.call::<LuaFunction>(predicate)?;
                        let cmd_map: LuaTable = cmd.get("map")?;
                        for pair in fn_map.pairs::<String, LuaFunction>() {
                            let (name, func) = pair?;
                            let entry = lua.create_table()?;
                            entry.set("predicate", predicate_fn.clone())?;
                            entry.set("perform", func)?;
                            cmd_map.set(name, entry)?;
                        }
                        Ok(())
                    }
                })?,
            )?;

            command.set(
                "prettify_name",
                lua.create_function(|_lua, name: String| Ok(prettify_name(&name)))?,
            )?;

            // command.get_all_valid() -> string[]
            command.set(
                "get_all_valid",
                lua.create_function({
                    let cmd_ref = lua.create_registry_value(command.clone())?;
                    move |lua, ()| {
                        let cmd: LuaTable = lua.registry_value(&cmd_ref)?;
                        let cmd_map: LuaTable = cmd.get("map")?;
                        let res = lua.create_table()?;
                        let memoized = lua.create_table()?;
                        let mut idx = 1;

                        for pair in cmd_map.pairs::<String, LuaTable>() {
                            let (name, entry) = pair?;
                            let predicate: LuaFunction = entry.get("predicate")?;
                            let key = format!("{:p}", predicate.to_pointer());
                            let cached: LuaValue = memoized.get(key.clone())?;
                            let result = if cached == LuaValue::Nil {
                                let val: LuaValue = predicate.call(())?;
                                let truthy = is_truthy(&val);
                                memoized.set(key, truthy)?;
                                truthy
                            } else {
                                cached.as_boolean().unwrap_or(false)
                            };
                            if result {
                                res.set(idx, name)?;
                                idx += 1;
                            }
                        }
                        Ok(res)
                    }
                })?,
            )?;

            // command.is_valid(name, ...) -> boolean
            command.set(
                "is_valid",
                lua.create_function({
                    let cmd_ref = lua.create_registry_value(command.clone())?;
                    move |lua, args: LuaMultiValue| {
                        let cmd: LuaTable = lua.registry_value(&cmd_ref)?;
                        let cmd_map: LuaTable = cmd.get("map")?;
                        let mut args_iter = args.into_iter();
                        let name: String = match args_iter.next() {
                            Some(LuaValue::String(s)) => s.to_str()?.to_string(),
                            _ => return Ok(false),
                        };
                        let entry: LuaValue = cmd_map.get(name)?;
                        match entry {
                            LuaValue::Table(e) => {
                                let predicate: LuaFunction = e.get("predicate")?;
                                let rest = LuaMultiValue::from_iter(args_iter);
                                let result: LuaValue = predicate.call(rest)?;
                                Ok(is_truthy(&result))
                            }
                            _ => Ok(false),
                        }
                    }
                })?,
            )?;

            // command.perform(name, ...) -> boolean
            command.set(
                "perform",
                lua.create_function({
                    let cmd_ref = lua.create_registry_value(command.clone())?;
                    move |lua, args: LuaMultiValue| {
                        let cmd: LuaTable = lua.registry_value(&cmd_ref)?;
                        let cmd_map: LuaTable = cmd.get("map")?;
                        let core: LuaTable = lua
                            .globals()
                            .get::<LuaTable>("package")?
                            .get::<LuaTable>("loaded")?
                            .get("core")?;

                        let mut args_vec: Vec<LuaValue> = args.into_iter().collect();
                        let name: String = match args_vec.first() {
                            Some(LuaValue::String(s)) => s.to_str()?.to_string(),
                            _ => return Ok(true),
                        };
                        args_vec.remove(0);

                        let perform_inner = lua.create_function({
                            let cmd_map = cmd_map.clone();
                            let name = name.clone();
                            move |_lua, args: LuaMultiValue| {
                                let entry: LuaValue = cmd_map.get(name.clone())?;
                                let entry = match entry {
                                    LuaValue::Table(t) => t,
                                    _ => return Ok(LuaValue::Boolean(false)),
                                };
                                let predicate: LuaFunction = entry.get("predicate")?;
                                let perform_fn: LuaFunction = entry.get("perform")?;

                                let pred_result: LuaMultiValue = predicate.call(args.clone())?;
                                let mut pred_values: Vec<LuaValue> =
                                    pred_result.into_iter().collect();

                                let first = pred_values.first().cloned();
                                if !is_truthy(&first.unwrap_or(LuaValue::Nil)) {
                                    return Ok(LuaValue::Boolean(false));
                                }
                                pred_values.remove(0);

                                if !pred_values.is_empty() {
                                    let multi = LuaMultiValue::from_iter(pred_values);
                                    perform_fn.call::<()>(multi)?;
                                } else {
                                    perform_fn.call::<()>(args)?;
                                }

                                Ok(LuaValue::Boolean(true))
                            }
                        })?;

                        let core_try: LuaFunction = core.get("try")?;
                        let rest = LuaMultiValue::from_iter(args_vec);
                        let try_result: LuaMultiValue =
                            core_try.call(lua.pack_multi((perform_inner, rest))?)?;
                        let try_values: Vec<LuaValue> = try_result.into_iter().collect();

                        let ok = is_truthy(try_values.first().unwrap_or(&LuaValue::Nil));
                        let res = is_truthy(try_values.get(1).unwrap_or(&LuaValue::Nil));

                        Ok(!ok || res)
                    }
                })?,
            )?;

            // command.add_defaults()
            command.set(
                "add_defaults",
                lua.create_function(|lua, ()| {
                    let require: LuaFunction = lua.globals().get("require")?;
                    let modules = [
                        "core",
                        "root",
                        "command",
                        "doc",
                        "findreplace",
                        "files",
                        "dialog",
                        "log",
                        "statusbar",
                        "contextmenu",
                    ];
                    for name in &modules {
                        let module_path = format!("core.commands.{name}");
                        require.call::<LuaValue>(module_path)?;
                    }
                    Ok(())
                })?,
            )?;

            Ok(LuaValue::Table(command))
        })?,
    )
}

/// Converts a predicate value into a callable predicate function.
fn generate_predicate(lua: &Lua, predicate: LuaValue) -> LuaResult<LuaFunction> {
    match predicate {
        LuaValue::Nil => lua.create_function(|_lua, ()| Ok(true)),
        LuaValue::Function(f) => Ok(f),
        LuaValue::String(s) => {
            let pred_str = s.to_str()?.to_string();
            let strict = pred_str.ends_with('!');
            let module_name = if strict {
                pred_str.trim_end_matches('!').to_string()
            } else {
                pred_str
            };

            let require: LuaFunction = lua.globals().get("require")?;
            let class: LuaValue = require.call(module_name)?;

            make_class_predicate(lua, class, strict)
        }
        LuaValue::Table(t) => make_class_predicate(lua, LuaValue::Table(t), false),
        _ => lua.create_function(|_lua, ()| Ok(true)),
    }
}

/// Builds a predicate function that checks `core.active_view:extends(class)` or `:is(class)`.
fn make_class_predicate(lua: &Lua, class: LuaValue, strict: bool) -> LuaResult<LuaFunction> {
    let class_key = lua.create_registry_value(class)?;
    lua.create_function(move |lua, args: LuaMultiValue| {
        let core: LuaTable = lua
            .globals()
            .get::<LuaTable>("package")?
            .get::<LuaTable>("loaded")?
            .get("core")?;
        let active_view_val: LuaValue = core.get("active_view")?;
        let active_view = match active_view_val {
            LuaValue::Table(t) => t,
            _ => return Ok(LuaMultiValue::from_iter([LuaValue::Boolean(false)])),
        };
        let class: LuaValue = lua.registry_value(&class_key)?;

        let method_name = if strict { "is" } else { "extends" };
        let check_fn: LuaFunction = active_view.get(method_name)?;
        let matches: bool = check_fn.call((active_view.clone(), class))?;

        if matches {
            let mut result = vec![LuaValue::Boolean(true), LuaValue::Table(active_view)];
            for val in args {
                result.push(val);
            }
            Ok(LuaMultiValue::from_iter(result))
        } else {
            Ok(LuaMultiValue::from_iter([LuaValue::Boolean(false)]))
        }
    })
}

fn is_truthy(val: &LuaValue) -> bool {
    !matches!(val, LuaValue::Nil | LuaValue::Boolean(false))
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let mut result = String::with_capacity(s.len());
            for upper in c.to_uppercase() {
                result.push(upper);
            }
            result.push_str(chars.as_str());
            result
        }
    }
}

fn prettify_words(text: &str) -> String {
    text.replace('-', " ")
        .split_whitespace()
        .map(capitalize_first)
        .collect::<Vec<_>>()
        .join(" ")
}

fn prettify_search_command(name: &str) -> Option<String> {
    let colon_pos = name.find(':')?;
    let category = &name[..colon_pos];
    let action = &name[colon_pos + 1..];

    if category != "find-replace" && category != "project-search" {
        return None;
    }

    let (label, remainder) = if action.contains("swap") {
        let r = action.trim_start_matches("swap").trim_start_matches('-');
        ("Swap", r)
    } else if action.contains("replace") {
        let r = action.trim_start_matches("replace").trim_start_matches('-');
        ("Replace", r)
    } else if action.contains("find") {
        let r = action.trim_start_matches("find").trim_start_matches('-');
        ("Find", r)
    } else {
        ("Find", action)
    };

    let remainder = remainder.trim_start_matches('-');
    if remainder.is_empty() {
        Some(label.to_string())
    } else {
        Some(format!("{label}: {}", prettify_words(remainder)))
    }
}

fn prettify_name(name: &str) -> String {
    if let Some(special) = prettify_search_command(name) {
        return special;
    }
    name.replace(':', ": ")
        .replace('-', " ")
        .split_whitespace()
        .map(capitalize_first)
        .collect::<Vec<_>>()
        .join(" ")
}
