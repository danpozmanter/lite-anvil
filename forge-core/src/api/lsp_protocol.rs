use mlua::prelude::*;
use serde_json::Value;
use std::collections::HashMap;

fn lua_to_json(value: LuaValue) -> LuaResult<Value> {
    Ok(match value {
        LuaValue::Nil => Value::Null,
        LuaValue::Boolean(v) => Value::Bool(v),
        LuaValue::Integer(v) => Value::Number(v.into()),
        LuaValue::Number(v) => serde_json::Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        LuaValue::String(v) => Value::String(v.to_str()?.to_string()),
        LuaValue::Table(table) => {
            let mut max_idx = 0i64;
            let mut count = 0i64;
            let mut array_like = true;
            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, _) = pair?;
                match key {
                    LuaValue::Integer(idx) if idx >= 1 => {
                        max_idx = max_idx.max(idx);
                        count += 1;
                    }
                    _ => {
                        array_like = false;
                        break;
                    }
                }
            }
            if array_like && count == max_idx {
                let mut out = Vec::new();
                for idx in 1..=max_idx {
                    out.push(lua_to_json(table.raw_get(idx)?)?);
                }
                Value::Array(out)
            } else {
                let mut out = serde_json::Map::new();
                for pair in table.pairs::<String, LuaValue>() {
                    let (key, value) = pair?;
                    out.insert(key, lua_to_json(value)?);
                }
                Value::Object(out)
            }
        }
        _ => Value::Null,
    })
}

fn json_to_lua(lua: &Lua, value: &Value) -> LuaResult<LuaValue> {
    Ok(match value {
        Value::Null => LuaValue::Nil,
        Value::Bool(v) => LuaValue::Boolean(*v),
        Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                LuaValue::Integer(i)
            } else {
                LuaValue::Number(v.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(v) => LuaValue::String(lua.create_string(v)?),
        Value::Array(items) => {
            let table = lua.create_table()?;
            for (idx, item) in items.iter().enumerate() {
                table.raw_set((idx + 1) as i64, json_to_lua(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
        Value::Object(map) => {
            let table = lua.create_table()?;
            for (key, item) in map {
                table.set(key.as_str(), json_to_lua(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
    })
}

fn decode_messages(buffer: &str) -> LuaResult<(Vec<Value>, String)> {
    let mut messages = Vec::new();
    let mut remaining = buffer.to_string();
    loop {
        let Some(header_end) = remaining.find("\r\n\r\n") else {
            break;
        };
        let header = &remaining[..header_end];
        let Some(content_length) = header.lines().find_map(|line| {
            line.split_once(':').and_then(|(k, v)| {
                if k.eq_ignore_ascii_case("Content-Length") {
                    v.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
        }) else {
            return Err(LuaError::RuntimeError(
                "invalid LSP message without Content-Length".to_string(),
            ));
        };
        let body_start = header_end + 4;
        let body_end = body_start + content_length;
        if remaining.len() < body_end {
            break;
        }
        let decoded: Value = serde_json::from_str(&remaining[body_start..body_end])
            .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
        messages.push(decoded);
        remaining = remaining[body_end..].to_string();
    }
    Ok((messages, remaining))
}

fn completion_kinds_table(lua: &Lua) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    let kinds: HashMap<i64, &str> = HashMap::from([
        (1, "keyword2"),
        (2, "function"),
        (3, "function"),
        (4, "keyword2"),
        (5, "keyword2"),
        (6, "keyword2"),
        (7, "keyword2"),
        (8, "keyword2"),
        (9, "keyword2"),
        (10, "keyword2"),
        (11, "literal"),
        (12, "function"),
        (13, "keyword"),
        (14, "keyword"),
        (15, "string"),
        (16, "keyword"),
        (17, "file"),
        (18, "keyword"),
        (19, "keyword"),
        (20, "keyword2"),
        (21, "literal"),
        (22, "keyword2"),
        (23, "operator"),
        (24, "keyword"),
        (25, "keyword"),
    ]);
    for (key, value) in kinds {
        table.set(key, value)?;
    }
    Ok(table)
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set("completion_kinds", completion_kinds_table(lua)?)?;
    module.set(
        "encode_message",
        lua.create_function(|_, message: LuaValue| {
            let json = serde_json::to_string(&lua_to_json(message)?)
                .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
            Ok(format!("Content-Length: {}\r\n\r\n{}", json.len(), json))
        })?,
    )?;
    module.set(
        "decode_messages",
        lua.create_function(|lua, buffer: String| {
            let (messages, remaining) = decode_messages(&buffer)?;
            let out = lua.create_table()?;
            for (idx, message) in messages.iter().enumerate() {
                out.raw_set((idx + 1) as i64, json_to_lua(lua, message)?)?;
            }
            Ok((out, remaining))
        })?,
    )?;
    module.set(
        "json_encode",
        lua.create_function(|_, value: LuaValue| {
            serde_json::to_string(&lua_to_json(value)?)
                .map_err(|e| LuaError::RuntimeError(e.to_string()))
        })?,
    )?;
    module.set(
        "json_decode",
        lua.create_function(|lua, text: String| {
            let decoded: Value =
                serde_json::from_str(&text).map_err(|e| LuaError::RuntimeError(e.to_string()))?;
            json_to_lua(lua, &decoded)
        })?,
    )?;
    Ok(module)
}
