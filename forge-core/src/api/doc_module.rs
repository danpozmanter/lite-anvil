use mlua::prelude::*;

use std::sync::Arc;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Extract an i64 from a LuaValue, defaulting to 1.
fn lua_to_i64(v: &LuaValue) -> i64 {
    match v {
        LuaValue::Integer(n) => *n,
        LuaValue::Number(n) => *n as i64,
        _ => 1,
    }
}

/// Sort two positions so (line1,col1) <= (line2,col2).
fn sort_positions(line1: i64, col1: i64, line2: i64, col2: i64) -> (i64, i64, i64, i64, bool) {
    if line1 > line2 || (line1 == line2 && col1 > col2) {
        (line2, col2, line1, col1, true)
    } else {
        (line1, col1, line2, col2, false)
    }
}

/// Registers `core.doc` as a pure Rust preload.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "core.doc",
        lua.create_function(|lua, ()| {
            let object: LuaTable = require_table(lua, "core.object")?;
            let doc = object.call_method::<LuaTable>("extend", ())?;

            doc.set(
                "__tostring",
                lua.create_function(|_lua, _self: LuaTable| Ok("Doc"))?,
            )?;

            let class_key = Arc::new(lua.create_registry_value(doc.clone())?);

            // show_read_only_message(self)
            let show_read_only_message = lua.create_function(|lua, this: LuaTable| {
                let warned: bool = this.get::<Option<bool>>("_read_only_warned")?.unwrap_or(false);
                if warned {
                    return Ok(());
                }
                this.set("_read_only_warned", true)?;
                let core: LuaTable = require_table(lua, "core")?;
                let style: LuaTable = require_table(lua, "core.style")?;
                let status_view: LuaTable = core.get("status_view")?;
                let name: String = this.call_method("get_name", ())?;
                let warn_color: LuaValue = style.get("warn")?;
                status_view.call_method::<()>(
                    "show_message",
                    ("!", warn_color, format!("{name} is read-only")),
                )?;
                Ok(())
            })?;
            let show_ro_key = Arc::new(lua.create_registry_value(show_read_only_message)?);

            // ensure_native_buffer(self)
            let ensure_native_buffer = lua.create_function(|lua, this: LuaTable| {
                let buf_id: LuaValue = this.get("buffer_id")?;
                if matches!(buf_id, LuaValue::Nil) {
                    let doc_native: LuaTable = require_table(lua, "doc_native")?;
                    let new_id: LuaValue = doc_native.call_function("buffer_new", ())?;
                    this.set("buffer_id", new_id)?;
                }
                Ok(())
            })?;
            let ensure_buf_key = Arc::new(lua.create_registry_value(ensure_native_buffer)?);

            // sync_native_selections(self)
            let sync_sel_fn = lua.create_function(|lua, this: LuaTable| {
                let buf_id: LuaValue = this.get("buffer_id")?;
                if !matches!(buf_id, LuaValue::Nil) {
                    let doc_native: LuaTable = require_table(lua, "doc_native")?;
                    let selections: LuaValue = this.get("selections")?;
                    doc_native.call_function::<()>("buffer_set_selections", (buf_id, selections))?;
                }
                Ok(())
            })?;
            let sync_sel_key = Arc::new(lua.create_registry_value(sync_sel_fn)?);

            // apply_native_snapshot(self, snapshot)
            let apply_snap_fn =
                lua.create_function(|_lua, (this, snapshot): (LuaTable, LuaValue)| {
                    let snap = match snapshot {
                        LuaValue::Table(t) => t,
                        _ => return Ok(()),
                    };
                    this.set("lines", snap.get::<LuaValue>("lines")?)?;
                    this.set("selections", snap.get::<LuaValue>("selections")?)?;
                    let change_id: LuaValue = snap.get("change_id")?;
                    let undo_stack: LuaTable = this.get("undo_stack")?;
                    undo_stack.set("idx", change_id.clone())?;
                    let redo_stack: LuaTable = this.get("redo_stack")?;
                    redo_stack.set("idx", change_id)?;
                    this.set("crlf", snap.get::<LuaValue>("crlf")?)?;
                    Ok(())
                })?;
            let apply_snap_key = Arc::new(lua.create_registry_value(apply_snap_fn)?);

            // apply_native_edit_result(self, result, undo_stack, time, line_hint) -> bool
            let apply_edit_fn = {
                let snap_k = Arc::clone(&apply_snap_key);
                lua.create_function(
                    move |lua,
                          (this, result, _undo_stack, _time, line_hint): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        i64,
                    )| {
                        let result_tbl = match result {
                            LuaValue::Table(t) => t,
                            _ => return Ok(false),
                        };
                        let lines: LuaTable = this.get("lines")?;
                        let old_lines = lines.raw_len() as i64;
                        let apply: LuaFunction = lua.registry_value(&snap_k)?;
                        apply.call::<()>((this.clone(), LuaValue::Table(result_tbl.clone())))?;
                        let lines: LuaTable = this.get("lines")?;
                        let new_lines = lines.raw_len() as i64;
                        let line_delta: i64 = result_tbl
                            .get::<Option<i64>>("line_delta")?
                            .unwrap_or(new_lines - old_lines);
                        let highlighter: LuaTable = this.get("highlighter")?;
                        if line_delta > 0 {
                            highlighter
                                .call_method::<()>("insert_notify", (line_hint, line_delta))?;
                        } else if line_delta < 0 {
                            highlighter
                                .call_method::<()>("remove_notify", (line_hint, -line_delta))?;
                        } else {
                            highlighter.call_method::<()>("invalidate", line_hint)?;
                        }
                        this.call_method::<()>("sanitize_selection", ())?;
                        Ok(true)
                    },
                )?
            };
            let apply_edit_key = Arc::new(lua.create_registry_value(apply_edit_fn)?);

            // Doc:new(filename, abs_filename, new_file, options)
            doc.set("new", {
                let ek = Arc::clone(&ensure_buf_key);
                lua.create_function(
                    move |lua,
                          (this, filename, abs_filename, new_file, options): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                    )| {
                        let opts = match &options {
                            LuaValue::Table(t) => t.clone(),
                            _ => lua.create_table()?,
                        };
                        this.set(
                            "large_file_mode",
                            opts.get::<Option<bool>>("large_file")?.unwrap_or(false),
                        )?;
                        this.set("large_file_size", opts.get::<LuaValue>("file_size")?)?;
                        this.set(
                            "hard_limited",
                            opts.get::<Option<bool>>("hard_limited")?.unwrap_or(false),
                        )?;
                        this.set(
                            "read_only",
                            opts.get::<Option<bool>>("read_only")?.unwrap_or(false),
                        )?;
                        this.set(
                            "plain_text_mode",
                            opts.get::<Option<bool>>("plain_text")?.unwrap_or(false),
                        )?;
                        this.set("new_file", new_file.clone())?;
                        let ensure: LuaFunction = lua.registry_value(&ek)?;
                        ensure.call::<()>(this.clone())?;
                        this.call_method::<()>("reset", ())?;
                        if !matches!(&filename, LuaValue::Nil) {
                            this.call_method::<()>(
                                "set_filename",
                                (filename, abs_filename.clone()),
                            )?;
                            let is_new = matches!(&new_file, LuaValue::Boolean(true));
                            let lazy_restore =
                                opts.get::<Option<bool>>("lazy_restore")?.unwrap_or(false);
                            if !is_new && !lazy_restore {
                                this.call_method::<()>("load", abs_filename.clone())?;
                                this.call_method::<()>("clean", ())?;
                            } else if !is_new && lazy_restore {
                                this.set("deferred_load", abs_filename)?;
                            }
                        }
                        if matches!(&new_file, LuaValue::Boolean(true)) {
                            let config: LuaTable = require_table(lua, "core.config")?;
                            let line_endings: String = config
                                .get::<Option<String>>("line_endings")?
                                .unwrap_or_default();
                            this.set("crlf", line_endings == "crlf")?;
                        }
                        let doc_native: LuaTable = require_table(lua, "doc_native")?;
                        doc_native.call_function::<()>("update_indent_info", this)?;
                        Ok(())
                    },
                )?
            })?;

            // Doc:ensure_loaded()
            doc.set(
                "ensure_loaded",
                lua.create_function(|_lua, this: LuaTable| -> LuaResult<LuaMultiValue> {
                    let deferred: LuaValue = this.get("deferred_load")?;
                    if matches!(deferred, LuaValue::Nil) {
                        return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(true)]));
                    }
                    this.set("deferred_load", LuaValue::Nil)?;
                    let result: LuaMultiValue = this.call_method("load", deferred)?;
                    let ok = result
                        .iter()
                        .next()
                        .is_some_and(|v| !matches!(v, LuaValue::Nil | LuaValue::Boolean(false)));
                    if ok {
                        this.set("new_file", false)?;
                        this.call_method::<()>("clean", ())?;
                        Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(true)]))
                    } else {
                        let err = result.into_vec().into_iter().nth(1).unwrap_or(LuaValue::Nil);
                        Ok(LuaMultiValue::from_vec(vec![LuaValue::Nil, err]))
                    }
                })?,
            )?;

            // Doc:reset()
            doc.set("reset", {
                let ek = Arc::clone(&ensure_buf_key);
                let ask = Arc::clone(&apply_snap_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let ensure: LuaFunction = lua.registry_value(&ek)?;
                    ensure.call::<()>(this.clone())?;
                    let lines = lua.create_table()?;
                    lines.raw_set(1, "\n")?;
                    this.set("lines", lines)?;
                    let sels = lua.create_table()?;
                    sels.raw_set(1, 1)?;
                    sels.raw_set(2, 1)?;
                    sels.raw_set(3, 1)?;
                    sels.raw_set(4, 1)?;
                    this.set("selections", sels)?;
                    this.set("last_selection", 1)?;
                    let undo_stack = lua.create_table()?;
                    undo_stack.set("idx", 1)?;
                    this.set("undo_stack", undo_stack)?;
                    let redo_stack = lua.create_table()?;
                    redo_stack.set("idx", 1)?;
                    this.set("redo_stack", redo_stack)?;
                    let highlighter_class: LuaTable =
                        require_table(lua, "core.doc.highlighter")?;
                    let hl: LuaTable = highlighter_class.call(this.clone())?;
                    this.set("highlighter", hl)?;
                    this.set("overwrite", false)?;
                    this.set("_read_only_warned", false)?;
                    let buf_id: LuaValue = this.get("buffer_id")?;
                    if !matches!(buf_id, LuaValue::Nil) {
                        let doc_native: LuaTable = require_table(lua, "doc_native")?;
                        let snap: LuaValue =
                            doc_native.call_function("buffer_reset", buf_id.clone())?;
                        let apply: LuaFunction = lua.registry_value(&ask)?;
                        apply.call::<()>((this.clone(), snap))?;
                    }
                    let change_id: LuaValue = this.call_method("get_change_id", ())?;
                    this.set("clean_change_id", change_id)?;
                    let doc_native: LuaTable = require_table(lua, "doc_native")?;
                    let buf_id: LuaValue = this.get("buffer_id")?;
                    let sig: LuaValue =
                        doc_native.call_function("buffer_content_signature", buf_id)?;
                    this.set("clean_signature", sig)?;
                    this.call_method::<()>("reset_syntax", ())?;
                    Ok(())
                })?
            })?;

            // Doc:reset_syntax()
            doc.set(
                "reset_syntax",
                lua.create_function(|lua, this: LuaTable| {
                    let syntax: LuaTable = require_table(lua, "core.syntax")?;
                    let plain_text: bool =
                        this.get::<Option<bool>>("plain_text_mode")?.unwrap_or(false);
                    if plain_text {
                        this.set("syntax", syntax.get::<LuaValue>("plain_text_syntax")?)?;
                        let hl: LuaTable = this.get("highlighter")?;
                        hl.call_method::<()>("soft_reset", ())?;
                        return Ok(());
                    }
                    let header: String = this.call_method("get_text", {
                        let pos: LuaMultiValue =
                            this.call_method("position_offset", (1, 1, 128))?;
                        let vals: Vec<LuaValue> = pos.into_vec();
                        let l2 = vals.first().cloned().unwrap_or(LuaValue::Integer(1));
                        let c2 = vals.get(1).cloned().unwrap_or(LuaValue::Integer(1));
                        (1, 1, l2, c2)
                    })?;
                    let mut path: LuaValue = this.get("abs_filename")?;
                    if matches!(path, LuaValue::Nil) {
                        let fname: LuaValue = this.get("filename")?;
                        if !matches!(fname, LuaValue::Nil) {
                            let core: LuaTable = require_table(lua, "core")?;
                            let root_project_fn: LuaValue = core.get("root_project")?;
                            let mut root_path: Option<String> = None;
                            if let LuaValue::Function(f) = root_project_fn {
                                let rp: LuaValue = f.call(())?;
                                if let LuaValue::Table(rp_tbl) = rp {
                                    let rp_path: LuaValue = rp_tbl.get("path")?;
                                    if let LuaValue::String(s) = rp_path {
                                        root_path = Some(s.to_str()?.to_string());
                                    }
                                }
                            }
                            if let Some(rp) = root_path {
                                let pathsep: String = lua.globals().get("PATHSEP")?;
                                let fname_str = match &fname {
                                    LuaValue::String(s) => s.to_str()?.to_string(),
                                    _ => String::new(),
                                };
                                path = LuaValue::String(
                                    lua.create_string(format!("{rp}{pathsep}{fname_str}"))?,
                                );
                            } else {
                                path = fname;
                            }
                        }
                    }
                    if !matches!(path, LuaValue::Nil) {
                        let common: LuaTable = require_table(lua, "core.common")?;
                        path = common.call_function("normalize_path", path)?;
                    }
                    let syn: LuaValue = syntax.call_function("get", (path, header))?;
                    let current_syn: LuaValue = this.get("syntax")?;
                    let same = match (&current_syn, &syn) {
                        (LuaValue::Table(a), LuaValue::Table(b)) => *a == *b,
                        _ => false,
                    };
                    if !same {
                        this.set("syntax", syn)?;
                        let hl: LuaTable = this.get("highlighter")?;
                        hl.call_method::<()>("soft_reset", ())?;
                    }
                    Ok(())
                })?,
            )?;

            // Doc:set_filename(filename, abs_filename)
            doc.set(
                "set_filename",
                lua.create_function(
                    |_lua, (this, filename, abs_filename): (LuaTable, LuaValue, LuaValue)| {
                        this.set("filename", filename)?;
                        this.set("abs_filename", abs_filename)?;
                        this.call_method::<()>("reset_syntax", ())?;
                        Ok(())
                    },
                )?,
            )?;

            // Doc:load(filename)
            doc.set("load", {
                let ek = Arc::clone(&ensure_buf_key);
                let ask = Arc::clone(&apply_snap_key);
                lua.create_function(
                    move |lua,
                          (this, filename): (LuaTable, LuaValue)|
                          -> LuaResult<LuaMultiValue> {
                        let ensure: LuaFunction = lua.registry_value(&ek)?;
                        ensure.call::<()>(this.clone())?;
                        this.call_method::<()>("reset", ())?;
                        let doc_native: LuaTable = require_table(lua, "doc_native")?;
                        let buf_id: LuaValue = this.get("buffer_id")?;
                        let pcall: LuaFunction = lua.globals().get("pcall")?;
                        let load_fn: LuaFunction = doc_native.get("buffer_load")?;
                        let result: LuaMultiValue =
                            pcall.call((load_fn, buf_id, filename.clone()))?;
                        let vals: Vec<LuaValue> = result.into_vec();
                        let ok = matches!(vals.first(), Some(LuaValue::Boolean(true)));
                        let snapshot = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                        if !ok || matches!(snapshot, LuaValue::Nil) {
                            this.call_method::<()>("reset", ())?;
                            let core: LuaTable = require_table(lua, "core")?;
                            let err_msg = match &snapshot {
                                LuaValue::String(s) => s.to_str()?.to_string(),
                                _ => "unknown error".to_string(),
                            };
                            let fname_str = match &filename {
                                LuaValue::String(s) => s.to_str()?.to_string(),
                                _ => "?".to_string(),
                            };
                            core.call_function::<()>(
                                "error",
                                (format!("Cannot open file {fname_str}: {err_msg}"),),
                            )?;
                            return Ok(LuaMultiValue::from_vec(vec![LuaValue::Nil, snapshot]));
                        }
                        let apply: LuaFunction = lua.registry_value(&ask)?;
                        apply.call::<()>((this.clone(), snapshot))?;
                        let lines: LuaTable = this.get("lines")?;
                        let nlines = lines.raw_len() as i64;
                        let hl: LuaTable = this.get("highlighter")?;
                        let hl_lines: LuaTable = hl.get("lines")?;
                        for i in 1..=nlines {
                            hl_lines.raw_set(i, false)?;
                        }
                        this.call_method::<()>("reset_syntax", ())?;
                        Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(true)]))
                    },
                )?
            })?;

            // Doc:reload()
            doc.set(
                "reload",
                lua.create_function(|lua, this: LuaTable| {
                    this.call_method::<()>("ensure_loaded", ())?;
                    let fname: LuaValue = this.get("filename")?;
                    if !matches!(fname, LuaValue::Nil) {
                        let sel: LuaMultiValue = this.call_method("get_selection", ())?;
                        let abs: LuaValue = this.get("abs_filename")?;
                        this.call_method::<()>("load", abs)?;
                        this.call_method::<()>("clean", ())?;
                        let sel_tbl = lua.create_table()?;
                        for (i, v) in sel.into_vec().into_iter().enumerate() {
                            sel_tbl.raw_set((i + 1) as i64, v)?;
                        }
                        let table_unpack: LuaFunction =
                            lua.globals().get::<LuaTable>("table")?.get("unpack")?;
                        let unpacked: LuaMultiValue = table_unpack.call(sel_tbl)?;
                        this.call_method::<()>("set_selection", unpacked)?;
                    }
                    Ok(())
                })?,
            )?;

            // Doc:save(filename, abs_filename)
            doc.set("save", {
                let rok = Arc::clone(&show_ro_key);
                lua.create_function(
                    move |lua,
                          (this, filename, abs_filename): (LuaTable, LuaValue, LuaValue)| {
                        this.call_method::<()>("ensure_loaded", ())?;
                        let read_only: bool =
                            this.get::<Option<bool>>("read_only")?.unwrap_or(false);
                        if read_only {
                            let show_ro: LuaFunction = lua.registry_value(&rok)?;
                            show_ro.call::<()>(this)?;
                            return Ok(());
                        }
                        let mut fname = filename;
                        let mut abs_fname = abs_filename;
                        if matches!(fname, LuaValue::Nil) {
                            let self_fname: LuaValue = this.get("filename")?;
                            if matches!(self_fname, LuaValue::Nil) {
                                return Err(LuaError::runtime("no filename set to default to"));
                            }
                            fname = self_fname;
                            abs_fname = this.get("abs_filename")?;
                        } else {
                            let self_fname: LuaValue = this.get("filename")?;
                            if matches!(self_fname, LuaValue::Nil)
                                && matches!(abs_fname, LuaValue::Nil)
                            {
                                return Err(LuaError::runtime(
                                    "calling save on unnamed doc without absolute path",
                                ));
                            }
                        }
                        let self_fname: LuaValue = this.get("filename")?;
                        let self_abs: LuaValue = this.get("abs_filename")?;
                        let filename_changed = fname != self_fname || abs_fname != self_abs;
                        let doc_native: LuaTable = require_table(lua, "doc_native")?;
                        let buf_id: LuaValue = this.get("buffer_id")?;
                        let crlf: LuaValue = this.get("crlf")?;
                        doc_native.call_function::<()>(
                            "buffer_save",
                            (buf_id, abs_fname.clone(), crlf),
                        )?;
                        if filename_changed {
                            this.call_method::<()>("set_filename", (fname, abs_fname))?;
                        }
                        this.set("new_file", false)?;
                        this.call_method::<()>("clean", ())?;
                        Ok(())
                    },
                )?
            })?;

            // Doc:get_name()
            doc.set(
                "get_name",
                lua.create_function(|_lua, this: LuaTable| {
                    let fname: LuaValue = this.get("filename")?;
                    match fname {
                        LuaValue::String(s) => Ok(s.to_str()?.to_string()),
                        _ => Ok("unsaved".to_string()),
                    }
                })?,
            )?;

            // Doc:is_dirty()
            doc.set(
                "is_dirty",
                lua.create_function(|lua, this: LuaTable| {
                    let new_file: bool = this.get::<Option<bool>>("new_file")?.unwrap_or(false);
                    if new_file {
                        let fname: LuaValue = this.get("filename")?;
                        if !matches!(fname, LuaValue::Nil) {
                            return Ok(true);
                        }
                        let lines: LuaTable = this.get("lines")?;
                        let nlines = lines.raw_len() as i64;
                        if nlines > 1 {
                            return Ok(true);
                        }
                        let first: String = lines.raw_get(1)?;
                        return Ok(first.len() > 1);
                    }
                    let change_id: i64 = this.call_method("get_change_id", ())?;
                    let clean_id: i64 = this.get("clean_change_id")?;
                    if clean_id == change_id {
                        return Ok(false);
                    }
                    let buf_id: LuaValue = this.get("buffer_id")?;
                    let clean_sig: LuaValue = this.get("clean_signature")?;
                    if matches!(buf_id, LuaValue::Nil) || matches!(clean_sig, LuaValue::Nil) {
                        return Ok(true);
                    }
                    let doc_native: LuaTable = require_table(lua, "doc_native")?;
                    let current_sig: LuaValue =
                        doc_native.call_function("buffer_content_signature", buf_id)?;
                    Ok(clean_sig != current_sig)
                })?,
            )?;

            // Doc:clean()
            doc.set(
                "clean",
                lua.create_function(|lua, this: LuaTable| {
                    let change_id: LuaValue = this.call_method("get_change_id", ())?;
                    this.set("clean_change_id", change_id)?;
                    let doc_native: LuaTable = require_table(lua, "doc_native")?;
                    let buf_id: LuaValue = this.get("buffer_id")?;
                    let sig: LuaValue =
                        doc_native.call_function("buffer_content_signature", buf_id)?;
                    this.set("clean_signature", sig)?;
                    let indent_info: LuaValue = this.get("indent_info")?;
                    let confirmed = match &indent_info {
                        LuaValue::Table(t) => t.get::<Option<bool>>("confirmed")?.unwrap_or(false),
                        _ => false,
                    };
                    if !confirmed {
                        doc_native.call_function::<()>("update_indent_info", this)?;
                    }
                    Ok(())
                })?,
            )?;

            // Doc:get_content_signature()
            doc.set(
                "get_content_signature",
                lua.create_function(|lua, (this, _change_id): (LuaTable, LuaValue)| {
                    let doc_native: LuaTable = require_table(lua, "doc_native")?;
                    let buf_id: LuaValue = this.get("buffer_id")?;
                    doc_native.call_function::<LuaValue>("buffer_content_signature", buf_id)
                })?,
            )?;

            // Doc:get_indent_info()
            doc.set(
                "get_indent_info",
                lua.create_function(
                    |lua, this: LuaTable| -> LuaResult<LuaMultiValue> {
                        let config: LuaTable = require_table(lua, "core.config")?;
                        let indent_info: LuaValue = this.get("indent_info")?;
                        let info = match indent_info {
                            LuaValue::Table(t) => t,
                            _ => {
                                return Ok(LuaMultiValue::from_vec(vec![
                                    config.get::<LuaValue>("tab_type")?,
                                    config.get::<LuaValue>("indent_size")?,
                                    LuaValue::Boolean(false),
                                ]));
                            }
                        };
                        let itype: LuaValue = info.get("type")?;
                        let isize_val: LuaValue = info.get("size")?;
                        let confirmed: bool =
                            info.get::<Option<bool>>("confirmed")?.unwrap_or(false);
                        let tab_type = if matches!(itype, LuaValue::Nil) {
                            config.get("tab_type")?
                        } else {
                            itype
                        };
                        let indent_size = if matches!(isize_val, LuaValue::Nil) {
                            config.get("indent_size")?
                        } else {
                            isize_val
                        };
                        Ok(LuaMultiValue::from_vec(vec![
                            tab_type,
                            indent_size,
                            LuaValue::Boolean(confirmed),
                        ]))
                    },
                )?,
            )?;

            // Doc:get_change_id()
            doc.set(
                "get_change_id",
                lua.create_function(|lua, this: LuaTable| {
                    let doc_native: LuaTable = require_table(lua, "doc_native")?;
                    let buf_id: LuaValue = this.get("buffer_id")?;
                    doc_native.call_function::<LuaValue>("buffer_get_change_id", buf_id)
                })?,
            )?;

            // Doc:get_selection(sort)
            doc.set(
                "get_selection",
                lua.create_function(
                    |_lua, (this, sort): (LuaTable, LuaValue)| -> LuaResult<LuaMultiValue> {
                        let last_sel: i64 = this.get("last_selection")?;
                        let result: LuaMultiValue =
                            this.call_method("get_selection_idx", (last_sel, sort.clone()))?;
                        let vals: Vec<LuaValue> = result.into_vec();
                        let line1_nil = vals
                            .first()
                            .is_none_or(|v| matches!(v, LuaValue::Nil));
                        if line1_nil {
                            return this.call_method("get_selection_idx", (1, sort));
                        }
                        Ok(LuaMultiValue::from_vec(vals))
                    },
                )?,
            )?;

            // Doc:get_selection_idx(idx, sort)
            doc.set(
                "get_selection_idx",
                lua.create_function(
                    |_lua,
                     (this, idx, sort): (LuaTable, i64, LuaValue)|
                     -> LuaResult<LuaMultiValue> {
                        let sels: LuaTable = this.get("selections")?;
                        let line1: LuaValue = sels.raw_get(idx * 4 - 3)?;
                        let col1: LuaValue = sels.raw_get(idx * 4 - 2)?;
                        let line2: LuaValue = sels.raw_get(idx * 4 - 1)?;
                        let col2: LuaValue = sels.raw_get(idx * 4)?;
                        if !matches!(line1, LuaValue::Nil)
                            && !matches!(sort, LuaValue::Nil | LuaValue::Boolean(false))
                        {
                            let (sl1, sc1, sl2, sc2, swap) = sort_positions(
                                lua_to_i64(&line1),
                                lua_to_i64(&col1),
                                lua_to_i64(&line2),
                                lua_to_i64(&col2),
                            );
                            Ok(LuaMultiValue::from_vec(vec![
                                LuaValue::Integer(sl1),
                                LuaValue::Integer(sc1),
                                LuaValue::Integer(sl2),
                                LuaValue::Integer(sc2),
                                LuaValue::Boolean(swap),
                            ]))
                        } else {
                            Ok(LuaMultiValue::from_vec(vec![line1, col1, line2, col2]))
                        }
                    },
                )?,
            )?;

            // Doc:get_selection_text(limit)
            doc.set(
                "get_selection_text",
                lua.create_function(|lua, (this, limit): (LuaTable, LuaValue)| {
                    let max_count: i64 = match limit {
                        LuaValue::Integer(n) => n,
                        LuaValue::Number(n) => n as i64,
                        _ => i64::MAX,
                    };
                    let result = lua.create_table()?;
                    let mut count = 0i64;
                    let get_sels: LuaFunction = this.get("get_selections")?;
                    let multi: LuaMultiValue = get_sels.call(this.clone())?;
                    let vals: Vec<LuaValue> = multi.into_vec();
                    let iter_fn = match vals.first() {
                        Some(LuaValue::Function(f)) => f.clone(),
                        _ => return Ok("".to_string()),
                    };
                    let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                    let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                    loop {
                        let ir: LuaMultiValue =
                            iter_fn.call((invariant.clone(), control.clone()))?;
                        let iv: Vec<LuaValue> = ir.into_vec();
                        let idx_val = iv.first().cloned().unwrap_or(LuaValue::Nil);
                        if matches!(idx_val, LuaValue::Nil) {
                            break;
                        }
                        control = idx_val.clone();
                        let idx = lua_to_i64(&idx_val);
                        if idx > max_count {
                            break;
                        }
                        let l1 = iv.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let c1 = iv.get(2).cloned().unwrap_or(LuaValue::Nil);
                        let l2 = iv.get(3).cloned().unwrap_or(LuaValue::Nil);
                        let c2 = iv.get(4).cloned().unwrap_or(LuaValue::Nil);
                        if l1 != l2 || c1 != c2 {
                            let text: String =
                                this.call_method("get_text", (l1, c1, l2, c2))?;
                            if !text.is_empty() {
                                count += 1;
                                result.raw_set(count, text)?;
                            }
                        }
                    }
                    let table_concat: LuaFunction =
                        lua.globals().get::<LuaTable>("table")?.get("concat")?;
                    table_concat.call((result, "\n"))
                })?,
            )?;

            // Doc:has_selection()
            doc.set(
                "has_selection",
                lua.create_function(|_lua, this: LuaTable| {
                    let result: LuaMultiValue =
                        this.call_method("get_selection", LuaValue::Boolean(false))?;
                    let vals: Vec<LuaValue> = result.into_vec();
                    let l1 = vals.first().cloned().unwrap_or(LuaValue::Nil);
                    let c1 = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                    let l2 = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                    let c2 = vals.get(3).cloned().unwrap_or(LuaValue::Nil);
                    Ok(l1 != l2 || c1 != c2)
                })?,
            )?;

            // Doc:has_any_selection()
            doc.set(
                "has_any_selection",
                lua.create_function(|_lua, this: LuaTable| {
                    let get_sels: LuaFunction = this.get("get_selections")?;
                    let multi: LuaMultiValue = get_sels.call(this.clone())?;
                    let vals: Vec<LuaValue> = multi.into_vec();
                    let iter_fn = match vals.first() {
                        Some(LuaValue::Function(f)) => f.clone(),
                        _ => return Ok(false),
                    };
                    let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                    let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                    loop {
                        let ir: LuaMultiValue =
                            iter_fn.call((invariant.clone(), control.clone()))?;
                        let iv: Vec<LuaValue> = ir.into_vec();
                        let idx_val = iv.first().cloned().unwrap_or(LuaValue::Nil);
                        if matches!(idx_val, LuaValue::Nil) {
                            break;
                        }
                        control = idx_val;
                        let l1 = iv.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let c1 = iv.get(2).cloned().unwrap_or(LuaValue::Nil);
                        let l2 = iv.get(3).cloned().unwrap_or(LuaValue::Nil);
                        let c2 = iv.get(4).cloned().unwrap_or(LuaValue::Nil);
                        if l1 != l2 || c1 != c2 {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                })?,
            )?;

            // Doc:sanitize_selection()
            doc.set(
                "sanitize_selection",
                lua.create_function(|_lua, this: LuaTable| {
                    let get_sels: LuaFunction = this.get("get_selections")?;
                    let multi: LuaMultiValue = get_sels.call(this.clone())?;
                    let vals: Vec<LuaValue> = multi.into_vec();
                    let iter_fn = match vals.first() {
                        Some(LuaValue::Function(f)) => f.clone(),
                        _ => return Ok(()),
                    };
                    let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                    let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                    loop {
                        let ir: LuaMultiValue =
                            iter_fn.call((invariant.clone(), control.clone()))?;
                        let iv: Vec<LuaValue> = ir.into_vec();
                        let idx_val = iv.first().cloned().unwrap_or(LuaValue::Nil);
                        if matches!(idx_val, LuaValue::Nil) {
                            break;
                        }
                        control = idx_val.clone();
                        let l1 = iv.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let c1 = iv.get(2).cloned().unwrap_or(LuaValue::Nil);
                        let l2 = iv.get(3).cloned().unwrap_or(LuaValue::Nil);
                        let c2 = iv.get(4).cloned().unwrap_or(LuaValue::Nil);
                        this.call_method::<()>("set_selections", (idx_val, l1, c1, l2, c2))?;
                    }
                    Ok(())
                })?,
            )?;

            // Doc:set_selections(idx, line1, col1, line2, col2, swap, rm)
            doc.set("set_selections", {
                let ssk = Arc::clone(&sync_sel_key);
                lua.create_function(
                    move |lua,
                          (this, idx, line1, col1, line2, col2, swap, rm): (
                        LuaTable,
                        i64,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                    )| {
                        let l2_nil = matches!(line2, LuaValue::Nil);
                        let c2_nil = matches!(col2, LuaValue::Nil);
                        if l2_nil != c2_nil {
                            return Err(LuaError::runtime("expected 3 or 5 arguments"));
                        }
                        let do_swap = matches!(swap, LuaValue::Boolean(true));
                        let (mut l1, mut c1, mut l2, mut c2) = if do_swap {
                            (line2, col2, line1, col1)
                        } else {
                            (line1, col1, line2, col2)
                        };
                        let san1: LuaMultiValue =
                            this.call_method("sanitize_position", (l1, c1))?;
                        let sv1: Vec<LuaValue> = san1.into_vec();
                        l1 = sv1.first().cloned().unwrap_or(LuaValue::Integer(1));
                        c1 = sv1.get(1).cloned().unwrap_or(LuaValue::Integer(1));
                        if l2_nil {
                            l2 = l1.clone();
                            c2 = c1.clone();
                        }
                        let san2: LuaMultiValue =
                            this.call_method("sanitize_position", (l2, c2))?;
                        let sv2: Vec<LuaValue> = san2.into_vec();
                        l2 = sv2.first().cloned().unwrap_or(LuaValue::Integer(1));
                        c2 = sv2.get(1).cloned().unwrap_or(LuaValue::Integer(1));
                        let common: LuaTable = require_table(lua, "core.common")?;
                        let sels: LuaTable = this.get("selections")?;
                        let rm_val: i64 = match &rm {
                            LuaValue::Integer(n) => *n,
                            LuaValue::Number(n) => *n as i64,
                            _ => 4,
                        };
                        let insert_tbl = lua.create_table()?;
                        insert_tbl.raw_set(1, l1)?;
                        insert_tbl.raw_set(2, c1)?;
                        insert_tbl.raw_set(3, l2)?;
                        insert_tbl.raw_set(4, c2)?;
                        common.call_function::<()>(
                            "splice",
                            (sels, (idx - 1) * 4 + 1, rm_val, insert_tbl),
                        )?;
                        let sync: LuaFunction = lua.registry_value(&ssk)?;
                        sync.call::<()>(this)?;
                        Ok(())
                    },
                )?
            })?;

            // Doc:add_selection(line1, col1, line2, col2, swap)
            doc.set(
                "add_selection",
                lua.create_function(
                    |_lua,
                     (this, line1, col1, line2, col2, swap): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                    )| {
                        let l2_v = if matches!(line2, LuaValue::Nil) {
                            line1.clone()
                        } else {
                            line2.clone()
                        };
                        let c2_v = if matches!(col2, LuaValue::Nil) {
                            col1.clone()
                        } else {
                            col2.clone()
                        };
                        let (sl1, sc1, _, _, _) = sort_positions(
                            lua_to_i64(&line1),
                            lua_to_i64(&col1),
                            lua_to_i64(&l2_v),
                            lua_to_i64(&c2_v),
                        );
                        let sels: LuaTable = this.get("selections")?;
                        let sel_count = sels.raw_len() as i64 / 4;
                        let mut target = sel_count + 1;
                        let get_sels: LuaFunction = this.get("get_selections")?;
                        let multi: LuaMultiValue =
                            get_sels.call((this.clone(), LuaValue::Boolean(true)))?;
                        let vals: Vec<LuaValue> = multi.into_vec();
                        let iter_fn = match vals.first() {
                            Some(LuaValue::Function(f)) => f.clone(),
                            _ => {
                                return Err(LuaError::runtime(
                                    "get_selections did not return iterator",
                                ))
                            }
                        };
                        let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                        loop {
                            let ir: LuaMultiValue =
                                iter_fn.call((invariant.clone(), control.clone()))?;
                            let iv: Vec<LuaValue> = ir.into_vec();
                            let idx_val = iv.first().cloned().unwrap_or(LuaValue::Nil);
                            if matches!(idx_val, LuaValue::Nil) {
                                break;
                            }
                            control = idx_val.clone();
                            let tl1 = lua_to_i64(iv.get(1).unwrap_or(&LuaValue::Integer(1)));
                            let tc1 = lua_to_i64(iv.get(2).unwrap_or(&LuaValue::Integer(1)));
                            if sl1 < tl1 || (sl1 == tl1 && sc1 < tc1) {
                                target = lua_to_i64(&idx_val);
                                break;
                            }
                        }
                        this.call_method::<()>(
                            "set_selections",
                            (target, line1, col1, line2, col2, swap, 0),
                        )?;
                        this.set("last_selection", target)?;
                        Ok(())
                    },
                )?,
            )?;

            // Doc:remove_selection(idx)
            doc.set("remove_selection", {
                let ssk = Arc::clone(&sync_sel_key);
                lua.create_function(move |lua, (this, idx): (LuaTable, i64)| {
                    let last: i64 = this.get("last_selection")?;
                    if last >= idx {
                        this.set("last_selection", last - 1)?;
                    }
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let sels: LuaTable = this.get("selections")?;
                    common.call_function::<()>("splice", (sels, (idx - 1) * 4 + 1, 4))?;
                    let sync: LuaFunction = lua.registry_value(&ssk)?;
                    sync.call::<()>(this)?;
                    Ok(())
                })?
            })?;

            // Doc:set_selection(line1, col1, line2, col2, swap)
            doc.set("set_selection", {
                let ssk = Arc::clone(&sync_sel_key);
                lua.create_function(
                    move |lua,
                          (this, line1, col1, line2, col2, swap): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                    )| {
                        this.set("selections", lua.create_table()?)?;
                        this.call_method::<()>(
                            "set_selections",
                            (1, line1, col1, line2, col2, swap),
                        )?;
                        this.set("last_selection", 1)?;
                        let sync: LuaFunction = lua.registry_value(&ssk)?;
                        sync.call::<()>(this)?;
                        Ok(())
                    },
                )?
            })?;

            // Doc:merge_cursors(idx)
            doc.set("merge_cursors", {
                let ssk = Arc::clone(&sync_sel_key);
                lua.create_function(move |lua, (this, idx): (LuaTable, LuaValue)| {
                    let sels: LuaTable = this.get("selections")?;
                    let sels_len = sels.raw_len() as i64;
                    let table_index = match &idx {
                        LuaValue::Integer(n) => Some((*n - 1) * 4 + 1),
                        LuaValue::Number(n) => Some((*n as i64 - 1) * 4 + 1),
                        _ => None,
                    };
                    let start = table_index.unwrap_or(sels_len - 3);
                    let end = table_index.unwrap_or(5);
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let mut i = start;
                    while i >= end {
                        let mut j = 1i64;
                        while j <= i - 4 {
                            let si: LuaValue = sels.raw_get(i)?;
                            let sj: LuaValue = sels.raw_get(j)?;
                            let si1: LuaValue = sels.raw_get(i + 1)?;
                            let sj1: LuaValue = sels.raw_get(j + 1)?;
                            if si == sj && si1 == sj1 {
                                common.call_function::<()>(
                                    "splice",
                                    (sels.clone(), i, 4),
                                )?;
                                let last: i64 = this.get("last_selection")?;
                                if last >= (i + 3) / 4 {
                                    this.set("last_selection", last - 1)?;
                                }
                                break;
                            }
                            j += 4;
                        }
                        i -= 4;
                    }
                    let sync: LuaFunction = lua.registry_value(&ssk)?;
                    sync.call::<()>(this)?;
                    Ok(())
                })?
            })?;

            // Doc:get_selections(sort_intra, idx_reverse)
            doc.set(
                "get_selections",
                lua.create_function(
                    |lua,
                     (this, sort_intra, idx_reverse): (LuaTable, LuaValue, LuaValue)| {
                        let sels: LuaTable = this.get("selections")?;
                        let sels_len = sels.raw_len() as i64;
                        if sels_len == 0 {
                            let new_sels = lua.create_table()?;
                            new_sels.raw_set(1, 1)?;
                            new_sels.raw_set(2, 1)?;
                            new_sels.raw_set(3, 1)?;
                            new_sels.raw_set(4, 1)?;
                            this.set("selections", new_sels)?;
                        }
                        let invariant = lua.create_table()?;
                        let sels: LuaTable = this.get("selections")?;
                        invariant.raw_set(1, sels.clone())?;
                        invariant.raw_set(2, sort_intra)?;
                        invariant.raw_set(3, idx_reverse.clone())?;
                        let sels_len = sels.raw_len() as i64;
                        let initial = if matches!(idx_reverse, LuaValue::Boolean(true)) {
                            (sels_len / 4) + 1
                        } else {
                            let offset = match &idx_reverse {
                                LuaValue::Integer(n) => *n,
                                LuaValue::Number(n) => *n as i64,
                                _ => -1,
                            };
                            offset + 1
                        };
                        let iter_fn = lua.create_function(
                            |_lua, (inv, idx): (LuaValue, LuaValue)| -> LuaResult<LuaMultiValue> {
                                let inv_tbl = match &inv {
                                    LuaValue::Table(t) => t,
                                    _ => return Ok(LuaMultiValue::new()),
                                };
                                let selections: LuaTable = inv_tbl.raw_get(1)?;
                                let do_sort = matches!(
                                    inv_tbl.raw_get::<LuaValue>(2)?,
                                    LuaValue::Boolean(true)
                                );
                                let reverse_val: LuaValue = inv_tbl.raw_get(3)?;
                                let is_truthy = !matches!(
                                    reverse_val,
                                    LuaValue::Nil | LuaValue::Boolean(false)
                                );
                                let idx_i = lua_to_i64(&idx);
                                let target = if is_truthy {
                                    idx_i * 4 - 7
                                } else {
                                    idx_i * 4 + 1
                                };
                                let sel_len = selections.raw_len() as i64;
                                if target > sel_len || target <= 0 {
                                    return Ok(LuaMultiValue::new());
                                }
                                if let LuaValue::Integer(n) = &reverse_val {
                                    if *n != idx_i - 1 {
                                        return Ok(LuaMultiValue::new());
                                    }
                                }
                                if let LuaValue::Number(n) = &reverse_val {
                                    if (*n as i64) != idx_i - 1 {
                                        return Ok(LuaMultiValue::new());
                                    }
                                }
                                let next_idx = if is_truthy { idx_i - 1 } else { idx_i + 1 };
                                let l1: LuaValue = selections.raw_get(target)?;
                                let c1: LuaValue = selections.raw_get(target + 1)?;
                                let l2: LuaValue = selections.raw_get(target + 2)?;
                                let c2: LuaValue = selections.raw_get(target + 3)?;
                                if do_sort {
                                    let (sl1, sc1, sl2, sc2, swap) = sort_positions(
                                        lua_to_i64(&l1),
                                        lua_to_i64(&c1),
                                        lua_to_i64(&l2),
                                        lua_to_i64(&c2),
                                    );
                                    Ok(LuaMultiValue::from_vec(vec![
                                        LuaValue::Integer(next_idx),
                                        LuaValue::Integer(sl1),
                                        LuaValue::Integer(sc1),
                                        LuaValue::Integer(sl2),
                                        LuaValue::Integer(sc2),
                                        LuaValue::Boolean(swap),
                                    ]))
                                } else {
                                    Ok(LuaMultiValue::from_vec(vec![
                                        LuaValue::Integer(next_idx),
                                        l1,
                                        c1,
                                        l2,
                                        c2,
                                    ]))
                                }
                            },
                        )?;
                        Ok(LuaMultiValue::from_vec(vec![
                            LuaValue::Function(iter_fn),
                            LuaValue::Table(invariant),
                            LuaValue::Integer(initial),
                        ]))
                    },
                )?,
            )?;

            // Doc:sanitize_position(line, col)
            doc.set(
                "sanitize_position",
                lua.create_function(
                    |lua,
                     (this, line, col): (LuaTable, LuaValue, LuaValue)|
                     -> LuaResult<LuaMultiValue> {
                        let lines: LuaTable = this.get("lines")?;
                        let nlines = lines.raw_len() as i64;
                        let line_i = lua_to_i64(&line);
                        let col_i = lua_to_i64(&col);
                        if line_i > nlines {
                            let last_line: String = lines.raw_get(nlines)?;
                            return Ok(LuaMultiValue::from_vec(vec![
                                LuaValue::Integer(nlines),
                                LuaValue::Integer(last_line.len() as i64),
                            ]));
                        }
                        if line_i < 1 {
                            return Ok(LuaMultiValue::from_vec(vec![
                                LuaValue::Integer(1),
                                LuaValue::Integer(1),
                            ]));
                        }
                        let current_line: String = lines.raw_get(line_i)?;
                        let common: LuaTable = require_table(lua, "core.common")?;
                        let clamped: i64 = common.call_function(
                            "clamp",
                            (col_i, 1, current_line.len() as i64),
                        )?;
                        Ok(LuaMultiValue::from_vec(vec![
                            LuaValue::Integer(line_i),
                            LuaValue::Integer(clamped),
                        ]))
                    },
                )?,
            )?;

            // Doc:position_offset(line, col, ...)
            doc.set(
                "position_offset",
                lua.create_function(
                    |lua,
                     (this, line, col, args): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaMultiValue,
                    )|
                     -> LuaResult<LuaMultiValue> {
                        this.call_method::<()>("ensure_loaded", ())?;
                        let args_vec: Vec<LuaValue> = args.into_vec();
                        let first_arg = args_vec.first().cloned().unwrap_or(LuaValue::Nil);
                        let num_args = args_vec.len();
                        if !matches!(
                            first_arg,
                            LuaValue::Integer(_) | LuaValue::Number(_)
                        ) {
                            let san: LuaMultiValue =
                                this.call_method("sanitize_position", (line, col))?;
                            let sv: Vec<LuaValue> = san.into_vec();
                            let sline = sv.first().cloned().unwrap_or(LuaValue::Integer(1));
                            let scol = sv.get(1).cloned().unwrap_or(LuaValue::Integer(1));
                            if let LuaValue::Function(f) = &first_arg {
                                let mut call_args =
                                    vec![LuaValue::Table(this), sline, scol];
                                call_args.extend(args_vec.into_iter().skip(1));
                                return f.call(LuaMultiValue::from_vec(call_args));
                            }
                            return Err(LuaError::runtime(
                                "position_offset: expected function or number",
                            ));
                        }
                        if num_args == 1 {
                            let doc_native: LuaTable = require_table(lua, "doc_native")?;
                            let buf_id: LuaValue = this.get("buffer_id")?;
                            doc_native.call_function(
                                "buffer_position_offset",
                                (buf_id, line, col, first_arg),
                            )
                        } else if num_args == 2 {
                            let line_off = lua_to_i64(&first_arg);
                            let col_off = lua_to_i64(
                                args_vec.get(1).unwrap_or(&LuaValue::Integer(0)),
                            );
                            this.call_method(
                                "sanitize_position",
                                (lua_to_i64(&line) + line_off, lua_to_i64(&col) + col_off),
                            )
                        } else {
                            Err(LuaError::runtime("bad number of arguments"))
                        }
                    },
                )?,
            )?;

            // Doc:get_text(line1, col1, line2, col2, inclusive)
            doc.set(
                "get_text",
                lua.create_function(
                    |lua,
                     (this, line1, col1, line2, col2, inclusive): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                    )| {
                        this.call_method::<()>("ensure_loaded", ())?;
                        let san1: LuaMultiValue =
                            this.call_method("sanitize_position", (line1, col1))?;
                        let sv1: Vec<LuaValue> = san1.into_vec();
                        let l1 = lua_to_i64(sv1.first().unwrap_or(&LuaValue::Integer(1)));
                        let c1 = lua_to_i64(sv1.get(1).unwrap_or(&LuaValue::Integer(1)));
                        let san2: LuaMultiValue =
                            this.call_method("sanitize_position", (line2, col2))?;
                        let sv2: Vec<LuaValue> = san2.into_vec();
                        let l2 = lua_to_i64(sv2.first().unwrap_or(&LuaValue::Integer(1)));
                        let c2 = lua_to_i64(sv2.get(1).unwrap_or(&LuaValue::Integer(1)));
                        let (sl1, sc1, sl2, sc2, _) = sort_positions(l1, c1, l2, c2);
                        let doc_native: LuaTable = require_table(lua, "doc_native")?;
                        let buf_id: LuaValue = this.get("buffer_id")?;
                        doc_native.call_function::<LuaValue>(
                            "buffer_get_text",
                            (buf_id, sl1, sc1, sl2, sc2, inclusive),
                        )
                    },
                )?,
            )?;

            // Doc:get_char(line, col)
            doc.set(
                "get_char",
                lua.create_function(
                    |lua, (this, line, col): (LuaTable, LuaValue, LuaValue)| {
                        this.call_method::<()>("ensure_loaded", ())?;
                        let san: LuaMultiValue =
                            this.call_method("sanitize_position", (line, col))?;
                        let sv: Vec<LuaValue> = san.into_vec();
                        let line_i = lua_to_i64(sv.first().unwrap_or(&LuaValue::Integer(1)));
                        let col_i = lua_to_i64(sv.get(1).unwrap_or(&LuaValue::Integer(1)));
                        let lines: LuaTable = this.get("lines")?;
                        let line_str: String = lines.raw_get(line_i)?;
                        let string_sub: LuaFunction =
                            lua.globals().get::<LuaTable>("string")?.get("sub")?;
                        string_sub.call::<LuaValue>((line_str, col_i, col_i))
                    },
                )?,
            )?;

            // Doc:raw_insert(line, col, text, undo_stack, time)
            doc.set("raw_insert", {
                let ssk = Arc::clone(&sync_sel_key);
                let aek = Arc::clone(&apply_edit_key);
                lua.create_function(
                    move |lua,
                          (this, line, col, text, undo_stack, time): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                    )| {
                        let sync: LuaFunction = lua.registry_value(&ssk)?;
                        sync.call::<()>(this.clone())?;
                        let doc_native: LuaTable = require_table(lua, "doc_native")?;
                        let buf_id: LuaValue = this.get("buffer_id")?;
                        let result: LuaValue = doc_native.call_function(
                            "buffer_apply_insert",
                            (buf_id, line.clone(), col, text),
                        )?;
                        let apply_edit: LuaFunction = lua.registry_value(&aek)?;
                        apply_edit.call::<bool>((
                            this,
                            result,
                            undo_stack,
                            time,
                            lua_to_i64(&line),
                        ))?;
                        Ok(())
                    },
                )?
            })?;

            // Doc:raw_remove(line1, col1, line2, col2, undo_stack, time)
            doc.set("raw_remove", {
                let ssk = Arc::clone(&sync_sel_key);
                let aek = Arc::clone(&apply_edit_key);
                lua.create_function(
                    move |lua,
                          (this, line1, col1, line2, col2, undo_stack, time): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                    )| {
                        let sync: LuaFunction = lua.registry_value(&ssk)?;
                        sync.call::<()>(this.clone())?;
                        let doc_native: LuaTable = require_table(lua, "doc_native")?;
                        let buf_id: LuaValue = this.get("buffer_id")?;
                        let result: LuaValue = doc_native.call_function(
                            "buffer_apply_remove",
                            (buf_id, line1.clone(), col1, line2, col2),
                        )?;
                        let apply_edit: LuaFunction = lua.registry_value(&aek)?;
                        apply_edit.call::<bool>((
                            this,
                            result,
                            undo_stack,
                            time,
                            lua_to_i64(&line1),
                        ))?;
                        Ok(())
                    },
                )?
            })?;

            // Doc:insert(line, col, text)
            doc.set("insert", {
                let rok = Arc::clone(&show_ro_key);
                lua.create_function(
                    move |lua,
                          (this, line, col, text): (LuaTable, LuaValue, LuaValue, LuaValue)| {
                        this.call_method::<()>("ensure_loaded", ())?;
                        let read_only: bool =
                            this.get::<Option<bool>>("read_only")?.unwrap_or(false);
                        if read_only {
                            let show_ro: LuaFunction = lua.registry_value(&rok)?;
                            show_ro.call::<()>(this)?;
                            return Ok(());
                        }
                        let redo_stack = lua.create_table()?;
                        redo_stack.set("idx", 1)?;
                        this.set("redo_stack", redo_stack)?;
                        let change_id: i64 = this.call_method("get_change_id", ())?;
                        let clean_id: i64 = this.get("clean_change_id")?;
                        if change_id < clean_id {
                            this.set("clean_change_id", -1)?;
                        }
                        let san: LuaMultiValue =
                            this.call_method("sanitize_position", (line, col))?;
                        let sv: Vec<LuaValue> = san.into_vec();
                        let sline = sv.first().cloned().unwrap_or(LuaValue::Integer(1));
                        let scol = sv.get(1).cloned().unwrap_or(LuaValue::Integer(1));
                        let undo_stack: LuaValue = this.get("undo_stack")?;
                        let system: LuaTable = lua.globals().get("system")?;
                        let time: LuaValue = system.call_function("get_time", ())?;
                        this.call_method::<()>(
                            "raw_insert",
                            (sline, scol, text, undo_stack, time),
                        )?;
                        this.call_method::<()>("on_text_change", "insert")?;
                        Ok(())
                    },
                )?
            })?;

            // Doc:remove(line1, col1, line2, col2)
            doc.set("remove", {
                let rok = Arc::clone(&show_ro_key);
                lua.create_function(
                    move |lua,
                          (this, line1, col1, line2, col2): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                    )| {
                        this.call_method::<()>("ensure_loaded", ())?;
                        let read_only: bool =
                            this.get::<Option<bool>>("read_only")?.unwrap_or(false);
                        if read_only {
                            let show_ro: LuaFunction = lua.registry_value(&rok)?;
                            show_ro.call::<()>(this)?;
                            return Ok(());
                        }
                        let redo_stack = lua.create_table()?;
                        redo_stack.set("idx", 1)?;
                        this.set("redo_stack", redo_stack)?;
                        let san1: LuaMultiValue =
                            this.call_method("sanitize_position", (line1, col1))?;
                        let sv1: Vec<LuaValue> = san1.into_vec();
                        let sl1 = lua_to_i64(sv1.first().unwrap_or(&LuaValue::Integer(1)));
                        let sc1 = lua_to_i64(sv1.get(1).unwrap_or(&LuaValue::Integer(1)));
                        let san2: LuaMultiValue =
                            this.call_method("sanitize_position", (line2, col2))?;
                        let sv2: Vec<LuaValue> = san2.into_vec();
                        let sl2 = lua_to_i64(sv2.first().unwrap_or(&LuaValue::Integer(1)));
                        let sc2 = lua_to_i64(sv2.get(1).unwrap_or(&LuaValue::Integer(1)));
                        let (rl1, rc1, rl2, rc2, _) = sort_positions(sl1, sc1, sl2, sc2);
                        let undo_stack: LuaValue = this.get("undo_stack")?;
                        let system: LuaTable = lua.globals().get("system")?;
                        let time: LuaValue = system.call_function("get_time", ())?;
                        this.call_method::<()>(
                            "raw_remove",
                            (rl1, rc1, rl2, rc2, undo_stack, time),
                        )?;
                        this.call_method::<()>("on_text_change", "remove")?;
                        Ok(())
                    },
                )?
            })?;

            // Doc:undo()
            doc.set("undo", {
                let rok = Arc::clone(&show_ro_key);
                let ask = Arc::clone(&apply_snap_key);
                lua.create_function(move |lua, this: LuaTable| {
                    this.call_method::<()>("ensure_loaded", ())?;
                    let read_only: bool =
                        this.get::<Option<bool>>("read_only")?.unwrap_or(false);
                    if read_only {
                        let show_ro: LuaFunction = lua.registry_value(&rok)?;
                        show_ro.call::<()>(this)?;
                        return Ok(());
                    }
                    let doc_native: LuaTable = require_table(lua, "doc_native")?;
                    let buf_id: LuaValue = this.get("buffer_id")?;
                    let snap: LuaValue = doc_native.call_function("buffer_undo", buf_id)?;
                    let apply: LuaFunction = lua.registry_value(&ask)?;
                    apply.call::<()>((this.clone(), snap))?;
                    let hl: LuaTable = this.get("highlighter")?;
                    hl.call_method::<()>("soft_reset", ())?;
                    this.call_method::<()>("on_text_change", "undo")?;
                    Ok(())
                })?
            })?;

            // Doc:redo()
            doc.set("redo", {
                let rok = Arc::clone(&show_ro_key);
                let ask = Arc::clone(&apply_snap_key);
                lua.create_function(move |lua, this: LuaTable| {
                    this.call_method::<()>("ensure_loaded", ())?;
                    let read_only: bool =
                        this.get::<Option<bool>>("read_only")?.unwrap_or(false);
                    if read_only {
                        let show_ro: LuaFunction = lua.registry_value(&rok)?;
                        show_ro.call::<()>(this)?;
                        return Ok(());
                    }
                    let doc_native: LuaTable = require_table(lua, "doc_native")?;
                    let buf_id: LuaValue = this.get("buffer_id")?;
                    let snap: LuaValue = doc_native.call_function("buffer_redo", buf_id)?;
                    let apply: LuaFunction = lua.registry_value(&ask)?;
                    apply.call::<()>((this.clone(), snap))?;
                    let hl: LuaTable = this.get("highlighter")?;
                    hl.call_method::<()>("soft_reset", ())?;
                    this.call_method::<()>("on_text_change", "undo")?;
                    Ok(())
                })?
            })?;

            // Doc:apply_edits(edits)
            doc.set("apply_edits", {
                let rok = Arc::clone(&show_ro_key);
                let ssk = Arc::clone(&sync_sel_key);
                let aek = Arc::clone(&apply_edit_key);
                lua.create_function(
                    move |lua,
                          (this, edits): (LuaTable, LuaValue)|
                          -> LuaResult<bool> {
                        this.call_method::<()>("ensure_loaded", ())?;
                        let read_only: bool =
                            this.get::<Option<bool>>("read_only")?.unwrap_or(false);
                        if read_only {
                            let show_ro: LuaFunction = lua.registry_value(&rok)?;
                            show_ro.call::<()>(this)?;
                            return Ok(false);
                        }
                        let edits_tbl = match &edits {
                            LuaValue::Table(t) => t,
                            _ => return Ok(false),
                        };
                        if edits_tbl.raw_len() == 0 {
                            return Ok(false);
                        }
                        let redo_stack = lua.create_table()?;
                        redo_stack.set("idx", 1)?;
                        this.set("redo_stack", redo_stack)?;
                        let change_id: i64 = this.call_method("get_change_id", ())?;
                        let clean_id: i64 = this.get("clean_change_id")?;
                        if change_id < clean_id {
                            this.set("clean_change_id", -1)?;
                        }
                        let sync: LuaFunction = lua.registry_value(&ssk)?;
                        sync.call::<()>(this.clone())?;
                        let first_edit: LuaTable = edits_tbl.raw_get(1)?;
                        let doc_native: LuaTable = require_table(lua, "doc_native")?;
                        let buf_id: LuaValue = this.get("buffer_id")?;
                        let result: LuaValue = doc_native
                            .call_function("buffer_apply_edits", (buf_id, edits))?;
                        let line_hint: i64 =
                            first_edit.get::<Option<i64>>("line1")?.unwrap_or(1);
                        let undo_stack: LuaValue = this.get("undo_stack")?;
                        let system: LuaTable = lua.globals().get("system")?;
                        let time: LuaValue = system.call_function("get_time", ())?;
                        let apply_edit: LuaFunction = lua.registry_value(&aek)?;
                        let applied: bool = apply_edit.call((
                            this.clone(),
                            result,
                            undo_stack,
                            time,
                            line_hint,
                        ))?;
                        if !applied {
                            return Ok(false);
                        }
                        this.call_method::<()>("on_text_change", "insert")?;
                        Ok(true)
                    },
                )?
            })?;

            // Doc:text_input(text, idx)
            doc.set(
                "text_input",
                lua.create_function(
                    |lua, (this, text, idx): (LuaTable, LuaValue, LuaValue)| {
                        this.call_method::<()>("ensure_loaded", ())?;
                        let idx_val = if matches!(idx, LuaValue::Nil) {
                            LuaValue::Boolean(true)
                        } else {
                            idx
                        };
                        let get_sels: LuaFunction = this.get("get_selections")?;
                        let multi: LuaMultiValue = get_sels.call((
                            this.clone(),
                            LuaValue::Boolean(true),
                            idx_val,
                        ))?;
                        let vals: Vec<LuaValue> = multi.into_vec();
                        let iter_fn = match vals.first() {
                            Some(LuaValue::Function(f)) => f.clone(),
                            _ => return Ok(()),
                        };
                        let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                        let translate: LuaTable = require_table(lua, "core.doc.translate")?;
                        loop {
                            let ir: LuaMultiValue =
                                iter_fn.call((invariant.clone(), control.clone()))?;
                            let iv: Vec<LuaValue> = ir.into_vec();
                            let sidx = iv.first().cloned().unwrap_or(LuaValue::Nil);
                            if matches!(sidx, LuaValue::Nil) {
                                break;
                            }
                            control = sidx.clone();
                            let l1 = iv.get(1).cloned().unwrap_or(LuaValue::Nil);
                            let c1 = iv.get(2).cloned().unwrap_or(LuaValue::Nil);
                            let l2 = iv.get(3).cloned().unwrap_or(LuaValue::Nil);
                            let c2 = iv.get(4).cloned().unwrap_or(LuaValue::Nil);
                            let mut had_selection = false;
                            if l1 != l2 || c1 != c2 {
                                this.call_method::<()>("delete_to_cursor", sidx.clone())?;
                                had_selection = true;
                            }
                            let overwrite: bool =
                                this.get::<Option<bool>>("overwrite")?.unwrap_or(false);
                            if overwrite && !had_selection {
                                let lines: LuaTable = this.get("lines")?;
                                let l1_i = lua_to_i64(&l1);
                                let c1_i = lua_to_i64(&c1);
                                let line_str: String = lines.raw_get(l1_i)?;
                                let text_str = match &text {
                                    LuaValue::String(s) => s.to_str()?.to_string(),
                                    _ => String::new(),
                                };
                                let ulen = text_str.chars().count();
                                if c1_i < line_str.len() as i64 && ulen == 1 {
                                    let next_char: LuaMultiValue =
                                        translate.call_function(
                                            "next_char",
                                            (this.clone(), l1.clone(), c1.clone()),
                                        )?;
                                    let nc: Vec<LuaValue> = next_char.into_vec();
                                    let nl =
                                        nc.first().cloned().unwrap_or(l1.clone());
                                    let nc_val =
                                        nc.get(1).cloned().unwrap_or(c1.clone());
                                    this.call_method::<()>(
                                        "remove",
                                        (l1.clone(), c1.clone(), nl, nc_val),
                                    )?;
                                }
                            }
                            this.call_method::<()>(
                                "insert",
                                (l1, c1, text.clone()),
                            )?;
                            let text_len = match &text {
                                LuaValue::String(s) => s.as_bytes().len() as i64,
                                _ => 0,
                            };
                            this.call_method::<()>(
                                "move_to_cursor",
                                (sidx, text_len),
                            )?;
                        }
                        Ok(())
                    },
                )?,
            )?;

            // Doc:ime_text_editing(text, start, length, idx)
            doc.set(
                "ime_text_editing",
                lua.create_function(
                    |_lua,
                     (this, text, _start, _length, idx): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                    )| {
                        this.call_method::<()>("ensure_loaded", ())?;
                        let idx_val = if matches!(idx, LuaValue::Nil) {
                            LuaValue::Boolean(true)
                        } else {
                            idx
                        };
                        let get_sels: LuaFunction = this.get("get_selections")?;
                        let multi: LuaMultiValue = get_sels.call((
                            this.clone(),
                            LuaValue::Boolean(true),
                            idx_val,
                        ))?;
                        let vals: Vec<LuaValue> = multi.into_vec();
                        let iter_fn = match vals.first() {
                            Some(LuaValue::Function(f)) => f.clone(),
                            _ => return Ok(()),
                        };
                        let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                        loop {
                            let ir: LuaMultiValue =
                                iter_fn.call((invariant.clone(), control.clone()))?;
                            let iv: Vec<LuaValue> = ir.into_vec();
                            let sidx = iv.first().cloned().unwrap_or(LuaValue::Nil);
                            if matches!(sidx, LuaValue::Nil) {
                                break;
                            }
                            control = sidx.clone();
                            let l1 = iv.get(1).cloned().unwrap_or(LuaValue::Nil);
                            let c1 = iv.get(2).cloned().unwrap_or(LuaValue::Nil);
                            let l2 = iv.get(3).cloned().unwrap_or(LuaValue::Nil);
                            let c2 = iv.get(4).cloned().unwrap_or(LuaValue::Nil);
                            if l1 != l2 || c1 != c2 {
                                this.call_method::<()>(
                                    "delete_to_cursor",
                                    sidx.clone(),
                                )?;
                            }
                            this.call_method::<()>(
                                "insert",
                                (l1.clone(), c1.clone(), text.clone()),
                            )?;
                            let text_len = match &text {
                                LuaValue::String(s) => s.as_bytes().len() as i64,
                                _ => 0,
                            };
                            let c1_i = lua_to_i64(&c1);
                            this.call_method::<()>(
                                "set_selections",
                                (sidx, l1.clone(), c1_i + text_len, l1, c1),
                            )?;
                        }
                        Ok(())
                    },
                )?,
            )?;

            // Doc:replace_cursor(idx, line1, col1, line2, col2, fn)
            doc.set(
                "replace_cursor",
                lua.create_function(
                    |_lua,
                     (this, idx, line1, col1, line2, col2, func): (
                        LuaTable,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaValue,
                        LuaFunction,
                    )|
                     -> LuaResult<LuaValue> {
                        let old_text: String = this.call_method(
                            "get_text",
                            (
                                line1.clone(),
                                col1.clone(),
                                line2.clone(),
                                col2.clone(),
                            ),
                        )?;
                        let result: LuaMultiValue = func.call(old_text.clone())?;
                        let rv: Vec<LuaValue> = result.into_vec();
                        let new_text_v =
                            rv.first().cloned().unwrap_or(LuaValue::Nil);
                        let res = rv.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let new_text = match &new_text_v {
                            LuaValue::String(s) => s.to_str()?.to_string(),
                            _ => old_text.clone(),
                        };
                        if old_text != new_text {
                            this.call_method::<()>(
                                "insert",
                                (line2.clone(), col2.clone(), new_text_v),
                            )?;
                            this.call_method::<()>(
                                "remove",
                                (
                                    line1.clone(),
                                    col1.clone(),
                                    line2.clone(),
                                    col2.clone(),
                                ),
                            )?;
                            let l1_i = lua_to_i64(&line1);
                            let c1_i = lua_to_i64(&col1);
                            let l2_i = lua_to_i64(&line2);
                            let c2_i = lua_to_i64(&col2);
                            if l1_i == l2_i && c1_i == c2_i {
                                let offset: LuaMultiValue = this.call_method(
                                    "position_offset",
                                    (l1_i, c1_i, new_text.len() as i64),
                                )?;
                                let ov: Vec<LuaValue> = offset.into_vec();
                                let nl2 =
                                    ov.first().cloned().unwrap_or(LuaValue::Nil);
                                let nc2 =
                                    ov.get(1).cloned().unwrap_or(LuaValue::Nil);
                                this.call_method::<()>(
                                    "set_selections",
                                    (idx, line1, col1, nl2, nc2),
                                )?;
                            }
                        }
                        Ok(res)
                    },
                )?,
            )?;

            // Doc:replace(fn)
            doc.set(
                "replace",
                lua.create_function(|lua, (this, func): (LuaTable, LuaFunction)| {
                    this.call_method::<()>("ensure_loaded", ())?;
                    let mut has_selection = false;
                    let results = lua.create_table()?;
                    let get_sels: LuaFunction = this.get("get_selections")?;
                    let multi: LuaMultiValue =
                        get_sels.call((this.clone(), LuaValue::Boolean(true)))?;
                    let vals: Vec<LuaValue> = multi.into_vec();
                    let iter_fn = match vals.first() {
                        Some(LuaValue::Function(f)) => f.clone(),
                        _ => return Ok(LuaValue::Table(results)),
                    };
                    let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                    let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                    loop {
                        let ir: LuaMultiValue =
                            iter_fn.call((invariant.clone(), control.clone()))?;
                        let iv: Vec<LuaValue> = ir.into_vec();
                        let idx_val = iv.first().cloned().unwrap_or(LuaValue::Nil);
                        if matches!(idx_val, LuaValue::Nil) {
                            break;
                        }
                        control = idx_val.clone();
                        let l1 = iv.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let c1 = iv.get(2).cloned().unwrap_or(LuaValue::Nil);
                        let l2 = iv.get(3).cloned().unwrap_or(LuaValue::Nil);
                        let c2 = iv.get(4).cloned().unwrap_or(LuaValue::Nil);
                        if l1 != l2 || c1 != c2 {
                            let res: LuaValue = this.call_method(
                                "replace_cursor",
                                (idx_val.clone(), l1, c1, l2, c2, func.clone()),
                            )?;
                            results.set(idx_val, res)?;
                            has_selection = true;
                        }
                    }
                    if !has_selection {
                        let sels: LuaTable = this.get("selections")?;
                        let table_unpack: LuaFunction =
                            lua.globals().get::<LuaTable>("table")?.get("unpack")?;
                        let unpacked: LuaMultiValue = table_unpack.call(sels)?;
                        this.call_method::<()>("set_selection", unpacked)?;
                        let lines: LuaTable = this.get("lines")?;
                        let nlines = lines.raw_len() as i64;
                        let last_line: String = lines.raw_get(nlines)?;
                        let res: LuaValue = this.call_method(
                            "replace_cursor",
                            (1, 1, 1, nlines, last_line.len() as i64, func),
                        )?;
                        results.raw_set(1, res)?;
                    }
                    Ok(LuaValue::Table(results))
                })?,
            )?;

            // Doc:delete_to_cursor(idx, ...)
            doc.set(
                "delete_to_cursor",
                lua.create_function(
                    |_lua,
                     (this, idx, args): (LuaTable, LuaValue, LuaMultiValue)| {
                        let idx_val = idx.clone();
                        let get_sels: LuaFunction = this.get("get_selections")?;
                        let multi: LuaMultiValue = get_sels.call((
                            this.clone(),
                            LuaValue::Boolean(true),
                            idx_val.clone(),
                        ))?;
                        let vals: Vec<LuaValue> = multi.into_vec();
                        let iter_fn = match vals.first() {
                            Some(LuaValue::Function(f)) => f.clone(),
                            _ => return Ok(()),
                        };
                        let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                        let extra_args = args.into_vec();
                        loop {
                            let ir: LuaMultiValue =
                                iter_fn.call((invariant.clone(), control.clone()))?;
                            let iv: Vec<LuaValue> = ir.into_vec();
                            let sidx = iv.first().cloned().unwrap_or(LuaValue::Nil);
                            if matches!(sidx, LuaValue::Nil) {
                                break;
                            }
                            control = sidx.clone();
                            let l1 = iv.get(1).cloned().unwrap_or(LuaValue::Nil);
                            let c1 = iv.get(2).cloned().unwrap_or(LuaValue::Nil);
                            let l2 = iv.get(3).cloned().unwrap_or(LuaValue::Nil);
                            let c2 = iv.get(4).cloned().unwrap_or(LuaValue::Nil);
                            if l1 != l2 || c1 != c2 {
                                this.call_method::<()>(
                                    "remove",
                                    (l1.clone(), c1.clone(), l2, c2),
                                )?;
                            } else {
                                let mut offset_args = vec![
                                    LuaValue::Table(this.clone()),
                                    l1.clone(),
                                    c1.clone(),
                                ];
                                offset_args.extend(extra_args.clone());
                                let pos_offset: LuaFunction =
                                    this.get("position_offset")?;
                                let result: LuaMultiValue = pos_offset
                                    .call(LuaMultiValue::from_vec(offset_args))?;
                                let rv: Vec<LuaValue> = result.into_vec();
                                let nl2 =
                                    rv.first().cloned().unwrap_or(LuaValue::Nil);
                                let nc2 =
                                    rv.get(1).cloned().unwrap_or(LuaValue::Nil);
                                this.call_method::<()>(
                                    "remove",
                                    (l1.clone(), c1.clone(), nl2.clone(), nc2.clone()),
                                )?;
                                let (sl1, sc1, _, _, _) = sort_positions(
                                    lua_to_i64(&l1),
                                    lua_to_i64(&c1),
                                    lua_to_i64(&nl2),
                                    lua_to_i64(&nc2),
                                );
                                this.call_method::<()>(
                                    "set_selections",
                                    (sidx, sl1, sc1),
                                )?;
                                continue;
                            }
                            this.call_method::<()>(
                                "set_selections",
                                (sidx, l1, c1),
                            )?;
                        }
                        this.call_method::<()>("merge_cursors", idx)?;
                        Ok(())
                    },
                )?,
            )?;

            // Doc:delete_to(...)
            doc.set(
                "delete_to",
                lua.create_function(
                    |_lua, (this, args): (LuaTable, LuaMultiValue)| {
                        let mut call_args =
                            vec![LuaValue::Table(this.clone()), LuaValue::Nil];
                        call_args.extend(args.into_vec());
                        let dtc: LuaFunction = this.get("delete_to_cursor")?;
                        dtc.call::<()>(LuaMultiValue::from_vec(call_args))
                    },
                )?,
            )?;

            // Doc:move_to_cursor(idx, ...)
            doc.set(
                "move_to_cursor",
                lua.create_function(
                    |_lua,
                     (this, idx, args): (LuaTable, LuaValue, LuaMultiValue)| {
                        let get_sels: LuaFunction = this.get("get_selections")?;
                        let multi: LuaMultiValue = get_sels.call((
                            this.clone(),
                            LuaValue::Boolean(false),
                            idx.clone(),
                        ))?;
                        let vals: Vec<LuaValue> = multi.into_vec();
                        let iter_fn = match vals.first() {
                            Some(LuaValue::Function(f)) => f.clone(),
                            _ => return Ok(()),
                        };
                        let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                        let extra_args = args.into_vec();
                        loop {
                            let ir: LuaMultiValue =
                                iter_fn.call((invariant.clone(), control.clone()))?;
                            let iv: Vec<LuaValue> = ir.into_vec();
                            let sidx = iv.first().cloned().unwrap_or(LuaValue::Nil);
                            if matches!(sidx, LuaValue::Nil) {
                                break;
                            }
                            control = sidx.clone();
                            let line = iv.get(1).cloned().unwrap_or(LuaValue::Nil);
                            let col = iv.get(2).cloned().unwrap_or(LuaValue::Nil);
                            let mut offset_args =
                                vec![LuaValue::Table(this.clone()), line, col];
                            offset_args.extend(extra_args.clone());
                            let pos_offset: LuaFunction =
                                this.get("position_offset")?;
                            let result: LuaMultiValue = pos_offset
                                .call(LuaMultiValue::from_vec(offset_args))?;
                            let rv: Vec<LuaValue> = result.into_vec();
                            let mut set_args = vec![sidx];
                            set_args.extend(rv);
                            let set_sels: LuaFunction =
                                this.get("set_selections")?;
                            set_sels.call::<()>((
                                this.clone(),
                                LuaMultiValue::from_vec(set_args),
                            ))?;
                        }
                        this.call_method::<()>("merge_cursors", idx)?;
                        Ok(())
                    },
                )?,
            )?;

            // Doc:move_to(...)
            doc.set(
                "move_to",
                lua.create_function(
                    |_lua, (this, args): (LuaTable, LuaMultiValue)| {
                        let mut call_args =
                            vec![LuaValue::Table(this.clone()), LuaValue::Nil];
                        call_args.extend(args.into_vec());
                        let mtc: LuaFunction = this.get("move_to_cursor")?;
                        mtc.call::<()>(LuaMultiValue::from_vec(call_args))
                    },
                )?,
            )?;

            // Doc:select_to_cursor(idx, ...)
            doc.set(
                "select_to_cursor",
                lua.create_function(
                    |_lua,
                     (this, idx, args): (LuaTable, LuaValue, LuaMultiValue)| {
                        let get_sels: LuaFunction = this.get("get_selections")?;
                        let multi: LuaMultiValue = get_sels.call((
                            this.clone(),
                            LuaValue::Boolean(false),
                            idx.clone(),
                        ))?;
                        let vals: Vec<LuaValue> = multi.into_vec();
                        let iter_fn = match vals.first() {
                            Some(LuaValue::Function(f)) => f.clone(),
                            _ => return Ok(()),
                        };
                        let invariant = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let mut control = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                        let extra_args = args.into_vec();
                        loop {
                            let ir: LuaMultiValue =
                                iter_fn.call((invariant.clone(), control.clone()))?;
                            let iv: Vec<LuaValue> = ir.into_vec();
                            let sidx = iv.first().cloned().unwrap_or(LuaValue::Nil);
                            if matches!(sidx, LuaValue::Nil) {
                                break;
                            }
                            control = sidx.clone();
                            let line = iv.get(1).cloned().unwrap_or(LuaValue::Nil);
                            let col = iv.get(2).cloned().unwrap_or(LuaValue::Nil);
                            let line2 = iv.get(3).cloned().unwrap_or(LuaValue::Nil);
                            let col2 = iv.get(4).cloned().unwrap_or(LuaValue::Nil);
                            let mut offset_args =
                                vec![LuaValue::Table(this.clone()), line, col];
                            offset_args.extend(extra_args.clone());
                            let pos_offset: LuaFunction =
                                this.get("position_offset")?;
                            let result: LuaMultiValue = pos_offset
                                .call(LuaMultiValue::from_vec(offset_args))?;
                            let rv: Vec<LuaValue> = result.into_vec();
                            let nline =
                                rv.first().cloned().unwrap_or(LuaValue::Nil);
                            let ncol =
                                rv.get(1).cloned().unwrap_or(LuaValue::Nil);
                            this.call_method::<()>(
                                "set_selections",
                                (sidx, nline, ncol, line2, col2),
                            )?;
                        }
                        this.call_method::<()>("merge_cursors", idx)?;
                        Ok(())
                    },
                )?,
            )?;

            // Doc:select_to(...)
            doc.set(
                "select_to",
                lua.create_function(
                    |_lua, (this, args): (LuaTable, LuaMultiValue)| {
                        let mut call_args =
                            vec![LuaValue::Table(this.clone()), LuaValue::Nil];
                        call_args.extend(args.into_vec());
                        let stc: LuaFunction = this.get("select_to_cursor")?;
                        stc.call::<()>(LuaMultiValue::from_vec(call_args))
                    },
                )?,
            )?;

            // Doc:get_indent_string()
            doc.set(
                "get_indent_string",
                lua.create_function(|lua, this: LuaTable| {
                    let result: LuaMultiValue =
                        this.call_method("get_indent_info", ())?;
                    let rv: Vec<LuaValue> = result.into_vec();
                    let indent_type = match rv.first() {
                        Some(LuaValue::String(s)) => s.to_str()?.to_string(),
                        _ => "soft".to_string(),
                    };
                    let indent_size = match rv.get(1) {
                        Some(LuaValue::Integer(n)) => *n,
                        Some(LuaValue::Number(n)) => *n as i64,
                        _ => 2,
                    };
                    if indent_type == "hard" {
                        Ok("\t".to_string())
                    } else {
                        let string_rep: LuaFunction = lua
                            .globals()
                            .get::<LuaTable>("string")?
                            .get("rep")?;
                        string_rep.call((" ", indent_size))
                    }
                })?,
            )?;

            // Doc:get_line_indent(line, rnd_up)
            doc.set(
                "get_line_indent",
                lua.create_function(
                    |lua,
                     (this, line, rnd_up): (LuaTable, LuaValue, LuaValue)|
                     -> LuaResult<LuaMultiValue> {
                        let line_str = match &line {
                            LuaValue::String(s) => s.to_str()?.to_string(),
                            _ => String::new(),
                        };
                        let string_find: LuaFunction = lua
                            .globals()
                            .get::<LuaTable>("string")?
                            .get("find")?;
                        let find_result: LuaMultiValue =
                            string_find.call((line_str.clone(), "^[ \t]+"))?;
                        let fv: Vec<LuaValue> = find_result.into_vec();
                        let e: LuaValue =
                            fv.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let indent_result: LuaMultiValue =
                            this.call_method("get_indent_info", ())?;
                        let ir: Vec<LuaValue> = indent_result.into_vec();
                        let indent_type = match ir.first() {
                            Some(LuaValue::String(s)) => {
                                s.to_str()?.to_string()
                            }
                            _ => "soft".to_string(),
                        };
                        let indent_size = match ir.get(1) {
                            Some(LuaValue::Integer(n)) => *n,
                            Some(LuaValue::Number(n)) => *n as i64,
                            _ => 2,
                        };
                        let string_rep: LuaFunction = lua
                            .globals()
                            .get::<LuaTable>("string")?
                            .get("rep")?;
                        let string_sub: LuaFunction = lua
                            .globals()
                            .get::<LuaTable>("string")?
                            .get("sub")?;
                        let string_gsub: LuaFunction = lua
                            .globals()
                            .get::<LuaTable>("string")?
                            .get("gsub")?;
                        let soft_tab: String =
                            string_rep.call((" ", indent_size))?;
                        let is_rnd_up = matches!(rnd_up, LuaValue::Boolean(true));
                        if indent_type == "hard" {
                            let indent: String = if !matches!(e, LuaValue::Nil) {
                                let e_i = lua_to_i64(&e);
                                let sub: String = string_sub
                                    .call((line_str, 1, e_i))?;
                                string_gsub
                                    .call::<(String, i64)>((
                                        sub,
                                        soft_tab.clone(),
                                        "\t",
                                    ))?
                                    .0
                            } else {
                                String::new()
                            };
                            let result: String = if is_rnd_up {
                                string_gsub
                                    .call::<(String, i64)>((indent, " +", "\t"))?
                                    .0
                            } else {
                                string_gsub
                                    .call::<(String, i64)>((indent, " +", ""))?
                                    .0
                            };
                            Ok(LuaMultiValue::from_vec(vec![
                                e,
                                LuaValue::String(lua.create_string(&result)?),
                            ]))
                        } else {
                            let indent: String = if !matches!(e, LuaValue::Nil) {
                                let e_i = lua_to_i64(&e);
                                let sub: String = string_sub
                                    .call((line_str, 1, e_i))?;
                                string_gsub
                                    .call::<(String, i64)>((
                                        sub,
                                        "\t",
                                        soft_tab.clone(),
                                    ))?
                                    .0
                            } else {
                                String::new()
                            };
                            let number = indent.len() as f64
                                / soft_tab.len().max(1) as f64;
                            let rounded = if is_rnd_up {
                                number.ceil() as usize
                            } else {
                                number.floor() as usize
                            };
                            let result: String = string_sub.call((
                                indent,
                                1,
                                (rounded * soft_tab.len()) as i64,
                            ))?;
                            Ok(LuaMultiValue::from_vec(vec![
                                e,
                                LuaValue::String(lua.create_string(&result)?),
                            ]))
                        }
                    },
                )?,
            )?;

            // Doc:indent_text(unindent, line1, col1, line2, col2)
            doc.set(
                "indent_text",
                lua.create_function(
                    |lua,
                     (this, unindent, line1, col1, line2, col2): (
                        LuaTable,
                        LuaValue,
                        i64,
                        i64,
                        i64,
                        i64,
                    )|
                     -> LuaResult<LuaMultiValue> {
                        let text: String =
                            this.call_method("get_indent_string", ())?;
                        let lines: LuaTable = this.get("lines")?;
                        let first_line: String = lines.raw_get(line1)?;
                        let string_find: LuaFunction = lua
                            .globals()
                            .get::<LuaTable>("string")?
                            .get("find")?;
                        let find_result: LuaMultiValue =
                            string_find.call((first_line, "^[ \t]+"))?;
                        let fv: Vec<LuaValue> = find_result.into_vec();
                        let se: LuaValue =
                            fv.get(1).cloned().unwrap_or(LuaValue::Nil);
                        let se_i = if matches!(se, LuaValue::Nil) {
                            None
                        } else {
                            Some(lua_to_i64(&se))
                        };
                        let in_beginning_whitespace =
                            col1 == 1 || se_i.is_some_and(|s| col1 <= s + 1);
                        let has_selection = line1 != line2 || col1 != col2;
                        let do_unindent =
                            matches!(unindent, LuaValue::Boolean(true));
                        if do_unindent || has_selection || in_beginning_whitespace
                        {
                            let lines: LuaTable = this.get("lines")?;
                            let l1d_before: String = lines.raw_get(line1)?;
                            let l2d_before: String = lines.raw_get(line2)?;
                            let l1d_len = l1d_before.len() as i64;
                            let l2d_len = l2d_before.len() as i64;
                            for line in line1..=line2 {
                                let lines: LuaTable = this.get("lines")?;
                                let line_str: String = lines.raw_get(line)?;
                                if has_selection && line_str.len() <= 1 {
                                    continue;
                                }
                                let indent_result: LuaMultiValue =
                                    this.call_method(
                                        "get_line_indent",
                                        (
                                            LuaValue::String(
                                                lua.create_string(&line_str)?,
                                            ),
                                            LuaValue::Boolean(do_unindent),
                                        ),
                                    )?;
                                let ir: Vec<LuaValue> =
                                    indent_result.into_vec();
                                let e_val = ir
                                    .first()
                                    .cloned()
                                    .unwrap_or(LuaValue::Integer(0));
                                let rnded = match ir.get(1) {
                                    Some(LuaValue::String(s)) => {
                                        s.to_str()?.to_string()
                                    }
                                    _ => String::new(),
                                };
                                let e_i = match &e_val {
                                    LuaValue::Nil => 0,
                                    _ => lua_to_i64(&e_val),
                                };
                                this.call_method::<()>(
                                    "remove",
                                    (line, 1, line, e_i + 1),
                                )?;
                                let insert_text = if do_unindent {
                                    let end = (rnded.len() as i64
                                        - text.len() as i64)
                                        .max(0)
                                        as usize;
                                    rnded[..end].to_string()
                                } else {
                                    format!("{rnded}{text}")
                                };
                                this.call_method::<()>(
                                    "insert",
                                    (line, 1, insert_text),
                                )?;
                            }
                            let lines: LuaTable = this.get("lines")?;
                            let l1d_after: String = lines.raw_get(line1)?;
                            let l2d_after: String = lines.raw_get(line2)?;
                            let l1d = l1d_after.len() as i64 - l1d_len;
                            let l2d = l2d_after.len() as i64 - l2d_len;
                            if (do_unindent || in_beginning_whitespace)
                                && !has_selection
                            {
                                let start_cursor = se_i.unwrap_or(0) + 1 + l1d;
                                return Ok(LuaMultiValue::from_vec(vec![
                                    LuaValue::Integer(line1),
                                    LuaValue::Integer(start_cursor),
                                    LuaValue::Integer(line2),
                                    LuaValue::Integer(start_cursor),
                                ]));
                            }
                            return Ok(LuaMultiValue::from_vec(vec![
                                LuaValue::Integer(line1),
                                LuaValue::Integer(col1 + l1d),
                                LuaValue::Integer(line2),
                                LuaValue::Integer(col2 + l2d),
                            ]));
                        }
                        this.call_method::<()>(
                            "insert",
                            (line1, col1, text.clone()),
                        )?;
                        let new_col = col1 + text.len() as i64;
                        Ok(LuaMultiValue::from_vec(vec![
                            LuaValue::Integer(line1),
                            LuaValue::Integer(new_col),
                            LuaValue::Integer(line1),
                            LuaValue::Integer(new_col),
                        ]))
                    },
                )?,
            )?;

            // Doc:on_text_change(type)
            doc.set(
                "on_text_change",
                lua.create_function(
                    |_lua, (_this, _type): (LuaTable, LuaValue)| Ok(()),
                )?,
            )?;

            // Doc:on_close()
            doc.set(
                "on_close",
                lua.create_function(|lua, this: LuaTable| {
                    let buf_id: LuaValue = this.get("buffer_id")?;
                    if !matches!(buf_id, LuaValue::Nil) {
                        let doc_native: LuaTable =
                            require_table(lua, "doc_native")?;
                        doc_native.call_function::<()>("buffer_free", buf_id)?;
                        this.set("buffer_id", LuaValue::Nil)?;
                    }
                    let core: LuaTable = require_table(lua, "core")?;
                    let name: String = this.call_method("get_name", ())?;
                    core.call_function::<()>(
                        "log_quiet",
                        (format!("Closed doc \"{name}\""),),
                    )?;
                    Ok(())
                })?,
            )?;

            let _ = &class_key;
            Ok(LuaValue::Table(doc))
        })?,
    )
}
