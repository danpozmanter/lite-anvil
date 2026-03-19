use mlua::prelude::*;
use serde_json::{Map, Number, Value};
use std::fs;
use std::path::PathBuf;

fn user_dir(lua: &Lua) -> LuaResult<PathBuf> {
    let globals = lua.globals();
    let userdir: String = globals.get("USERDIR")?;
    Ok(PathBuf::from(userdir))
}

fn session_path(base: &std::path::Path) -> PathBuf {
    base.join("session.json")
}

fn legacy_session_path(base: &std::path::Path) -> PathBuf {
    base.join("session.lua")
}

fn to_json(value: LuaValue) -> LuaResult<Value> {
    Ok(match value {
        LuaValue::Nil => Value::Null,
        LuaValue::Boolean(v) => Value::Bool(v),
        LuaValue::Integer(v) => Value::Number(Number::from(v)),
        LuaValue::Number(v) => Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| LuaError::RuntimeError("cannot encode non-finite number".into()))?,
        LuaValue::String(v) => Value::String(v.to_str()?.to_string()),
        LuaValue::Table(table) => table_to_json(table)?,
        _ => {
            return Err(LuaError::RuntimeError(
                "unsupported session value type".to_string(),
            ));
        }
    })
}

fn table_to_json(table: LuaTable) -> LuaResult<Value> {
    let len = table.raw_len();
    let mut is_array = true;
    let mut object = Map::new();

    for pair in table.clone().pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair?;
        match key {
            LuaValue::Integer(index) if index >= 1 && index as usize <= len => {}
            LuaValue::Number(index)
                if index.fract() == 0.0 && index >= 1.0 && index as usize <= len => {}
            _ => {
                is_array = false;
            }
        }

        if !is_array {
            let key = match key {
                LuaValue::String(v) => v.to_str()?.to_string(),
                LuaValue::Integer(v) => v.to_string(),
                LuaValue::Number(v) => {
                    if v.fract() != 0.0 {
                        return Err(LuaError::RuntimeError(
                            "unsupported non-integer table key".to_string(),
                        ));
                    }
                    (v as i64).to_string()
                }
                _ => {
                    return Err(LuaError::RuntimeError(
                        "unsupported session table key".to_string(),
                    ));
                }
            };
            object.insert(key, to_json(value)?);
        }
    }

    if is_array {
        let mut array = Vec::with_capacity(len);
        for index in 1..=len {
            array.push(to_json(table.raw_get(index)?)?);
        }
        Ok(Value::Array(array))
    } else {
        Ok(Value::Object(object))
    }
}

fn to_lua(lua: &Lua, value: &Value) -> LuaResult<LuaValue> {
    Ok(match value {
        Value::Null => LuaValue::Nil,
        Value::Bool(v) => LuaValue::Boolean(*v),
        Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                LuaValue::Integer(i)
            } else {
                LuaValue::Number(
                    v.as_f64()
                        .ok_or_else(|| LuaError::RuntimeError("invalid json number".into()))?,
                )
            }
        }
        Value::String(v) => LuaValue::String(lua.create_string(v)?),
        Value::Array(values) => {
            let table = lua.create_table()?;
            for (index, value) in values.iter().enumerate() {
                table.raw_set(index + 1, to_lua(lua, value)?)?;
            }
            LuaValue::Table(table)
        }
        Value::Object(map) => {
            let table = lua.create_table()?;
            for (key, value) in map {
                table.set(key.as_str(), to_lua(lua, value)?)?;
            }
            LuaValue::Table(table)
        }
    })
}

fn update_recent(items: &mut Vec<String>, entry: &str, add: bool, limit: usize) {
    items.retain(|item| item != entry);
    if add {
        items.insert(0, entry.to_string());
        items.truncate(limit);
    }
}

