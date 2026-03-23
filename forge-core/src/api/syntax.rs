use mlua::prelude::*;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Registers `core.syntax` as a pure Rust preload, replacing `data/core/syntax.lua`.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "core.syntax",
        lua.create_function(|lua, ()| {
            let common: LuaTable = require_table(lua, "core.common")?;
            let core: LuaTable = require_table(lua, "core")?;
            let native_tokenizer: LuaTable = require_table(lua, "native_tokenizer")?;

            let syntax = lua.create_table()?;
            let items = lua.create_table()?;
            let lazy_items = lua.create_table()?;
            let lazy_loaded = lua.create_table()?;
            let loaded_assets = lua.create_table()?;

            syntax.set("items", items)?;
            syntax.set("lazy_items", lazy_items)?;
            syntax.set("lazy_loaded", lazy_loaded)?;
            syntax.set("loaded_assets", loaded_assets)?;

            let plain = lua.create_table()?;
            plain.set("name", "Plain Text")?;
            plain.set("patterns", lua.create_table()?)?;
            plain.set("symbols", lua.create_table()?)?;
            syntax.set("plain_text_syntax", plain.clone())?;

            // Register plain text with native tokenizer if available
            let register_fn: LuaValue = native_tokenizer.get("register_syntax")?;
            if let LuaValue::Function(f) = register_fn {
                let _ = lua
                    .globals()
                    .get::<LuaFunction>("pcall")?
                    .call::<LuaMultiValue>((f, "Plain Text", plain));
            }

            // check_pattern(pattern_type, pattern) -> ok, err
            let check_pattern = lua.create_function(|lua, (pattern_type, pattern): (String, LuaString)| {
                let pcall: LuaFunction = lua.globals().get("pcall")?;
                if pattern_type == "regex" {
                    let regex: LuaTable = lua.globals().get("regex")?;
                    let compile: LuaFunction = regex.get("compile")?;
                    let result: LuaMultiValue = pcall.call((compile, pattern.clone()))?;
                    let vals: Vec<LuaValue> = result.into_vec();
                    let ok = matches!(vals.first(), Some(LuaValue::Boolean(true)));
                    if !ok {
                        let err = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                        return Ok((LuaValue::Boolean(false), err));
                    }
                    let compiled = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                    let find_offsets: LuaFunction = regex.get("find_offsets")?;
                    let result2: LuaMultiValue = pcall.call((find_offsets, compiled, ""))?;
                    let vals2: Vec<LuaValue> = result2.into_vec();
                    let pcall_ok = matches!(vals2.first(), Some(LuaValue::Boolean(true)));
                    if pcall_ok {
                        let mstart = vals2.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let mend = vals2.get(2).cloned().unwrap_or(LuaValue::Nil);
                        if let (LuaValue::Integer(s), LuaValue::Integer(e)) = (&mstart, &mend) {
                            if s > e {
                                return Ok((
                                    LuaValue::Boolean(false),
                                    LuaValue::String(lua.create_string("Regex matches an empty string")?),
                                ));
                            }
                        }
                    }
                    Ok((LuaValue::Boolean(true), LuaValue::Nil))
                } else {
                    let string_tbl: LuaTable = lua.globals().get("string")?;
                    let ufind: LuaFunction = string_tbl.get("ufind")?;
                    let result: LuaMultiValue = pcall.call((ufind, "", pattern))?;
                    let vals: Vec<LuaValue> = result.into_vec();
                    let ok = matches!(vals.first(), Some(LuaValue::Boolean(true)));
                    if !ok {
                        let err = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                        return Ok((LuaValue::Boolean(false), err));
                    }
                    let mstart = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                    let mend = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                    if let (LuaValue::Integer(s), LuaValue::Integer(e)) = (&mstart, &mend) {
                        if s > e {
                            return Ok((
                                LuaValue::Boolean(false),
                                LuaValue::String(lua.create_string("Pattern matches an empty string")?),
                            ));
                        }
                    }
                    Ok((LuaValue::Boolean(true), LuaValue::Nil))
                }
            })?;
            let check_pattern_key = lua.create_registry_value(check_pattern)?;

            // syntax.add(t)
            let syntax_ref = lua.create_registry_value(syntax.clone())?;
            let core_ref = lua.create_registry_value(core.clone())?;
            let nt_ref = lua.create_registry_value(native_tokenizer.clone())?;
            syntax.set(
                "add",
                lua.create_function(move |lua, t: LuaTable| {
                    let syntax: LuaTable = lua.registry_value(&syntax_ref)?;
                    let core: LuaTable = lua.registry_value(&core_ref)?;
                    let native_tokenizer: LuaTable = lua.registry_value(&nt_ref)?;
                    let check_pattern: LuaFunction =
                        lua.registry_value(&check_pattern_key)?;

                    // Default space_handling to true
                    let space_handling: LuaValue = t.get("space_handling")?;
                    let space_handling = match space_handling {
                        LuaValue::Boolean(b) => b,
                        _ => {
                            t.set("space_handling", true)?;
                            true
                        }
                    };

                    let patterns: LuaValue = t.get("patterns")?;
                    if let LuaValue::Table(ref patterns_tbl) = patterns {
                        let t_name: String = t.get::<String>("name").unwrap_or_default();
                        let len = patterns_tbl.raw_len();
                        for i in 1..=len {
                            let pattern: LuaTable = patterns_tbl.raw_get(i)?;
                            let p_pat: LuaValue = pattern.get("pattern")?;
                            let p_regex: LuaValue = pattern.get("regex")?;
                            let is_pattern = !matches!(p_pat, LuaValue::Nil);
                            let p = if is_pattern { p_pat } else { p_regex };
                            let pattern_type = if is_pattern { "pattern" } else { "regex" };

                            match p {
                                LuaValue::Table(ref p_tbl) => {
                                    for j in 1..=2i64 {
                                        let pj: LuaValue = p_tbl.raw_get(j)?;
                                        let result: LuaMultiValue = check_pattern.call((pattern_type, pj.clone()))?;
                                        let vals: Vec<LuaValue> = result.into_vec();
                                        let ok = matches!(vals.first(), Some(LuaValue::Boolean(true)));
                                        if !ok {
                                            let pj_str = match pj {
                                                LuaValue::String(s) => s.to_str()?.to_string(),
                                                _ => String::new(),
                                            };
                                            let err_name = format!("#{i}:{j} <{pj_str}>");
                                            let err_val = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                                            let warn: LuaFunction = core.get("warn")?;
                                            warn.call::<()>((
                                                "Malformed pattern %s in %s language plugin: %s",
                                                err_name.as_str(),
                                                t_name.as_str(),
                                                err_val,
                                            ))?;
                                            pattern.set("disabled", true)?;
                                        }
                                    }
                                }
                                LuaValue::String(ref s) => {
                                    let result: LuaMultiValue = check_pattern.call((pattern_type, s.clone()))?;
                                    let vals: Vec<LuaValue> = result.into_vec();
                                    let ok = matches!(vals.first(), Some(LuaValue::Boolean(true)));
                                    if !ok {
                                        let s_str = s.to_str()?.to_string();
                                        let err_name = format!("#{i} <{s_str}>");
                                        let err_val = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                                        let warn: LuaFunction = core.get("warn")?;
                                        warn.call::<()>((
                                            "Malformed pattern %s in %s language plugin: %s",
                                            err_name.as_str(),
                                            t_name.as_str(),
                                            err_val,
                                        ))?;
                                        pattern.set("disabled", true)?;
                                    }
                                }
                                _ => {
                                    let err_name = format!("#{i}");
                                    let warn: LuaFunction = core.get("warn")?;
                                    warn.call::<()>((
                                        "Malformed pattern %s in %s language plugin: %s",
                                        err_name.as_str(),
                                        t_name.as_str(),
                                        "Missing pattern or regex",
                                    ))?;
                                    pattern.set("disabled", true)?;
                                }
                            }
                        }

                        let table_insert: LuaFunction = lua.globals().get::<LuaTable>("table")?.get("insert")?;

                        if space_handling {
                            let ws_pat = lua.create_table()?;
                            ws_pat.set("pattern", "%s+")?;
                            ws_pat.set("type", "normal")?;
                            table_insert.call::<()>((patterns_tbl.clone(), ws_pat))?;
                        }

                        let word_pat = lua.create_table()?;
                        word_pat.set("pattern", "%w+%f[%s]")?;
                        word_pat.set("type", "normal")?;
                        table_insert.call::<()>((patterns_tbl.clone(), word_pat))?;
                    }

                    let items: LuaTable = syntax.get("items")?;
                    let table_insert: LuaFunction = lua.globals().get::<LuaTable>("table")?.get("insert")?;
                    table_insert.call::<()>((items, t.clone()))?;

                    // Register with native tokenizer
                    let available: LuaValue = native_tokenizer.get("available")?;
                    let is_available = match available {
                        LuaValue::Boolean(b) => b,
                        LuaValue::Function(ref f) => {
                            let r: bool = f.call(())?;
                            r
                        }
                        _ => false,
                    };
                    let name: LuaValue = t.get("name")?;
                    if is_available && !matches!(name, LuaValue::Nil) {
                        let pcall: LuaFunction = lua.globals().get("pcall")?;
                        let register: LuaFunction = native_tokenizer.get("register_syntax")?;
                        let result: LuaMultiValue = pcall.call((register, name.clone(), t.clone()))?;
                        let vals: Vec<LuaValue> = result.into_vec();
                        let ok = matches!(vals.first(), Some(LuaValue::Boolean(true)));
                        if !ok {
                            let err = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                            let warn: LuaFunction = core.get("warn")?;
                            warn.call::<()>((
                                "Failed to register %s with native tokenizer: %s",
                                name,
                                err,
                            ))?;
                        }
                    }

                    Ok(())
                })?,
            )?;

            // find(str, field) — local helper
            let find_fn = {
                let syntax_ref2 = lua.create_registry_value(syntax.clone())?;
                let common_ref = lua.create_registry_value(common.clone())?;
                lua.create_function(move |lua, (s, field): (String, String)| -> LuaResult<LuaValue> {
                    let syntax: LuaTable = lua.registry_value(&syntax_ref2)?;
                    let common: LuaTable = lua.registry_value(&common_ref)?;
                    let items: LuaTable = syntax.get("items")?;
                    let match_pattern: LuaFunction = common.get("match_pattern")?;
                    let mut best_match: i64 = 0;
                    let mut best_syntax: LuaValue = LuaValue::Nil;

                    let len = items.raw_len();
                    for i in (1..=len).rev() {
                        let t: LuaTable = items.raw_get(i)?;
                        let field_val: LuaValue = t.get(field.as_str())?;
                        let patterns = match field_val {
                            LuaValue::Table(tbl) => tbl,
                            _ => lua.create_table()?,
                        };
                        let result: LuaMultiValue = match_pattern.call((s.as_str(), patterns))?;
                        let vals: Vec<LuaValue> = result.into_vec();
                        if let Some(LuaValue::Integer(s_val)) = vals.first() {
                            if let Some(LuaValue::Integer(e_val)) = vals.get(1) {
                                let span = e_val - s_val;
                                if span > best_match {
                                    best_match = span;
                                    best_syntax = LuaValue::Table(t);
                                }
                            }
                        }
                    }
                    Ok(best_syntax)
                })?
            };
            let find_key = lua.create_registry_value(find_fn)?;

            // extract_match_list(source, field) — local helper
            let extract_match_list_fn = lua.create_function(|lua, (source, field): (LuaString, String)| {
                let list = lua.create_table()?;
                let string_tbl: LuaTable = lua.globals().get("string")?;
                let smatch: LuaFunction = string_tbl.get("match")?;
                let pattern = format!("{field}%s*=%s*%b{{}}");
                let block: LuaValue = smatch.call((source, pattern))?;
                if let LuaValue::String(block_str) = block {
                    let gmatch: LuaFunction = string_tbl.get("gmatch")?;
                    let iter: LuaFunction = gmatch.call((block_str, "(['\"])(.-)%1"))?;
                    loop {
                        let result: LuaMultiValue = iter.call(())?;
                        let vals: Vec<LuaValue> = result.into_vec();
                        if vals.is_empty() || matches!(vals.first(), Some(LuaValue::Nil)) {
                            break;
                        }
                        if let Some(text) = vals.get(1) {
                            let list_len = list.raw_len();
                            list.raw_set(list_len + 1, text.clone())?;
                        }
                    }
                }
                Ok(list)
            })?;
            let extract_key = lua.create_registry_value(extract_match_list_fn)?;

            // syntax.register_lazy_plugin(plugin)
            let syntax_ref3 = lua.create_registry_value(syntax.clone())?;
            let extract_key2 = lua.create_registry_value(lua.registry_value::<LuaFunction>(&extract_key)?)?;
            syntax.set(
                "register_lazy_plugin",
                lua.create_function(move |lua, plugin: LuaTable| {
                    let syntax: LuaTable = lua.registry_value(&syntax_ref3)?;
                    let extract_match_list: LuaFunction = lua.registry_value(&extract_key2)?;

                    let pkg_loaded: LuaTable = lua.globals().get::<LuaTable>("package")?.get("loaded")?;
                    let json: LuaValue = pkg_loaded.get("plugins.lsp.json")?;

                    let mut files = lua.create_table()?;
                    let mut headers = lua.create_table()?;

                    let plugin_file: String = plugin.get("file")?;
                    let metadata_path = plugin_file.replace(".lua", ".lazy.json");

                    let io_tbl: LuaTable = lua.globals().get("io")?;
                    let io_open: LuaFunction = io_tbl.get("open")?;
                    let mfile: LuaValue = io_open.call((metadata_path, "r"))?;
                    let mut metadata = LuaValue::Nil;

                    if let LuaValue::UserData(ref f) = mfile {
                        let read_result: LuaValue = f.call_method("read", "*a")?;
                        metadata = read_result;
                        let _: () = f.call_method("close", ())?;
                    }

                    if let (LuaValue::String(_), LuaValue::Table(json_tbl)) = (&metadata, &json) {
                        let decode_safe: LuaValue = json_tbl.get("decode_safe")?;
                        if let LuaValue::Function(decode_fn) = decode_safe {
                            let result: LuaMultiValue = decode_fn.call(metadata.clone())?;
                            let vals: Vec<LuaValue> = result.into_vec();
                            let ok = matches!(vals.first(), Some(LuaValue::Boolean(true)));
                            if ok {
                                if let Some(LuaValue::Table(decoded)) = vals.get(1) {
                                    let f: LuaValue = decoded.get("files")?;
                                    let h: LuaValue = decoded.get("headers")?;
                                    if let LuaValue::Table(f_tbl) = f {
                                        files = f_tbl;
                                    }
                                    if let LuaValue::Table(h_tbl) = h {
                                        headers = h_tbl;
                                    }
                                }
                            }
                        }
                    }

                    if files.raw_len() == 0 && headers.raw_len() == 0 {
                        let src_file: LuaValue = io_open.call((plugin_file, "r"))?;
                        if let LuaValue::UserData(ref f) = src_file {
                            let source: LuaValue = f.call_method("read", "*a")?;
                            let _: () = f.call_method("close", ())?;
                            if let LuaValue::String(s) = source {
                                files = extract_match_list.call((s.clone(), "files"))?;
                                headers = extract_match_list.call((s, "headers"))?;
                            }
                        } else {
                            return Ok(());
                        }
                    }

                    let lazy_items: LuaTable = syntax.get("lazy_items")?;
                    let entry = lua.create_table()?;
                    let name: LuaValue = plugin.get("name")?;
                    entry.set("name", name)?;
                    entry.set("plugin", plugin.clone())?;
                    let load_fn: LuaValue = plugin.get("load")?;
                    entry.set("load", load_fn)?;
                    entry.set("files", files)?;
                    entry.set("headers", headers)?;
                    let idx = lazy_items.raw_len() + 1;
                    lazy_items.raw_set(idx, entry)?;

                    Ok(())
                })?,
            )?;

            // syntax.get(filename, header)
            let syntax_ref4 = lua.create_registry_value(syntax.clone())?;
            let find_key2 = lua.create_registry_value(lua.registry_value::<LuaFunction>(&find_key)?)?;
            let core_ref2 = lua.create_registry_value(core.clone())?;
            let common_ref2 = lua.create_registry_value(common.clone())?;
            syntax.set(
                "get",
                lua.create_function(move |lua, (filename, header): (LuaValue, LuaValue)| {
                    let syntax: LuaTable = lua.registry_value(&syntax_ref4)?;
                    let find_fn: LuaFunction = lua.registry_value(&find_key2)?;
                    let core: LuaTable = lua.registry_value(&core_ref2)?;
                    let common: LuaTable = lua.registry_value(&common_ref2)?;
                    let match_pattern: LuaFunction = common.get("match_pattern")?;

                    // Try already-loaded syntaxes
                    if let LuaValue::String(ref s) = filename {
                        let result: LuaValue = find_fn.call((s.clone(), "files"))?;
                        if let LuaValue::Table(t) = result {
                            return Ok(LuaValue::Table(t));
                        }
                    }
                    if let LuaValue::String(ref s) = header {
                        let result: LuaValue = find_fn.call((s.clone(), "headers"))?;
                        if let LuaValue::Table(t) = result {
                            return Ok(LuaValue::Table(t));
                        }
                    }

                    // Try lazy items
                    let lazy_items: LuaTable = syntax.get("lazy_items")?;
                    let lazy_loaded: LuaTable = syntax.get("lazy_loaded")?;
                    let try_fn: LuaFunction = core.get("try")?;
                    let table_remove: LuaFunction = lua.globals().get::<LuaTable>("table")?.get("remove")?;

                    let len = lazy_items.raw_len();
                    for i in (1..=len).rev() {
                        let entry: LuaTable = lazy_items.raw_get(i)?;
                        let entry_files: LuaValue = entry.get("files")?;
                        let entry_headers: LuaValue = entry.get("headers")?;

                        let mut should_load = false;
                        if let LuaValue::String(ref s) = filename {
                            if let LuaValue::Table(ref f) = entry_files {
                                let result: LuaMultiValue = match_pattern.call((s.clone(), f.clone()))?;
                                let vals: Vec<LuaValue> = result.into_vec();
                                if !vals.is_empty() && !matches!(vals.first(), Some(LuaValue::Nil) | Some(LuaValue::Boolean(false))) {
                                    should_load = true;
                                }
                            }
                        }
                        if !should_load {
                            if let LuaValue::String(ref s) = header {
                                if let LuaValue::Table(ref h) = entry_headers {
                                    let result: LuaMultiValue = match_pattern.call((s.clone(), h.clone()))?;
                                    let vals: Vec<LuaValue> = result.into_vec();
                                    if !vals.is_empty() && !matches!(vals.first(), Some(LuaValue::Nil) | Some(LuaValue::Boolean(false))) {
                                        should_load = true;
                                    }
                                }
                            }
                        }

                        if should_load {
                            table_remove.call::<()>((lazy_items.clone(), i))?;

                            // load_lazy_plugin
                            let entry_name: LuaValue = entry.get("name")?;
                            let already: LuaValue = lazy_loaded.get(entry_name.clone())?;
                            if !matches!(already, LuaValue::Boolean(true)) {
                                lazy_loaded.set(entry_name, true)?;
                                let load_fn: LuaValue = entry.get("load")?;
                                let plugin: LuaValue = entry.get("plugin")?;
                                if let LuaValue::Function(ref load) = load_fn {
                                    let _ = try_fn.call::<LuaMultiValue>((load.clone(), plugin));
                                }
                            }

                            // Check if now loaded
                            if let LuaValue::String(ref s) = filename {
                                let result: LuaValue = find_fn.call((s.clone(), "files"))?;
                                if let LuaValue::Table(t) = result {
                                    return Ok(LuaValue::Table(t));
                                }
                            }
                            if let LuaValue::String(ref s) = header {
                                let result: LuaValue = find_fn.call((s.clone(), "headers"))?;
                                if let LuaValue::Table(t) = result {
                                    return Ok(LuaValue::Table(t));
                                }
                            }
                        }
                    }

                    let plain: LuaTable = syntax.get("plain_text_syntax")?;
                    Ok(LuaValue::Table(plain))
                })?,
            )?;

            // Eagerly load all builtin JSON syntax assets via Rust
            let load_assets: LuaValue = native_tokenizer.get("load_assets")?;
            if let LuaValue::Function(load_fn) = load_assets {
                let datadir: LuaValue = lua.globals().get("DATADIR")?;
                let assets: LuaTable = load_fn.call(datadir)?;
                let add_fn: LuaFunction = syntax.get("add")?;
                for pair in assets.sequence_values::<LuaTable>() {
                    let t = pair?;
                    add_fn.call::<()>(t)?;
                }
            }

            // Compatibility stub
            syntax.set(
                "add_from_asset",
                lua.create_function(|_, _asset: LuaValue| Ok(true))?,
            )?;

            Ok(LuaValue::Table(syntax))
        })?,
    )
}
