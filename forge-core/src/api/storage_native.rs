use mlua::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

fn user_dir(lua: &Lua) -> LuaResult<PathBuf> {
    let globals = lua.globals();
    let userdir: String = globals.get("USERDIR")?;
    Ok(PathBuf::from(userdir))
}

fn sanitize_key(key: &str) -> String {
    key.replace(['/', '\\'], "-")
}

fn module_dir(base: &Path, module: &str) -> PathBuf {
    base.join("storage").join(module)
}

fn key_path(base: &Path, module: &str, key: &str) -> PathBuf {
    module_dir(base, module).join(sanitize_key(key))
}

fn list_keys_impl(base: &Path, module: &str) -> Vec<String> {
    let dir = module_dir(base, module);
    let mut entries = Vec::new();
    let Ok(read_dir) = fs::read_dir(dir) else {
        return entries;
    };
    for entry in read_dir.flatten() {
        if let Ok(name) = entry.file_name().into_string() {
            entries.push(name);
        }
    }
    entries.sort();
    entries
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "load_text",
        lua.create_function(
            |lua, (store, key): (String, String)| -> LuaResult<LuaValue> {
                let base = user_dir(lua)?;
                let path = key_path(&base, &store, &key);
                match fs::read_to_string(path) {
                    Ok(text) => Ok(LuaValue::String(lua.create_string(text.as_bytes())?)),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(LuaValue::Nil),
                    Err(err) => Err(LuaError::RuntimeError(err.to_string())),
                }
            },
        )?,
    )?;

    module.set(
        "save_text",
        lua.create_function(
            |lua, (store, key, text): (String, String, String)| -> LuaResult<bool> {
                let base = user_dir(lua)?;
                let dir = module_dir(&base, &store);
                fs::create_dir_all(&dir).map_err(|e| LuaError::RuntimeError(e.to_string()))?;
                let path = key_path(&base, &store, &key);
                fs::write(path, text).map_err(|e| LuaError::RuntimeError(e.to_string()))?;
                Ok(true)
            },
        )?,
    )?;

    module.set(
        "keys",
        lua.create_function(|lua, store: String| -> LuaResult<LuaTable> {
            let base = user_dir(lua)?;
            let keys = list_keys_impl(&base, &store);
            let out = lua.create_table()?;
            for (idx, key) in keys.iter().enumerate() {
                out.set(idx + 1, key.as_str())?;
            }
            Ok(out)
        })?,
    )?;

    module.set(
        "clear",
        lua.create_function(
            |lua, (store, key): (String, Option<String>)| -> LuaResult<bool> {
                let base = user_dir(lua)?;
                let path = match key {
                    Some(key) => key_path(&base, &store, &key),
                    None => module_dir(&base, &store),
                };
                if !path.exists() {
                    return Ok(true);
                }
                if path.is_dir() {
                    fs::remove_dir_all(&path).map_err(|e| LuaError::RuntimeError(e.to_string()))?;
                } else {
                    fs::remove_file(&path).map_err(|e| LuaError::RuntimeError(e.to_string()))?;
                }
                Ok(true)
            },
        )?,
    )?;

    Ok(module)
}