fn sanitize_session_table(table: &LuaTable) -> LuaResult<()> {
    if let Some(window) = table.get::<Option<LuaTable>>("window")? {
        let width = window.get::<Option<LuaValue>>(1)?;
        let height = window.get::<Option<LuaValue>>(2)?;
        let valid = matches!(width, Some(LuaValue::Integer(_) | LuaValue::Number(_)))
            && matches!(height, Some(LuaValue::Integer(_) | LuaValue::Number(_)));
        if !valid {
            table.set("window", LuaValue::Nil)?;
        }
    }
    Ok(())
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "load",
        lua.create_function(|lua, ()| -> LuaResult<LuaTable> {
            let base = user_dir(lua)?;
            let path = session_path(&base);
            if let Ok(content) = fs::read_to_string(path) {
                let decoded: Value = serde_json::from_str(&content)
                    .map_err(|err| LuaError::RuntimeError(err.to_string()))?;
                return match to_lua(lua, &decoded)? {
                    LuaValue::Table(table) => {
                        sanitize_session_table(&table)?;
                        Ok(table)
                    }
                    _ => lua.create_table(),
                };
            }

            let session = lua.create_table()?;
            session.set(
                "legacy_path",
                legacy_session_path(&base).to_string_lossy().to_string(),
            )?;
            Ok(session)
        })?,
    )?;

    module.set(
        "save",
        lua.create_function(|lua, session: LuaTable| -> LuaResult<bool> {
            let base = user_dir(lua)?;
            fs::create_dir_all(&base).map_err(|err| LuaError::RuntimeError(err.to_string()))?;
            let json = table_to_json(session)?;
            let content = serde_json::to_string_pretty(&json)
                .map_err(|err| LuaError::RuntimeError(err.to_string()))?;
            fs::write(session_path(&base), content)
                .map_err(|err| LuaError::RuntimeError(err.to_string()))?;
            Ok(true)
        })?,
    )?;

    module.set(
        "update_recent_projects",
        lua.create_function(
            |lua, (projects, action, path): (LuaTable, String, String)| {
                let mut items = Vec::new();
                for value in projects.sequence_values::<String>() {
                    items.push(value?);
                }
                update_recent(&mut items, &path, action == "add", usize::MAX);
                let table = lua.create_table()?;
                for (index, value) in items.iter().enumerate() {
                    table.set(index + 1, value.as_str())?;
                }
                Ok(table)
            },
        )?,
    )?;

    module.set(
        "update_recent_files",
        lua.create_function(|lua, (files, path): (LuaTable, String)| {
            let mut items = Vec::new();
            for value in files.sequence_values::<String>() {
                items.push(value?);
            }
            update_recent(&mut items, &path, true, 100);
            let table = lua.create_table()?;
            for (index, value) in items.iter().enumerate() {
                table.set(index + 1, value.as_str())?;
            }
            Ok(table)
        })?,
    )?;

    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::{table_to_json, update_recent};
    use mlua::{Lua, LuaOptions, StdLib};

    #[test]
    fn session_json_round_trip_preserves_nested_data() {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).expect("lua");
        let table = lua.create_table().expect("table");
        table
            .set("active_project", "/tmp/project")
            .expect("active project");
        let open_files = lua.create_table().expect("open files");
        open_files.set(1, "/tmp/project/a.rs").expect("open file 1");
        open_files.set(2, "/tmp/project/b.rs").expect("open file 2");
        table.set("open_files", open_files).expect("open_files");
        let plugin_data = lua.create_table().expect("plugin_data");
        let workspace = lua.create_table().expect("workspace");
        workspace.set("expanded", true).expect("expanded");
        plugin_data
            .set("workspace", workspace)
            .expect("workspace set");
        table
            .set("plugin_data", plugin_data)
            .expect("plugin_data set");

        let json = table_to_json(table).expect("json");
        assert_eq!(json["active_project"], "/tmp/project");
        assert_eq!(json["open_files"][0], "/tmp/project/a.rs");
        assert_eq!(json["plugin_data"]["workspace"]["expanded"], true);
    }

    #[test]
    fn update_recent_deduplicates_and_caps() {
        let mut items = vec!["b".to_string(), "a".to_string(), "c".to_string()];
        update_recent(&mut items, "a", true, 3);
        assert_eq!(items, vec!["a", "b", "c"]);

        update_recent(&mut items, "b", false, 3);
        assert_eq!(items, vec!["a", "c"]);
    }
}
