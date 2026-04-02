use mlua::prelude::*;

/// Registers `core` as a native Rust preload, replacing `data/core/core.lua`.
pub fn register_builtin_preloads(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    preload.set("core", lua.create_function(build_core_module)?)?;
    Ok(())
}

fn build_core_module(lua: &Lua, _: ()) -> LuaResult<LuaValue> {
    let require: LuaFunction = lua.globals().get("require")?;

    require.call::<()>("core.strict")?;
    require.call::<()>("core.regex")?;

    let core = lua.create_table()?;

    // Module-local mutable state stored in a registry table.
    let state = lua.create_table()?;
    state.set("thread_counter", 0i64)?;
    state.set("temp_file_counter", 0i64)?;
    state.set("last_file_dialog_tag", 0i64)?;
    state.set("mod_version_regex", LuaValue::Nil)?;
    state.set("priority_regex", LuaValue::Nil)?;
    state.set("alerted_deprecations", lua.create_table()?)?;
    let state_key = lua.create_registry_value(state.clone())?;
    lua.set_named_registry_value("core._state", state)?;

    // Compute temp_file_prefix from system.get_time().
    let system: LuaTable = lua.globals().get("system")?;
    let get_time: LuaFunction = system.get("get_time")?;
    let time_val: f64 = get_time.call(())?;
    let temp_uid = (time_val * 1000.0) as u64 % 0xFFFF_FFFF;
    let temp_file_prefix = format!(".lite_temp_{:08x}", temp_uid);
    let prefix_key = lua.create_registry_value(temp_file_prefix)?;

    // Lazy-command infrastructure stored in registry.
    let lazy_command_plugins = lua.create_table()?;
    register_lazy_command_plugin_defs(lua, &lazy_command_plugins)?;
    let lazy_plugins_key = lua.create_registry_value(lazy_command_plugins)?;
    let lazy_handlers = lua.create_table()?;
    let lazy_handlers_key = lua.create_registry_value(lazy_handlers)?;
    let lazy_loaded = lua.create_table()?;
    let lazy_loaded_key = lua.create_registry_value(lazy_loaded)?;

    // Set core.plugin_list = {}
    core.set("plugin_list", lua.create_table()?)?;

    register_local_helpers(lua, &core, &state_key, &prefix_key)?;
    register_project_fns(lua, &core, &state_key)?;
    register_init_fn(
        lua,
        &core,
        &state_key,
        &lazy_plugins_key,
        &lazy_handlers_key,
        &lazy_loaded_key,
    )?;
    register_plugin_loader(
        lua,
        &core,
        &state_key,
        &lazy_plugins_key,
        &lazy_handlers_key,
        &lazy_loaded_key,
    )?;
    register_logging(lua, &core, &state_key)?;
    register_doc_fns(lua, &core)?;
    register_view_fns(lua, &core)?;
    register_event_handling(lua, &core)?;
    register_thread_scheduler(lua, &core, &state_key)?;
    register_run_loop(lua, &core)?;
    register_misc(lua, &core, &state_key, &prefix_key)?;

    // Store core in package.loaded and as a global.
    let package: LuaTable = lua.globals().get("package")?;
    let loaded: LuaTable = package.get("loaded")?;
    loaded.set("core", core.clone())?;
    // Register with strict so `core` is accessible as a global.
    let global_fn: LuaValue = lua.globals().raw_get("global")?;
    if let LuaValue::Function(gf) = global_fn {
        let t = lua.create_table()?;
        t.set("core", core.clone())?;
        gf.call::<()>(t)?;
    }

    // Load the default color theme (applies colors to the style table).
    require.call::<LuaValue>("colors.default")?;

    Ok(LuaValue::Table(core))
}

/// Helper to get the `core` table from package.loaded.
fn get_core(lua: &Lua) -> LuaResult<LuaTable> {
    lua.globals()
        .get::<LuaTable>("package")?
        .get::<LuaTable>("loaded")?
        .get("core")
}

/// Helper to get a required module from package.loaded.
fn get_module(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    lua.globals()
        .get::<LuaTable>("package")?
        .get::<LuaTable>("loaded")?
        .get(name)
}

/// Adds a tick-mode thread to core.threads. The tick function is called each
/// scheduler cycle; it returns a sleep time (number) to reschedule, or nil to finish.
fn add_tick_thread(lua: &Lua, tick_fn: LuaFunction) -> LuaResult<()> {
    let state: LuaTable = lua.named_registry_value("core._state")?;
    let counter: i64 = state.get("thread_counter")?;
    let new_counter = counter + 1;
    state.set("thread_counter", new_counter)?;
    let core = get_core(lua)?;
    let threads: LuaTable = core.get("threads")?;
    let thread_entry = lua.create_table()?;
    thread_entry.set("tick", tick_fn)?;
    thread_entry.set("wake", 0)?;
    threads.set(new_counter, thread_entry)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Local helper functions (close_unreferenced_docs, get_user_init_filename, etc.)
// ---------------------------------------------------------------------------

fn register_local_helpers(
    lua: &Lua,
    core: &LuaTable,
    state_key: &LuaRegistryKey,
    prefix_key: &LuaRegistryKey,
) -> LuaResult<()> {
    // close_unreferenced_docs — module-local, exposed as core._close_unreferenced_docs
    core.set(
        "_close_unreferenced_docs",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let docs: LuaTable = core.get("docs")?;
            let get_views: LuaFunction = core.get("get_views_referencing_doc")?;
            let len = docs.raw_len();
            let mut i = len;
            while i >= 1 {
                let doc: LuaTable = docs.get(i)?;
                let views: LuaTable = get_views.call(doc.clone())?;
                if views.raw_len() == 0 {
                    let table_mod: LuaTable = lua.globals().get("table")?;
                    let remove: LuaFunction = table_mod.get("remove")?;
                    remove.call::<()>((docs.clone(), i))?;
                    let on_close: LuaFunction = doc.get("on_close")?;
                    on_close.call::<()>(doc)?;
                }
                i -= 1;
            }
            Ok(())
        })?,
    )?;

    // _get_user_init_filename
    core.set(
        "_get_user_init_filename",
        lua.create_function(|lua, ()| {
            let userdir: String = lua.globals().get("USERDIR")?;
            let pathsep: String = lua.globals().get("PATHSEP")?;
            Ok(format!("{}{}{}", userdir, pathsep, "init.lua"))
        })?,
    )?;

    // _get_user_config_filename
    core.set(
        "_get_user_config_filename",
        lua.create_function(|lua, ()| {
            let userdir: String = lua.globals().get("USERDIR")?;
            let pathsep: String = lua.globals().get("PATHSEP")?;
            Ok(format!("{}{}{}", userdir, pathsep, "config.lua"))
        })?,
    )?;

    // _load_session
    core.set(
        "_load_session",
        lua.create_function(|lua, ()| {
            let session_native: LuaTable = get_module(lua, "session_native")?;
            let load_fn: LuaFunction = session_native.get("load")?;
            let session: LuaTable = load_fn.call(())?;
            let legacy_path: LuaValue = session.get("legacy_path")?;
            if let LuaValue::String(path) = legacy_path {
                let path_str = path.to_str()?.to_string();
                let system: LuaTable = lua.globals().get("system")?;
                let get_file_info: LuaFunction = system.get("get_file_info")?;
                let info: LuaValue = get_file_info.call(path_str.clone())?;
                if info != LuaValue::Nil {
                    let pcall: LuaFunction = lua.globals().get("pcall")?;
                    let dofile: LuaFunction = lua.globals().get("dofile")?;
                    let result: LuaMultiValue = pcall.call((dofile, path_str))?;
                    let mut vals: Vec<LuaValue> = result.into_vec();
                    let ok = vals
                        .first()
                        .and_then(|v| {
                            if let LuaValue::Boolean(b) = v {
                                Some(*b)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(false);
                    if ok && vals.len() >= 2 {
                        if let LuaValue::Table(legacy) = vals.remove(1) {
                            let save_fn: LuaFunction = session_native.get("save")?;
                            let pcall2: LuaFunction = lua.globals().get("pcall")?;
                            pcall2.call::<()>((save_fn, legacy.clone()))?;
                            return Ok(LuaValue::Table(legacy));
                        }
                    }
                }
            }
            Ok(LuaValue::Table(session))
        })?,
    )?;

    // _save_session
    core.set(
        "_save_session",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let config: LuaTable = get_module(lua, "core.config")?;
            let session_native: LuaTable = get_module(lua, "session_native")?;

            // treeview_size
            let mut treeview_size = LuaValue::Nil;
            let plugins: LuaValue = config.get("plugins")?;
            if let LuaValue::Table(ref plugins_t) = plugins {
                let tv: LuaValue = plugins_t.get("treeview")?;
                if let LuaValue::Table(ref tv_t) = tv {
                    let sz: LuaValue = tv_t.get("size")?;
                    if matches!(sz, LuaValue::Number(_) | LuaValue::Integer(_)) {
                        treeview_size = sz;
                    }
                }
            }

            // open_files + persistent undo for clean files
            let open_files = lua.create_table()?;
            let open_files_undo = lua.create_table()?;
            // Collect undo data in memory; files are written after backup dir is reset.
            let mut pending_session_undo: Vec<(String, Vec<u8>)> = Vec::new();
            let skip: bool = core
                .get::<LuaValue>("skip_session_open_files")
                .map(|v| matches!(v, LuaValue::Boolean(true)))
                .unwrap_or(false);
            if !skip {
                let seen = lua.create_table()?;
                let docs: LuaTable = core.get("docs")?;
                let doc_native_of: LuaTable = get_module(lua, "doc_native")?;
                for pair in docs.sequence_values::<LuaTable>() {
                    let doc = pair?;
                    let abs: LuaValue = doc.get("abs_filename")?;
                    if let LuaValue::String(ref s) = abs {
                        let key_str = s.to_str()?.to_string();
                        let already: LuaValue = seen.get(key_str.as_str())?;
                        if already == LuaValue::Nil {
                            seen.set(key_str.as_str(), true)?;
                            let len = open_files.raw_len();
                            open_files.set(len + 1, abs.clone())?;
                            // Collect undo data for clean files (dirty ones handled below).
                            let dirty: bool = doc.call_method("is_dirty", ())?;
                            let new_file: bool =
                                doc.get::<Option<bool>>("new_file")?.unwrap_or(false);
                            if !dirty && !new_file {
                                let buf_id: LuaValue = doc.get("buffer_id")?;
                                if !matches!(buf_id, LuaValue::Nil) {
                                    let get_undo: LuaFunction =
                                        doc_native_of.get("buffer_get_undo_data")?;
                                    let undo_data: Vec<u8> = get_undo.call(buf_id)?;
                                    if undo_data.len() > 8 {
                                        pending_session_undo
                                            .push((key_str.clone(), undo_data));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Save backups of dirty/unsaved docs.
            {
                let userdir: String = lua.globals().get("USERDIR")?;
                let backup_dir = std::path::PathBuf::from(&userdir).join("backups");
                // Remove stale backups before writing new ones.
                let _ = std::fs::remove_dir_all(&backup_dir);
                let _ = std::fs::create_dir_all(&backup_dir);
                let doc_native: LuaTable = get_module(lua, "doc_native")?;
                let docs: LuaTable = core.get("docs")?;
                let mut manifest = Vec::<serde_json::Value>::new();
                let mut idx: u32 = 0;
                for pair in docs.sequence_values::<LuaTable>() {
                    let doc = pair?;
                    let new_file: bool = doc.get::<Option<bool>>("new_file")?.unwrap_or(false);
                    let dirty: bool = doc.call_method("is_dirty", ())?;
                    if !dirty && !new_file {
                        continue;
                    }
                    // Ensure deferred content is loaded before saving.
                    doc.call_method::<()>("ensure_loaded", ())?;
                    let buf_id: LuaValue = doc.get("buffer_id")?;
                    if matches!(buf_id, LuaValue::Nil) {
                        continue;
                    }
                    let backup_name = format!("backup_{idx}.txt");
                    let backup_path = backup_dir.join(&backup_name);
                    let backup_str = backup_path.to_string_lossy().to_string();
                    let crlf: bool = doc.get::<Option<bool>>("crlf")?.unwrap_or(false);
                    let save_fn: LuaFunction = doc_native.get("buffer_save")?;
                    let ok: LuaValue = save_fn.call((buf_id.clone(), backup_str.clone(), crlf))?;
                    if !matches!(ok, LuaValue::Boolean(true)) {
                        continue;
                    }
                    let filename: LuaValue = doc.get("filename")?;
                    let abs_filename: LuaValue = doc.get("abs_filename")?;
                    let selections: LuaValue = doc.get("selections")?;
                    let mut sel_vec = Vec::new();
                    if let LuaValue::Table(ref sel_t) = selections {
                        for v in sel_t.sequence_values::<LuaValue>() {
                            match v? {
                                LuaValue::Integer(i) => {
                                    sel_vec.push(serde_json::Value::from(i));
                                }
                                LuaValue::Number(n) => {
                                    sel_vec.push(serde_json::json!(n));
                                }
                                _ => {}
                            }
                        }
                    }
                    // Save undo data alongside backup
                    let doc_native: LuaTable = get_module(lua, "doc_native")?;
                    let get_undo: LuaFunction = doc_native.get("buffer_get_undo_data")?;
                    let undo_data: Vec<u8> = get_undo.call(buf_id.clone())?;
                    let undo_path = backup_dir.join(format!("backup_{idx}.undo"));
                    let _ = std::fs::write(&undo_path, &undo_data);
                    let undo_str = undo_path.to_string_lossy().to_string();
                    let entry = serde_json::json!({
                        "filename": match &filename {
                            LuaValue::String(s) => {
                                serde_json::Value::String(s.to_str()?.to_string())
                            }
                            _ => serde_json::Value::Null,
                        },
                        "abs_filename": match &abs_filename {
                            LuaValue::String(s) => {
                                serde_json::Value::String(s.to_str()?.to_string())
                            }
                            _ => serde_json::Value::Null,
                        },
                        "new_file": new_file,
                        "backup_path": backup_str,
                        "undo_path": undo_str,
                        "selections": sel_vec,
                        "crlf": crlf,
                    });
                    manifest.push(entry);
                    idx += 1;
                }
                if !manifest.is_empty() {
                    let manifest_path = backup_dir.join("manifest.json");
                    let content = serde_json::to_string_pretty(&serde_json::Value::Array(manifest))
                        .unwrap_or_default();
                    let _ = std::fs::write(&manifest_path, content);
                }
            }

            // Write session undo files (after backup dir was reset above).
            if !pending_session_undo.is_empty() {
                let userdir_su: String = lua.globals().get("USERDIR")?;
                let undo_dir = std::path::PathBuf::from(&userdir_su).join("backups");
                let _ = std::fs::create_dir_all(&undo_dir);
                for (idx, (abs_path, undo_data)) in pending_session_undo.iter().enumerate() {
                    let undo_path = undo_dir.join(format!("session_{idx}.undo"));
                    if std::fs::write(&undo_path, undo_data).is_ok() {
                        let undo_str = undo_path.to_string_lossy().to_string();
                        open_files_undo.set(abs_path.as_str(), undo_str)?;
                    }
                }
            }

            // plugin_data
            let plugin_data = lua.create_table()?;
            let hooks: LuaValue = core.get("session_save_hooks")?;
            if let LuaValue::Table(hooks_t) = hooks {
                let pcall: LuaFunction = lua.globals().get("pcall")?;
                for pair in hooks_t.pairs::<LuaValue, LuaFunction>() {
                    let (name, hook) = pair?;
                    let result: LuaMultiValue = pcall.call(hook)?;
                    let vals: Vec<LuaValue> = result.into_vec();
                    let ok = vals
                        .first()
                        .and_then(|v| {
                            if let LuaValue::Boolean(b) = v {
                                Some(*b)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(false);
                    if ok {
                        if let Some(result_val) = vals.get(1) {
                            if *result_val != LuaValue::Nil {
                                plugin_data.set(name, result_val.clone())?;
                            }
                        }
                    }
                }
            }

            // active_project
            let root_project_fn: LuaFunction = core.get("root_project")?;
            let rp: LuaValue = root_project_fn.call(())?;
            let active_project: LuaValue = if let LuaValue::Table(ref rp_t) = rp {
                rp_t.get("path")?
            } else {
                LuaValue::Boolean(false)
            };

            let system: LuaTable = lua.globals().get("system")?;
            let get_window_size: LuaFunction = system.get("get_window_size")?;
            let window: LuaValue = core.get("window")?;
            // Build window size table without table.pack's `n` field,
            // which poisons table_to_json's array detection.
            let win_packed: LuaValue = {
                let vals: LuaMultiValue =
                    get_window_size.call(window.clone())?;
                let v = vals.into_vec();
                let t = lua.create_table()?;
                for (i, val) in v.into_iter().enumerate() {
                    t.raw_set((i + 1) as i64, val)?;
                }
                LuaValue::Table(t)
            };
            let get_window_mode: LuaFunction = system.get("get_window_mode")?;
            let win_mode: LuaValue = get_window_mode.call(window)?;

            let session_data = lua.create_table()?;
            session_data.set("recents", core.get::<LuaValue>("recent_projects")?)?;
            session_data.set("active_project", active_project)?;
            session_data.set("window", win_packed)?;
            session_data.set("window_mode", win_mode)?;
            session_data.set("previous_find", core.get::<LuaValue>("previous_find")?)?;
            session_data.set(
                "previous_replace",
                core.get::<LuaValue>("previous_replace")?,
            )?;
            let recent_files: LuaValue = core.get("recent_files")?;
            session_data.set(
                "recent_files",
                if recent_files == LuaValue::Nil {
                    LuaValue::Table(lua.create_table()?)
                } else {
                    recent_files
                },
            )?;
            session_data.set("treeview_size", treeview_size)?;
            session_data.set("open_files", open_files)?;
            session_data.set("open_files_undo", open_files_undo)?;
            session_data.set("plugin_data", plugin_data)?;

            // Persist active file in session.json and on disk.
            // Try active_view first, fall back to last_active_view (the
            // active view may be the NagView dialog during quit).
            let mut active_file_val = LuaValue::Nil;
            let av_candidates = [
                core.get::<LuaValue>("active_view")?,
                core.get::<LuaValue>("last_active_view")?,
            ];
            let active_view = av_candidates
                .iter()
                .find(|v| {
                    matches!(v, LuaValue::Table(t) if !matches!(t.get::<LuaValue>("doc"), Ok(LuaValue::Nil) | Err(_)))
                })
                .cloned()
                .unwrap_or(LuaValue::Nil);
            if let LuaValue::Table(ref av) = active_view {
                let doc_val: LuaValue = av.get("doc")?;
                if let LuaValue::Table(ref doc) = doc_val {
                    let abs: LuaValue = doc.get("abs_filename")?;
                    if let LuaValue::String(_) = abs {
                        active_file_val = abs.clone();
                    }
                }
            }
            // Note: active_file is now written immediately on tab switch,
            // not during session save. We still store it in session_data for compatibility.
            session_data.set("active_file", active_file_val)?;

            let save_fn: LuaFunction = session_native.get("save")?;
            save_fn.call::<()>(session_data)?;
            Ok(())
        })?,
    )?;

    // _update_recents_project(action, dir_path_abs)
    core.set(
        "_update_recents_project",
        lua.create_function(|lua, (action, dir_path_abs): (String, String)| {
            let common: LuaTable = get_module(lua, "core.common")?;
            let normalize_volume: LuaFunction = common.get("normalize_volume")?;
            let dirname: LuaValue = normalize_volume.call(dir_path_abs)?;
            if dirname == LuaValue::Nil {
                return Ok(());
            }
            let core = get_core(lua)?;
            let session_native: LuaTable = get_module(lua, "session_native")?;
            let update_fn: LuaFunction = session_native.get("update_recent_projects")?;
            let recents: LuaValue = core.get("recent_projects")?;
            let new_recents: LuaValue = update_fn.call((recents, action, dirname))?;
            core.set("recent_projects", new_recents)?;
            Ok(())
        })?,
    )?;

    // _update_recent_file(path)
    core.set(
        "_update_recent_file",
        lua.create_function(|lua, path: String| {
            let common: LuaTable = get_module(lua, "core.common")?;
            let normalize_volume: LuaFunction = common.get("normalize_volume")?;
            let filename: LuaValue = normalize_volume.call(path)?;
            if filename == LuaValue::Nil {
                return Ok(());
            }
            let core = get_core(lua)?;
            let session_native: LuaTable = get_module(lua, "session_native")?;
            let update_fn: LuaFunction = session_native.get("update_recent_files")?;
            let recent_files: LuaValue = core.get("recent_files")?;
            let rf = if recent_files == LuaValue::Nil {
                LuaValue::Table(lua.create_table()?)
            } else {
                recent_files
            };
            let new_rf: LuaValue = update_fn.call((rf, filename))?;
            core.set("recent_files", new_rf)?;
            Ok(())
        })?,
    )?;

    // update_recent_file (public alias)
    let urf: LuaFunction = core.get("_update_recent_file")?;
    core.set("update_recent_file", urf)?;

    // _release_project_resources(project)
    core.set(
        "_release_project_resources",
        lua.create_function(|lua, project: LuaValue| {
            if let LuaValue::Table(ref proj) = project {
                let path: LuaValue = proj.get("path")?;
                if path == LuaValue::Nil {
                    return Ok(());
                }
                let pcall: LuaFunction = lua.globals().get("pcall")?;
                let npm: LuaTable = get_module(lua, "project_model")?;
                let inv: LuaValue = npm.get("invalidate")?;
                if inv != LuaValue::Nil {
                    pcall.call::<()>((inv, path.clone()))?;
                }
                let nm: LuaTable = get_module(lua, "project_manifest")?;
                let inv2: LuaValue = nm.get("invalidate")?;
                if inv2 != LuaValue::Nil {
                    pcall.call::<()>((inv2, path))?;
                }
            }
            Ok(())
        })?,
    )?;

    // _clear_native_runtime_caches
    core.set(
        "_clear_native_runtime_caches",
        lua.create_function(|lua, ()| {
            let pcall: LuaFunction = lua.globals().get("pcall")?;

            let si: LuaTable = get_module(lua, "symbol_index")?;
            let clear_all: LuaValue = si.get("clear_all")?;
            if clear_all != LuaValue::Nil {
                pcall.call::<()>(clear_all)?;
            }
            let shrink: LuaValue = si.get("shrink")?;
            if shrink != LuaValue::Nil {
                pcall.call::<()>(shrink)?;
            }

            let gn: LuaTable = get_module(lua, "git_native")?;
            let cc: LuaValue = gn.get("clear_cache")?;
            if cc != LuaValue::Nil {
                pcall.call::<()>(cc)?;
            }

            let lm: LuaTable = get_module(lua, "lsp_manager")?;
            let crs: LuaValue = lm.get("clear_runtime_state")?;
            if crs != LuaValue::Nil {
                pcall.call::<()>(crs)?;
            }

            let lt: LuaTable = get_module(lua, "lsp_transport")?;
            let ca: LuaValue = lt.get("clear_all")?;
            if ca != LuaValue::Nil {
                pcall.call::<()>(ca)?;
            }
            Ok(())
        })?,
    )?;

    // _strip_trailing_slash(filename)
    core.set(
        "_strip_trailing_slash",
        lua.create_function(|lua, filename: String| {
            let pathsep: String = lua.globals().get("PATHSEP")?;
            let sep = pathsep.chars().next().unwrap_or('/');
            if filename.ends_with(sep) && filename.len() > 1 {
                // Check it's not just "X:" on windows
                let bytes = filename.as_bytes();
                let second_last = bytes.len().checked_sub(2).and_then(|i| bytes.get(i));
                if second_last != Some(&b':') {
                    return Ok(filename[..filename.len() - 1].to_string());
                }
            }
            Ok(filename)
        })?,
    )?;

    // _create_user_directory
    core.set(
        "_create_user_directory",
        lua.create_function(|lua, ()| {
            let common: LuaTable = get_module(lua, "core.common")?;
            let mkdirp: LuaFunction = common.get("mkdirp")?;
            let userdir: String = lua.globals().get("USERDIR")?;
            let pathsep: String = lua.globals().get("PATHSEP")?;

            let result: LuaMultiValue = mkdirp.call(userdir.clone())?;
            let vals: Vec<LuaValue> = result.into_vec();
            let success = vals
                .first()
                .and_then(|v| {
                    if let LuaValue::Boolean(b) = v {
                        Some(*b)
                    } else {
                        None
                    }
                })
                .unwrap_or(false);
            if !success {
                let err_msg = vals.get(1).map(|v| format!("{:?}", v)).unwrap_or_default();
                return Err(LuaError::runtime(format!(
                    "cannot create directory \"{}\": {}",
                    userdir, err_msg
                )));
            }

            let system: LuaTable = lua.globals().get("system")?;
            let mkdir: LuaFunction = system.get("mkdir")?;
            for modname in &["plugins", "colors", "fonts"] {
                let subdirname = format!("{}{}{}", userdir, pathsep, modname);
                let ok: LuaValue = mkdir.call(subdirname.clone())?;
                if ok == LuaValue::Nil || ok == LuaValue::Boolean(false) {
                    return Err(LuaError::runtime(format!(
                        "cannot create directory: \"{}\"",
                        subdirname
                    )));
                }
            }
            Ok(())
        })?,
    )?;

    // _write_user_init_file(init_filename)
    core.set(
        "_write_user_init_file",
        lua.create_function(|lua, init_filename: String| {
            let io_mod: LuaTable = lua.globals().get("io")?;
            let open: LuaFunction = io_mod.get("open")?;
            let file: LuaValue = open.call((init_filename.clone(), "w"))?;
            let file_t = match file {
                LuaValue::Table(t) => t,
                LuaValue::UserData(ud) => {
                    let write: LuaFunction = ud.get("write")?;
                    write.call::<()>((
                        ud.clone(),
                        "-- Bootstrap the user configuration.\n\
                         -- Put all runtime settings and customizations in config.lua.\n\
                         \n\
                         if not rawget(_G, \"__lite_anvil_user_config_loaded\") then\n  \
                           require \"config\"\n\
                         end\n\n",
                    ))?;
                    let close: LuaFunction = ud.get("close")?;
                    close.call::<()>(ud)?;
                    return Ok(());
                }
                _ => {
                    return Err(LuaError::runtime(format!(
                        "cannot create file: \"{}\"",
                        init_filename
                    )));
                }
            };
            let write: LuaFunction = file_t.get("write")?;
            write.call::<()>((
                file_t.clone(),
                "-- Bootstrap the user configuration.\n\
                 -- Put all runtime settings and customizations in config.lua.\n\
                 \n\
                 if not rawget(_G, \"__lite_anvil_user_config_loaded\") then\n  \
                   require \"config\"\n\
                 end\n\n",
            ))?;
            let close: LuaFunction = file_t.get("close")?;
            close.call::<()>(file_t)?;
            Ok(())
        })?,
    )?;

    // _write_user_config_file(config_filename) — writes the default config template
    core.set(
        "_write_user_config_file",
        lua.create_function(|lua, config_filename: String| {
            let io_mod: LuaTable = lua.globals().get("io")?;
            let open: LuaFunction = io_mod.get("open")?;
            let file: LuaValue = open.call((config_filename.clone(), "w"))?;
            let content = build_default_config_content();
            match file {
                LuaValue::UserData(ud) => {
                    let write: LuaFunction = ud.get("write")?;
                    write.call::<()>((ud.clone(), content))?;
                    let close: LuaFunction = ud.get("close")?;
                    close.call::<()>(ud)?;
                }
                LuaValue::Table(t) => {
                    let write: LuaFunction = t.get("write")?;
                    write.call::<()>((t.clone(), content))?;
                    let close: LuaFunction = t.get("close")?;
                    close.call::<()>(t)?;
                }
                _ => {
                    return Err(LuaError::runtime(format!(
                        "cannot create file: \"{}\"",
                        config_filename
                    )));
                }
            }
            Ok(())
        })?,
    )?;

    // write_init_project_module(init_filename)
    core.set(
        "write_init_project_module",
        lua.create_function(|lua, init_filename: String| {
            let io_mod: LuaTable = lua.globals().get("io")?;
            let open: LuaFunction = io_mod.get("open")?;
            let file: LuaValue = open.call((init_filename.clone(), "w"))?;
            let content = build_project_init_content();
            match file {
                LuaValue::UserData(ud) => {
                    let write: LuaFunction = ud.get("write")?;
                    write.call::<()>((ud.clone(), content))?;
                    let close: LuaFunction = ud.get("close")?;
                    close.call::<()>(ud)?;
                }
                LuaValue::Table(t) => {
                    let write: LuaFunction = t.get("write")?;
                    write.call::<()>((t.clone(), content))?;
                    let close: LuaFunction = t.get("close")?;
                    close.call::<()>(t)?;
                }
                _ => {
                    return Err(LuaError::runtime(format!(
                        "cannot create file: \"{}\"",
                        init_filename
                    )));
                }
            }
            Ok(())
        })?,
    )?;

    // ensure_user_directory
    core.set(
        "ensure_user_directory",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let try_fn: LuaFunction = core.get("try")?;
            let inner = lua.create_function(|lua, ()| {
                let core = get_core(lua)?;
                let system: LuaTable = lua.globals().get("system")?;
                let get_file_info: LuaFunction = system.get("get_file_info")?;
                let userdir: String = lua.globals().get("USERDIR")?;

                let info: LuaValue = get_file_info.call(userdir)?;
                if info == LuaValue::Nil {
                    let create_dir: LuaFunction = core.get("_create_user_directory")?;
                    create_dir.call::<()>(())?;
                }

                let get_init: LuaFunction = core.get("_get_user_init_filename")?;
                let init_filename: String = get_init.call(())?;
                let info2: LuaValue = get_file_info.call(init_filename.clone())?;
                if info2 == LuaValue::Nil {
                    let write_init: LuaFunction = core.get("_write_user_init_file")?;
                    write_init.call::<()>(init_filename)?;
                }

                let get_config: LuaFunction = core.get("_get_user_config_filename")?;
                let config_filename: String = get_config.call(())?;
                let info3: LuaValue = get_file_info.call(config_filename.clone())?;
                if info3 == LuaValue::Nil {
                    let write_config: LuaFunction = core.get("_write_user_config_file")?;
                    write_config.call::<()>(config_filename)?;
                }
                Ok(())
            })?;
            try_fn.call::<LuaMultiValue>(inner)
        })?,
    )?;

    // configure_borderless_window
    core.set(
        "configure_borderless_window",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let config: LuaTable = get_module(lua, "core.config")?;
            let borderless: bool = config
                .get::<LuaValue>("borderless")?
                .eq(&LuaValue::Boolean(true));
            let system: LuaTable = lua.globals().get("system")?;
            let set_bordered: LuaFunction = system.get("set_window_bordered")?;
            let window: LuaValue = core.get("window")?;
            set_bordered.call::<()>((window, !borderless))?;
            let title_view: LuaTable = core.get("title_view")?;
            let configure_hit: LuaFunction = title_view.get("configure_hit_test")?;
            configure_hit.call::<()>((title_view.clone(), borderless))?;
            title_view.set("visible", borderless)?;
            Ok(())
        })?,
    )?;

    // temp_file_prefix stored so delete_temp_files can use it
    {
        let prefix: String = lua.registry_value(prefix_key)?;
        core.set("_temp_file_prefix", prefix)?;
    }

    // delete_temp_files(dir?)
    {
        let prefix_key2 = lua.create_registry_value(lua.registry_value::<String>(prefix_key)?)?;
        core.set(
            "delete_temp_files",
            lua.create_function(move |lua, dir: Option<String>| {
                let common: LuaTable = get_module(lua, "core.common")?;
                let userdir: String = lua.globals().get("USERDIR")?;
                let pathsep: String = lua.globals().get("PATHSEP")?;
                let dir = match dir {
                    Some(d) => {
                        let normalize: LuaFunction = common.get("normalize_path")?;
                        normalize.call::<String>(d)?
                    }
                    None => userdir,
                };
                let system: LuaTable = lua.globals().get("system")?;
                let list_dir: LuaFunction = system.get("list_dir")?;
                let entries: LuaValue = list_dir.call(dir.clone())?;
                let prefix: String = lua.registry_value(&prefix_key2)?;
                if let LuaValue::Table(entries_t) = entries {
                    let os_mod: LuaTable = lua.globals().get("os")?;
                    let remove: LuaFunction = os_mod.get("remove")?;
                    for entry in entries_t.sequence_values::<String>() {
                        let filename = entry?;
                        if filename.starts_with(&prefix) {
                            let full = format!("{}{}{}", dir, pathsep, filename);
                            remove.call::<()>(full)?;
                        }
                    }
                }
                Ok(())
            })?,
        )?;
    }

    // temp_filename(ext?, dir?)
    {
        let state_key2 =
            lua.create_registry_value(lua.registry_value::<LuaTable>(state_key)?.clone())?;
        let prefix_key3 = lua.create_registry_value(lua.registry_value::<String>(prefix_key)?)?;
        core.set(
            "temp_filename",
            lua.create_function(move |lua, (ext, dir): (Option<String>, Option<String>)| {
                let common: LuaTable = get_module(lua, "core.common")?;
                let userdir: String = lua.globals().get("USERDIR")?;
                let pathsep: String = lua.globals().get("PATHSEP")?;
                let dir = match dir {
                    Some(d) => {
                        let normalize: LuaFunction = common.get("normalize_path")?;
                        normalize.call::<String>(d)?
                    }
                    None => userdir,
                };
                let state: LuaTable = lua.registry_value(&state_key2)?;
                let counter: i64 = state.get("temp_file_counter")?;
                let new_counter = counter + 1;
                state.set("temp_file_counter", new_counter)?;
                let prefix: String = lua.registry_value(&prefix_key3)?;
                let ext_str = ext.unwrap_or_default();
                Ok(format!(
                    "{}{}{}{:06x}{}",
                    dir, pathsep, prefix, new_counter, ext_str
                ))
            })?,
        )?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Project management functions
// ---------------------------------------------------------------------------

fn register_project_fns(lua: &Lua, core: &LuaTable, _state_key: &LuaRegistryKey) -> LuaResult<()> {
    // core.add_project(project)
    core.set(
        "add_project",
        lua.create_function(|lua, project: LuaValue| {
            let core = get_core(lua)?;
            let common: LuaTable = get_module(lua, "core.common")?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            let insert: LuaFunction = table_mod.get("insert")?;

            let project_val = if let LuaValue::String(s) = &project {
                let normalize: LuaFunction = common.get("normalize_volume")?;
                let normalized: LuaValue = normalize.call(s.to_str()?.to_string())?;
                // Call Project(normalized)
                let require: LuaFunction = lua.globals().get("require")?;
                let project_cls: LuaValue = require.call("core.project")?;
                match project_cls {
                    LuaValue::Table(t) => {
                        let mt: Option<LuaTable> = t.metatable();
                        if let Some(mt) = mt {
                            let call_fn: LuaValue = mt.get("__call")?;
                            if let LuaValue::Function(f) = call_fn {
                                f.call::<LuaValue>((t, normalized))?
                            } else {
                                LuaValue::Nil
                            }
                        } else {
                            LuaValue::Nil
                        }
                    }
                    LuaValue::Function(f) => f.call::<LuaValue>(normalized)?,
                    _ => LuaValue::Nil,
                }
            } else {
                project
            };

            let projects: LuaTable = core.get("projects")?;
            insert.call::<()>((projects, project_val.clone()))?;

            if let LuaValue::Table(ref p) = project_val {
                let path: String = p.get("path")?;
                let update: LuaFunction = core.get("_update_recents_project")?;
                update.call::<()>(("add", path))?;
            }

            core.set("redraw", true)?;
            Ok(project_val)
        })?,
    )?;

    // core.remove_project(project, force?)
    core.set(
        "remove_project",
        lua.create_function(|lua, (project, force): (LuaValue, Option<bool>)| {
            let core = get_core(lua)?;
            let common: LuaTable = get_module(lua, "core.common")?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            let remove: LuaFunction = table_mod.get("remove")?;
            let projects: LuaTable = core.get("projects")?;
            let start = if force.unwrap_or(false) { 1 } else { 2 };
            let len = projects.raw_len();

            for i in start..=len {
                let proj_i: LuaTable = projects.get(i)?;
                let proj_path: String = proj_i.get("path")?;

                let matches = if let LuaValue::Table(ref pt) = project {
                    lua.globals()
                        .get::<LuaFunction>("rawequal")?
                        .call::<bool>((pt.clone(), proj_i.clone()))?
                } else if let LuaValue::String(ref s) = project {
                    s.to_str()? == proj_path
                } else {
                    false
                };

                if matches {
                    remove.call::<()>((projects.clone(), i))?;

                    // Close open views belonging to the removed project.
                    let root_view: LuaValue = core.get("root_view")?;
                    if let LuaValue::Table(ref rv) = root_view {
                        let root_node: LuaTable = rv.get("root_node")?;
                        let get_children: LuaFunction = root_node.get("get_children")?;
                        let children: LuaTable = get_children.call(root_node.clone())?;
                        let entries = lua.create_table()?;
                        let mut entry_count = 0;

                        for child in children.sequence_values::<LuaTable>() {
                            let view = child?;
                            let doc_val: LuaValue = view.get("doc")?;
                            if let LuaValue::Table(ref doc) = doc_val {
                                let abs: LuaValue = doc.get("abs_filename")?;
                                if let LuaValue::String(ref abs_s) = abs {
                                    let abs_str = abs_s.to_str()?.to_string();
                                    let path_belongs: LuaFunction =
                                        common.get("path_belongs_to")?;
                                    let belongs: bool =
                                        path_belongs.call((abs_str, proj_path.clone()))?;
                                    if belongs {
                                        let root_node2: LuaTable = rv.get("root_node")?;
                                        let get_node: LuaFunction =
                                            root_node2.get("get_node_for_view")?;
                                        let node: LuaValue =
                                            get_node.call((root_node2, view.clone()))?;
                                        if node != LuaValue::Nil {
                                            entry_count += 1;
                                            let entry = lua.create_table()?;
                                            entry.set("node", node)?;
                                            entry.set("view", view)?;
                                            entries.set(entry_count, entry)?;
                                        }
                                    }
                                }
                            }
                        }

                        if entry_count > 0 {
                            let confirm_close: LuaFunction = rv.get("confirm_close_views")?;
                            confirm_close.call::<()>((rv.clone(), entries))?;
                        }
                    }

                    return Ok(LuaValue::Table(proj_i));
                }
            }

            Ok(LuaValue::Boolean(false))
        })?,
    )?;

    // core.set_project(project)
    core.set(
        "set_project",
        lua.create_function(|lua, project: LuaValue| {
            let core = get_core(lua)?;

            // Close all docviews and unreferenced docs.
            let root_view: LuaValue = core.get("root_view")?;
            if let LuaValue::Table(ref rv) = root_view {
                let close_all: LuaFunction = rv.get("close_all_docviews")?;
                close_all.call::<()>(rv.clone())?;
                let close_unref: LuaFunction = core.get("_close_unreferenced_docs")?;
                close_unref.call::<()>(())?;
            }

            // Remove all projects.
            let projects: LuaTable = core.get("projects")?;
            while projects.raw_len() > 0 {
                let last: LuaValue = projects.get(projects.raw_len())?;
                let remove_fn: LuaFunction = core.get("remove_project")?;
                let removed: LuaValue = remove_fn.call((last, true))?;
                let release: LuaFunction = core.get("_release_project_resources")?;
                release.call::<()>(removed)?;
            }

            let clear_caches: LuaFunction = core.get("_clear_native_runtime_caches")?;
            clear_caches.call::<()>(())?;

            if project == LuaValue::Nil || project == LuaValue::Boolean(false) {
                core.set("redraw", true)?;
                return Ok(LuaValue::Nil);
            }

            let add_project: LuaFunction = core.get("add_project")?;
            let result: LuaValue = add_project.call(project)?;
            Ok(result)
        })?,
    )?;

    // core.close_project()
    core.set(
        "close_project",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;

            let root_view: LuaValue = core.get("root_view")?;
            if let LuaValue::Table(ref rv) = root_view {
                let close_all: LuaFunction = rv.get("close_all_docviews")?;
                close_all.call::<()>(rv.clone())?;
                let close_unref: LuaFunction = core.get("_close_unreferenced_docs")?;
                close_unref.call::<()>(())?;
            }

            let projects: LuaTable = core.get("projects")?;
            while projects.raw_len() > 0 {
                let last: LuaValue = projects.get(projects.raw_len())?;
                let remove_fn: LuaFunction = core.get("remove_project")?;
                let removed: LuaValue = remove_fn.call((last, true))?;
                let release: LuaFunction = core.get("_release_project_resources")?;
                release.call::<()>(removed)?;
            }

            let clear_caches: LuaFunction = core.get("_clear_native_runtime_caches")?;
            clear_caches.call::<()>(())?;
            core.set("redraw", true)?;
            Ok(())
        })?,
    )?;

    // core.open_project(project)
    core.set(
        "open_project",
        lua.create_function(|lua, project: LuaValue| {
            let core = get_core(lua)?;
            core.set("skip_session_open_files", true)?;
            let set_project: LuaFunction = core.get("set_project")?;
            let proj: LuaTable = set_project.call(project)?;
            let path: String = proj.get("path")?;
            let update: LuaFunction = core.get("_update_recents_project")?;
            update.call::<()>(("add", path))?;
            let command: LuaTable = get_module(lua, "core.command")?;
            let perform: LuaFunction = command.get("perform")?;
            perform.call::<()>("core:restart")?;
            Ok(())
        })?,
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// core.init — the big initialization function
// ---------------------------------------------------------------------------

fn register_init_fn(
    lua: &Lua,
    core: &LuaTable,
    state_key: &LuaRegistryKey,
    lazy_plugins_key: &LuaRegistryKey,
    lazy_handlers_key: &LuaRegistryKey,
    lazy_loaded_key: &LuaRegistryKey,
) -> LuaResult<()> {
    // We need separate registry values for each closure that captures them.
    let state_key2 =
        lua.create_registry_value(lua.registry_value::<LuaTable>(state_key)?.clone())?;
    let lazy_plugins_key2 =
        lua.create_registry_value(lua.registry_value::<LuaTable>(lazy_plugins_key)?.clone())?;
    let lazy_handlers_key2 =
        lua.create_registry_value(lua.registry_value::<LuaTable>(lazy_handlers_key)?.clone())?;
    let lazy_loaded_key2 =
        lua.create_registry_value(lua.registry_value::<LuaTable>(lazy_loaded_key)?.clone())?;

    // Suppress unused variable warnings — these keys are captured by the closure.
    let _ = (
        &state_key2,
        &lazy_plugins_key2,
        &lazy_handlers_key2,
        &lazy_loaded_key2,
    );

    core.set(
        "init",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let require: LuaFunction = lua.globals().get("require")?;
            let pcall: LuaFunction = lua.globals().get("pcall")?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            core.set("log_items", lua.create_table()?)?;

            // Log version.
            let log_quiet: LuaFunction = core.get("log_quiet")?;
            let version: String = lua.globals().get("VERSION")?;
            let mod_version_string: String = lua.globals().get("MOD_VERSION_STRING")?;
            log_quiet.call::<()>((
                "Lite-Anvil version %s - mod-version %s",
                version,
                mod_version_string,
            ))?;

            // Require deferred modules.
            let _command: LuaTable = require.call("core.command")?;
            let _keymap: LuaTable = require.call("core.keymap")?;
            let _dirwatch: LuaTable = require.call("core.dirwatch")?;
            let _ime: LuaTable = require.call("core.ime")?;
            let root_view_cls: LuaValue = require.call("core.rootview")?;
            let status_view_cls: LuaValue = require.call("core.statusview")?;
            let title_view_cls: LuaValue = require.call("core.titleview")?;
            let command_view_cls: LuaValue = require.call("core.commandview")?;
            let nag_view_cls: LuaValue = require.call("core.nagview")?;
            let _project_cls: LuaValue = require.call("core.project")?;
            let _doc_view_cls: LuaValue = require.call("core.docview")?;
            let _doc_cls: LuaValue = require.call("core.doc")?;

            // Normalize paths on Windows.
            let pathsep: String = lua.globals().get("PATHSEP")?;
            if pathsep == "\\" {
                let common: LuaTable = get_module(lua, "core.common")?;
                let normalize_volume: LuaFunction = common.get("normalize_volume")?;
                let userdir: String = lua.globals().get("USERDIR")?;
                let datadir: String = lua.globals().get("DATADIR")?;
                let exedir: String = lua.globals().get("EXEDIR")?;
                let nu: String = normalize_volume.call(userdir)?;
                let nd: String = normalize_volume.call(datadir)?;
                let ne: String = normalize_volume.call(exedir)?;
                lua.globals().set("USERDIR", nu)?;
                lua.globals().set("DATADIR", nd)?;
                lua.globals().set("EXEDIR", ne)?;
            }

            // Load session.
            let load_session: LuaFunction = core.get("_load_session")?;
            let session: LuaTable = load_session.call(())?;
            core.set("session", session.clone())?;
            let recents: LuaValue = session.get("recents")?;
            core.set(
                "recent_projects",
                if recents == LuaValue::Nil {
                    LuaValue::Table(lua.create_table()?)
                } else {
                    recents
                },
            )?;
            let recent_files: LuaValue = session.get("recent_files")?;
            core.set(
                "recent_files",
                if recent_files == LuaValue::Nil {
                    LuaValue::Table(lua.create_table()?)
                } else {
                    recent_files
                },
            )?;
            core.set("previous_find", lua.create_table()?)?;
            core.set("previous_replace", lua.create_table()?)?;

            // Determine project_dir from session or args.
            let active_proj: LuaValue = session.get("active_project")?;
            let recent_projects: LuaTable = core.get("recent_projects")?;
            let first_recent: LuaValue = recent_projects.get(1)?;
            // If active_project is explicitly false, the user closed the
            // project before quitting -- don't reopen a recent project.
            let mut project_dir: String = match active_proj {
                LuaValue::String(s) => s.to_str()?.to_string(),
                LuaValue::Boolean(false) => ".".to_string(),
                _ => match first_recent {
                    LuaValue::String(s) => s.to_str()?.to_string(),
                    _ => ".".to_string(),
                },
            };
            let mut project_dir_explicit = false;
            let files_list = lua.create_table()?;

            let restarted: LuaValue = lua.globals().get("RESTARTED")?;
            if restarted == LuaValue::Nil || restarted == LuaValue::Boolean(false) {
                let args: LuaTable = lua.globals().get("ARGS")?;
                let args_len = args.raw_len();
                let common: LuaTable = get_module(lua, "core.common")?;
                let system_t: LuaTable = lua.globals().get("system")?;
                let strip_fn: LuaFunction = core.get("_strip_trailing_slash")?;
                let is_abs: LuaFunction = common.get("is_absolute_path")?;
                let abs_path_fn: LuaFunction = system_t.get("absolute_path")?;
                let normalize_path: LuaFunction = common.get("normalize_path")?;
                let get_file_info: LuaFunction = system_t.get("get_file_info")?;

                for i in 2..=args_len {
                    let arg: String = args.get(i)?;
                    let arg_filename: String = strip_fn.call(arg.clone())?;
                    let info: LuaValue = get_file_info.call(arg_filename.clone())?;
                    let info_type: LuaValue = if let LuaValue::Table(ref t) = info {
                        t.get("type")?
                    } else {
                        LuaValue::Nil
                    };
                    if info_type == LuaValue::String(lua.create_string("dir")?) {
                        project_dir = arg_filename;
                        project_dir_explicit = true;
                    } else if !arg.starts_with("-psn") {
                        let is_absolute: bool = is_abs.call(arg_filename.clone())?;
                        let file_abs: String = if is_absolute {
                            arg_filename
                        } else {
                            let cwd: String = abs_path_fn.call(".")?;
                            let normalized: String = normalize_path.call(arg_filename)?;
                            format!("{}{}{}", cwd, pathsep, normalized)
                        };
                        let fl = files_list.raw_len() + 1;
                        files_list.set(fl, file_abs.clone())?;
                        // Extract directory from file path.
                        if let Some(pos) = file_abs.rfind(['/', '\\']) {
                            project_dir = file_abs[..pos].to_string();
                        }
                        project_dir_explicit = true;
                    }
                }
            }

            // Ensure user directory exists.
            let ensure: LuaFunction = core.get("ensure_user_directory")?;
            ensure.call::<()>(())?;

            // Initialize core state fields.
            core.set("frame_start", 0)?;
            let clip_inner = lua.create_table()?;
            clip_inner.set(1, 0)?;
            clip_inner.set(2, 0)?;
            clip_inner.set(3, 0)?;
            clip_inner.set(4, 0)?;
            let clip_stack = lua.create_table()?;
            clip_stack.set(1, clip_inner)?;
            core.set("clip_rect_stack", clip_stack)?;
            core.set("docs", lua.create_table()?)?;
            core.set("projects", lua.create_table()?)?;
            core.set("cursor_clipboard", lua.create_table()?)?;
            core.set("cursor_clipboard_whole_line", lua.create_table()?)?;
            core.set("window_mode", "normal")?;

            // core.threads = setmetatable({}, { __mode = "k" })
            let threads = lua.create_table()?;
            let threads_mt = lua.create_table()?;
            threads_mt.set("__mode", "k")?;
            threads.set_metatable(Some(threads_mt))?;
            core.set("threads", threads)?;

            let system_t: LuaTable = lua.globals().get("system")?;
            let get_time: LuaFunction = system_t.get("get_time")?;
            let now: f64 = get_time.call(())?;
            core.set("blink_start", now)?;
            core.set("blink_timer", now)?;
            core.set("active_file_dialogs", lua.create_table()?)?;
            core.set("redraw", true)?;
            core.set("visited_files", lua.create_table()?)?;
            core.set("restart_request", false)?;
            core.set("quit_request", false)?;

            // Create core views.
            let call_constructor = |cls: &LuaValue| -> LuaResult<LuaValue> {
                match cls {
                    LuaValue::Table(t) => {
                        let mt: Option<LuaTable> = t.metatable();
                        if let Some(mt) = mt {
                            let call_fn: LuaValue = mt.get("__call")?;
                            if let LuaValue::Function(f) = call_fn {
                                return f.call(t.clone());
                            }
                        }
                        Err(LuaError::runtime("cannot call constructor"))
                    }
                    LuaValue::Function(f) => f.call(()),
                    _ => Err(LuaError::runtime("invalid class")),
                }
            };

            core.set("root_view", call_constructor(&root_view_cls)?)?;
            core.set("command_view", call_constructor(&command_view_cls)?)?;
            core.set("status_view", call_constructor(&status_view_cls)?)?;
            core.set("nag_view", call_constructor(&nag_view_cls)?)?;
            core.set("title_view", call_constructor(&title_view_cls)?)?;

            // Build node tree.
            let root_view: LuaTable = core.get("root_view")?;
            let cur_node: LuaTable = root_view.get("root_node")?;
            cur_node.set("is_primary_node", true)?;
            let title_view: LuaValue = core.get("title_view")?;
            let nag_view: LuaValue = core.get("nag_view")?;
            let command_view: LuaValue = core.get("command_view")?;
            let status_view: LuaValue = core.get("status_view")?;

            let split_opts_y = lua.create_table()?;
            split_opts_y.set("y", true)?;

            let split: LuaFunction = cur_node.get("split")?;
            split.call::<()>((
                cur_node.clone(),
                "up",
                title_view,
                split_opts_y.clone(),
            ))?;
            let cur_node_b: LuaTable = cur_node.get("b")?;
            let split2: LuaFunction = cur_node_b.get("split")?;
            split2.call::<()>((
                cur_node_b.clone(),
                "up",
                nag_view,
                split_opts_y.clone(),
            ))?;
            let cur_node_b2: LuaTable = cur_node_b.get("b")?;
            let split3: LuaFunction = cur_node_b2.get("split")?;
            let cur_node_c: LuaTable =
                split3.call((cur_node_b2.clone(), "down", command_view, split_opts_y.clone()))?;
            let split4: LuaFunction = cur_node_c.get("split")?;
            split4.call::<()>((
                cur_node_c.clone(),
                "down",
                status_view,
                split_opts_y,
            ))?;

            // Load default commands.
            let command_mod: LuaTable = get_module(lua, "core.command")?;
            let add_defaults: LuaFunction = command_mod.get("add_defaults")?;
            add_defaults.call::<()>(())?;

            // Set up project directory.
            let system_t2: LuaTable = lua.globals().get("system")?;
            let abs_path_fn: LuaFunction = system_t2.get("absolute_path")?;
            let project_dir_abs: LuaValue = if !project_dir.is_empty() {
                abs_path_fn.call(project_dir.clone())?
            } else {
                LuaValue::Nil
            };

            let mut pda_cleared = false;
            if let LuaValue::String(ref pda_s) = project_dir_abs {
                let pda_str = pda_s.to_str()?.to_string();
                let set_project: LuaFunction = core.get("set_project")?;
                let result: LuaMultiValue = pcall.call((set_project, pda_str.clone()))?;
                let vals: Vec<LuaValue> = result.into_vec();
                let ok = vals.first().and_then(|v| {
                    if let LuaValue::Boolean(b) = v { Some(*b) } else { None }
                }).unwrap_or(false);
                if ok {
                    if project_dir_explicit {
                        let update: LuaFunction = core.get("_update_recents_project")?;
                        update.call::<()>(("add", pda_str))?;
                    }
                } else {
                    if !project_dir_explicit {
                        let update: LuaFunction = core.get("_update_recents_project")?;
                        update.call::<()>(("remove", project_dir))?;
                    }
                    pda_cleared = true;
                }
            } else {
                pda_cleared = true;
            }

            core.set("session_save_hooks", lua.create_table()?)?;
            core.set("session_load_hooks", lua.create_table()?)?;

            // Load plugins.
            let load_plugins: LuaFunction = core.get("load_plugins")?;
            let lp_result: LuaMultiValue = load_plugins.call(())?;
            let lp_vals: Vec<LuaValue> = lp_result.into_vec();
            let plugins_success = lp_vals.first().and_then(|v| {
                if let LuaValue::Boolean(b) = v { Some(*b) } else { None }
            }).unwrap_or(true);
            let plugins_refuse_list: LuaTable = match lp_vals.get(1) {
                Some(LuaValue::Table(t)) => t.clone(),
                _ => lua.create_table()?,
            };

            // Initialize git as a core feature.
            register_core_git(lua)?;

            // Initialize LSP if not disabled.
            let config: LuaTable = get_module(lua, "core.config")?;
            let plugins_conf: LuaValue = config.get("plugins")?;
            let lsp_disabled = if let LuaValue::Table(ref pt) = plugins_conf {
                let lsp_val: LuaValue = pt.get("lsp")?;
                lsp_val == LuaValue::Boolean(false)
            } else {
                false
            };
            if !lsp_disabled {
                require.call::<()>("plugins.lsp")?;
            }

            // Restore or create window.
            let renwindow: LuaTable = lua.globals().get("renwindow")?;
            let restore_fn: LuaFunction = renwindow.get("_restore")?;
            let restored: LuaValue = restore_fn.call(())?;

            let window: LuaValue = core.get("window")?;
            if window != LuaValue::Nil {
                // keep existing window
            } else if restored != LuaValue::Nil {
                core.set("window", restored.clone())?;
            } else {
                let create_fn: LuaFunction = renwindow.get("create")?;
                let new_win: LuaValue = create_fn.call("")?;
                core.set("window", new_win)?;
            }

            let was_restored = restored != LuaValue::Nil;
            if !was_restored {
                let win_mode: LuaValue = session.get("window_mode")?;
                let session_window: LuaValue = session.get("window")?;
                if win_mode == LuaValue::String(lua.create_string("normal")?)
                    && matches!(session_window, LuaValue::Table(_))
                {
                    if let LuaValue::Table(ref wt) = session_window {
                        let set_window_size: LuaFunction = system_t.get("set_window_size")?;
                        let table_unpack: LuaFunction = table_mod.get("unpack")?;
                        let unpacked: LuaMultiValue = table_unpack.call(wt.clone())?;
                        let mut args = vec![core.get::<LuaValue>("window")?];
                        args.extend(unpacked.into_vec());
                        set_window_size.call::<()>(LuaMultiValue::from_vec(args))?;
                    }
                } else if win_mode == LuaValue::String(lua.create_string("maximized")?) {
                    let set_window_mode: LuaFunction = system_t.get("set_window_mode")?;
                    set_window_mode.call::<()>((core.get::<LuaValue>("window")?, "maximized"))?;
                }
            }

            // Log project open.
            if !pda_cleared {
                if let LuaValue::String(ref pda_s) = project_dir_abs {
                    let pda_str = pda_s.to_str()?.to_string();
                    // Split into dir and name.
                    if let Some(pos) = pda_str.rfind(['/', '\\']) {
                        let pdir = &pda_str[..pos];
                        let pname = &pda_str[pos + 1..];
                        let log_quiet2: LuaFunction = core.get("log_quiet")?;
                        log_quiet2.call::<()>((
                            "Opening project %q from directory %s",
                            pname.to_string(),
                            pdir.to_string(),
                        ))?;
                    }
                }
            }

            // Close existing documents if files were specified on command line.
            if files_list.raw_len() > 0 {
                let root_view_close: LuaTable = core.get("root_view")?;
                let close_all: LuaFunction = root_view_close.get("close_all_docviews")?;
                close_all.call::<()>(root_view_close.clone())?;
            }

            for file_val in files_list.sequence_values::<String>() {
                let filename = file_val?;
                let root_view2: LuaTable = core.get("root_view")?;
                let open_doc_fn: LuaFunction = core.get("open_doc")?;
                let doc: LuaValue = open_doc_fn.call(filename)?;
                let rv_open: LuaFunction = root_view2.get("open_doc")?;
                rv_open.call::<()>((root_view2, doc))?;
            }

            if files_list.raw_len() == 0 {
                let add_thread: LuaFunction = core.get("add_thread")?;
                let restore_fn = lua.create_function(|lua, ()| {
                    let core = get_core(lua)?;
                    core.set("_restoring_session", true)?;

                    // Read active file from disk, falling back to session.json.
                    let saved_active: Option<String> = {
                        let userdir: String = lua.globals().get("USERDIR")?;
                        let path = std::path::PathBuf::from(&userdir)
                            .join("storage").join("session").join("active_file");
                        let from_disk = std::fs::read_to_string(&path).ok().and_then(|s| {
                            let trimmed = s.trim();
                            if trimmed.starts_with('"') && trimmed.ends_with('"') {
                                Some(trimmed[1..trimmed.len()-1].to_string())
                            } else {
                                None
                            }
                        });
                        if from_disk.is_some() {
                            from_disk
                        } else {
                            let session: LuaTable = core.get("session")?;
                            let af: LuaValue = session.get("active_file")?;
                            if let LuaValue::String(s) = af {
                                Some(s.to_str()?.to_string())
                            } else {
                                None
                            }
                        }
                    };

                    let root_view: LuaTable = core.get("root_view")?;
                    let get_primary: LuaFunction = root_view.get("get_primary_node")?;
                    let primary: LuaTable = get_primary.call(root_view.clone())?;
                    core.set("skip_session_restore_open_files", false)?;

                    let hooks: LuaValue = core.get("session_load_hooks")?;
                    if let LuaValue::Table(hooks_t) = hooks {
                        let session: LuaTable = core.get("session")?;
                        let plugin_data: LuaValue = session.get("plugin_data")?;
                        let pcall: LuaFunction = lua.globals().get("pcall")?;
                        for pair in hooks_t.pairs::<LuaValue, LuaFunction>() {
                            let (name, hook) = pair?;
                            let data = if let LuaValue::Table(ref pd) = plugin_data {
                                pd.get(name)?
                            } else {
                                LuaValue::Nil
                            };
                            pcall.call::<()>((hook, data, primary.clone()))?;
                        }
                    }

                    let skip: bool = core
                        .get::<LuaValue>("skip_session_restore_open_files")?
                        .eq(&LuaValue::Boolean(true));
                    if !skip {
                        let session: LuaTable = core.get("session")?;
                        let open_files: LuaValue = session.get("open_files")?;
                        let ofu: LuaValue = session.get("open_files_undo")?;
                        if let LuaValue::Table(of) = open_files {
                            let pcall2: LuaFunction = lua.globals().get("pcall")?;
                            let require2: LuaFunction = lua.globals().get("require")?;
                            let dv_cls: LuaValue = require2.call("core.docview")?;
                            let doc_native_r: LuaTable = get_module(lua, "doc_native")?;
                            for path_val in of.sequence_values::<String>() {
                                let path = path_val?;
                                let open_doc: LuaFunction = core.get("open_doc")?;
                                let opts = lua.create_table()?;
                                opts.set("lazy_restore", true)?;
                                let result: LuaMultiValue =
                                    pcall2.call((open_doc, path, opts))?;
                                let vals: Vec<LuaValue> = result.into_vec();
                                let ok = vals.first().and_then(|v| {
                                    if let LuaValue::Boolean(b) = v { Some(*b) } else { None }
                                }).unwrap_or(false);
                                if ok {
                                    if let Some(LuaValue::Table(doc)) = vals.get(1) {
                                        // Restore persistent undo data for this file.
                                        if let LuaValue::Table(ref ofu_t) = ofu {
                                            let abs_fn: LuaValue = doc.get("abs_filename")?;
                                            if let LuaValue::String(ref abs_s) = abs_fn {
                                                let undo_path: LuaValue =
                                                    ofu_t.get(abs_s.clone())?;
                                                if let LuaValue::String(ref up) = undo_path {
                                                    let up_str = up.to_str()?.to_string();
                                                    let deferred: LuaValue =
                                                        doc.get("deferred_load")?;
                                                    if !matches!(deferred, LuaValue::Nil) {
                                                        // Lazily loaded: defer undo restore.
                                                        doc.set(
                                                            "deferred_undo_path",
                                                            up_str,
                                                        )?;
                                                    } else if let Ok(undo_data) =
                                                        std::fs::read(&up_str)
                                                    {
                                                        let buf_id: LuaValue =
                                                            doc.get("buffer_id")?;
                                                        if !matches!(buf_id, LuaValue::Nil) {
                                                            let set_undo: LuaFunction =
                                                                doc_native_r
                                                                    .get("buffer_set_undo_data")?;
                                                            let _ = set_undo
                                                                .call::<LuaValue>((
                                                                    buf_id, undo_data,
                                                                ));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        // Check if already open.
                                        let views: LuaTable = primary.get("views")?;
                                        let mut already_open = false;
                                        for v in views.sequence_values::<LuaTable>() {
                                            let view = v?;
                                            let vdoc: LuaValue = view.get("doc")?;
                                            if let LuaValue::Table(ref vd) = vdoc {
                                                let rawequal: LuaFunction =
                                                    lua.globals().get("rawequal")?;
                                                let eq: bool = rawequal
                                                    .call((vd.clone(), doc.clone()))?;
                                                if eq {
                                                    already_open = true;
                                                    break;
                                                }
                                            }
                                        }
                                        if !already_open {
                                            let view = match &dv_cls {
                                                LuaValue::Table(t) => {
                                                    let mt: Option<LuaTable> = t.metatable();
                                                    if let Some(mt) = mt {
                                                        let cf: LuaValue = mt.get("__call")?;
                                                        if let LuaValue::Function(f) = cf {
                                                            f.call::<LuaValue>((
                                                                t.clone(),
                                                                doc.clone(),
                                                            ))?
                                                        } else {
                                                            LuaValue::Nil
                                                        }
                                                    } else {
                                                        LuaValue::Nil
                                                    }
                                                }
                                                _ => LuaValue::Nil,
                                            };
                                            let add_view: LuaFunction =
                                                primary.get("add_view")?;
                                            add_view.call::<()>((
                                                primary.clone(),
                                                view.clone(),
                                            ))?;
                                            if let LuaValue::Table(ref vt) = view {
                                                let doc_t: LuaTable = vt.get("doc")?;
                                                let get_sel: LuaFunction =
                                                    doc_t.get("get_selection")?;
                                                let sel: LuaValue =
                                                    get_sel.call(doc_t)?;
                                                let scroll_to: LuaFunction =
                                                    vt.get("scroll_to_line")?;
                                                scroll_to.call::<()>((
                                                    vt.clone(),
                                                    sel,
                                                    true,
                                                    true,
                                                ))?;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Restore backed-up dirty/unsaved docs.
                    let mut last_backup_view: Option<LuaTable> = None;
                    {
                        let userdir: String = lua.globals().get("USERDIR")?;
                        let manifest_path = std::path::PathBuf::from(&userdir)
                            .join("backups")
                            .join("manifest.json");
                        if let Ok(manifest_str) = std::fs::read_to_string(&manifest_path) {
                            if let Ok(serde_json::Value::Array(entries)) =
                                serde_json::from_str::<serde_json::Value>(&manifest_str)
                            {
                                let require2: LuaFunction = lua.globals().get("require")?;
                                let doc_native: LuaTable = require2.call("doc_native")?;
                                let dv_cls: LuaValue = require2.call("core.docview")?;
                                let primary: LuaTable =
                                    get_primary.call(root_view.clone())?;
                                for entry in &entries {
                                    let backup_path = match entry.get("backup_path") {
                                        Some(serde_json::Value::String(s)) => s.clone(),
                                        _ => continue,
                                    };
                                    if !std::path::Path::new(&backup_path).exists() {
                                        continue;
                                    }
                                    let new_file = entry
                                        .get("new_file")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(true);
                                    let filename: LuaValue = match entry.get("filename") {
                                        Some(serde_json::Value::String(s)) => {
                                            LuaValue::String(lua.create_string(s.as_bytes())?)
                                        }
                                        _ => LuaValue::Nil,
                                    };
                                    let abs_filename: LuaValue =
                                        match entry.get("abs_filename") {
                                            Some(serde_json::Value::String(s)) => {
                                                LuaValue::String(
                                                    lua.create_string(s.as_bytes())?,
                                                )
                                            }
                                            _ => LuaValue::Nil,
                                        };
                                    let crlf = entry
                                        .get("crlf")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);

                                    // Check if doc already opened by the
                                    // normal session restore (dirty edits
                                    // to on-disk files).
                                    let mut existing_doc: Option<LuaTable> = None;
                                    if !new_file {
                                        if let LuaValue::String(ref abs_s) = abs_filename
                                        {
                                            let abs_str = abs_s.to_str()?;
                                            let docs: LuaTable = core.get("docs")?;
                                            for d in docs.sequence_values::<LuaTable>() {
                                                let d = d?;
                                                let da: LuaValue =
                                                    d.get("abs_filename")?;
                                                if let LuaValue::String(ref ds) = da {
                                                    if ds.to_str()? == abs_str {
                                                        existing_doc = Some(d);
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    let (doc, doc_t, need_view) =
                                        if let Some(ed) = existing_doc {
                                            let d = LuaValue::Table(ed.clone());
                                            (d, ed, false)
                                        } else {
                                            // Create a new Doc via the class
                                            // constructor — same pattern as the
                                            // session open_files restore above.
                                            let doc_cls: LuaValue =
                                                require2.call("core.doc")?;
                                            let d: LuaValue = match doc_cls {
                                                LuaValue::Table(ref t) => {
                                                    let mt: Option<LuaTable> =
                                                        t.metatable();
                                                    if let Some(mt) = mt {
                                                        let cf: LuaValue =
                                                            mt.get("__call")?;
                                                        if let LuaValue::Function(f) =
                                                            cf
                                                        {
                                                            f.call::<LuaValue>((
                                                                t.clone(),
                                                                filename.clone(),
                                                                abs_filename.clone(),
                                                                true,
                                                            ))?
                                                        } else {
                                                            continue;
                                                        }
                                                    } else {
                                                        continue;
                                                    }
                                                }
                                                _ => continue,
                                            };
                                            let dt = match &d {
                                                LuaValue::Table(t) => t.clone(),
                                                _ => continue,
                                            };
                                            (d, dt, true)
                                        };

                                    // Clear deferred_load so the lazy-load
                                    // path does not overwrite backup content.
                                    doc_t.set("deferred_load", LuaValue::Nil)?;

                                    // Load backup content into the buffer.
                                    let buf_id: LuaValue = doc_t.get("buffer_id")?;
                                    let load_fn: LuaFunction =
                                        doc_native.get("buffer_load")?;
                                    let pcall: LuaFunction =
                                        lua.globals().get("pcall")?;
                                    let result: LuaMultiValue = pcall.call((
                                        load_fn,
                                        buf_id,
                                        backup_path.clone(),
                                    ))?;
                                    let vals: Vec<LuaValue> = result.into_vec();
                                    if !matches!(
                                        vals.first(),
                                        Some(LuaValue::Boolean(true))
                                    ) {
                                        continue;
                                    }
                                    // Apply the snapshot from buffer_load.
                                    if let Some(LuaValue::Table(snapshot)) =
                                        vals.get(1)
                                    {
                                        let lines: LuaValue = snapshot.get("lines")?;
                                        if let LuaValue::Table(ref lt) = lines {
                                            doc_t.set("lines", lt.clone())?;
                                            let hl: LuaTable =
                                                doc_t.get("highlighter")?;
                                            let hl_lines: LuaTable =
                                                hl.get("lines")?;
                                            for i in 1..=(lt.raw_len() as i64) {
                                                hl_lines.raw_set(i, false)?;
                                            }
                                        }
                                    }

                                    doc_t.set("new_file", new_file)?;
                                    doc_t.set("crlf", crlf)?;

                                    // Restore undo data if present
                                    if let Some(undo_path) = entry.get("undo_path").and_then(|v| v.as_str()) {
                                        if std::path::Path::new(undo_path).exists() {
                                            if let Ok(undo_data) = std::fs::read(undo_path) {
                                                let buf_id: LuaValue = doc_t.get("buffer_id")?;
                                                let set_undo: LuaFunction = doc_native.get("buffer_set_undo_data")?;
                                                let _: LuaValue = set_undo.call((buf_id, undo_data))?;
                                            }
                                        }
                                    }

                                    // Restore selections.
                                    if let Some(serde_json::Value::Array(sel_arr)) =
                                        entry.get("selections")
                                    {
                                        if !sel_arr.is_empty() {
                                            let sel_t = lua.create_table()?;
                                            for (i, v) in sel_arr.iter().enumerate() {
                                                let num = v
                                                    .as_i64()
                                                    .unwrap_or_else(|| {
                                                        v.as_f64()
                                                            .map(|f| f as i64)
                                                            .unwrap_or(1)
                                                    });
                                                sel_t.raw_set(
                                                    (i + 1) as i64,
                                                    num,
                                                )?;
                                            }
                                            doc_t.set("selections", sel_t)?;
                                        }
                                    }

                                    doc_t.call_method::<()>("reset_syntax", ())?;

                                    if need_view {
                                        // Add to core.docs.
                                        let docs: LuaTable = core.get("docs")?;
                                        let table_mod: LuaTable =
                                            lua.globals().get("table")?;
                                        let insert_fn: LuaFunction =
                                            table_mod.get("insert")?;
                                        insert_fn
                                            .call::<()>((docs, doc.clone()))?;

                                        // Create a DocView and add it — same
                                        // pattern as session open_files restore.
                                        let view: LuaValue = match &dv_cls {
                                            LuaValue::Table(t) => {
                                                let mt: Option<LuaTable> =
                                                    t.metatable();
                                                if let Some(mt) = mt {
                                                    let cf: LuaValue =
                                                        mt.get("__call")?;
                                                    if let LuaValue::Function(f) =
                                                        cf
                                                    {
                                                        f.call::<LuaValue>((
                                                            t.clone(),
                                                            doc.clone(),
                                                        ))?
                                                    } else {
                                                        LuaValue::Nil
                                                    }
                                                } else {
                                                    LuaValue::Nil
                                                }
                                            }
                                            _ => LuaValue::Nil,
                                        };
                                        let add_view: LuaFunction =
                                            primary.get("add_view")?;
                                        add_view.call::<()>((
                                            primary.clone(),
                                            view.clone(),
                                        ))?;
                                        if let LuaValue::Table(ref vt) = view {
                                            last_backup_view = Some(vt.clone());
                                        }
                                    }
                                }
                                // No update_layout here — core.step runs it
                                // every frame with the actual window size.
                                // Clean up backup files after successful restore.
                                let backup_dir = std::path::PathBuf::from(&userdir)
                                    .join("backups");
                                let _ = std::fs::remove_dir_all(&backup_dir);
                            }
                        }
                    }

                    // Restore focus: prefer saved_active file, fall back to
                    // the last backup-restored view if saved_active is absent
                    // (e.g. only unsaved files were open).
                    let mut focus_restored = false;
                    if let Some(af_str) = saved_active {
                        let primary: LuaTable = get_primary.call(root_view.clone())?;
                        let root_node: LuaTable = root_view.get("root_node")?;
                        let views: LuaTable = primary.get("views")?;
                        for view in views.sequence_values::<LuaTable>() {
                            let v = view?;
                            if let Some(doc) = v.get::<Option<LuaTable>>("doc")? {
                                if let LuaValue::String(ref s) = doc.get::<LuaValue>("abs_filename")? {
                                    if s.to_str()? == af_str.as_str() {
                                        let get_node: LuaFunction = root_node.get("get_node_for_view")?;
                                        if let LuaValue::Table(node) = get_node.call::<LuaValue>((root_node.clone(), v.clone()))? {
                                            node.call_method::<()>("set_active_view", v)?;
                                            focus_restored = true;
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    if !focus_restored {
                        if let Some(bv) = last_backup_view {
                            let root_node: LuaTable = root_view.get("root_node")?;
                            let get_node: LuaFunction = root_node.get("get_node_for_view")?;
                            if let LuaValue::Table(node) = get_node.call::<LuaValue>((root_node.clone(), bv.clone()))? {
                                node.call_method::<()>("set_active_view", bv)?;
                            }
                        }
                    }

                    core.set("_restoring_session", false)?;
                    Ok(())
                })?;
                add_thread.call::<()>(restore_fn)?;
            }

            // Open log if plugin loading had errors.
            if !plugins_success {
                let add_thread2: LuaFunction = core.get("add_thread")?;
                let open_log = lua.create_function(|lua, ()| {
                    let command_mod: LuaTable = get_module(lua, "core.command")?;
                    let perform: LuaFunction = command_mod.get("perform")?;
                    perform.call::<()>("core:open-log")?;
                    Ok(())
                })?;
                add_thread2.call::<()>(open_log)?;
            }

            // Configure borderless window.
            let configure: LuaFunction = core.get("configure_borderless_window")?;
            configure.call::<()>(())?;

            // Show nag view for refused plugins.
            let userdir_entry: LuaValue = plugins_refuse_list.get("userdir")?;
            let datadir_entry: LuaValue = plugins_refuse_list.get("datadir")?;
            let mut has_refused = false;
            if let LuaValue::Table(ref ud) = userdir_entry {
                let plugins: LuaTable = ud.get("plugins")?;
                if plugins.raw_len() > 0 {
                    has_refused = true;
                }
            }
            if !has_refused {
                if let LuaValue::Table(ref dd) = datadir_entry {
                    let plugins: LuaTable = dd.get("plugins")?;
                    if plugins.raw_len() > 0 {
                        has_refused = true;
                    }
                }
            }

            if has_refused {
                let common: LuaTable = get_module(lua, "core.common")?;
                let home_encode: LuaFunction = common.get("home_encode")?;
                let string_mod: LuaTable = lua.globals().get("string")?;
                let format_fn: LuaFunction = string_mod.get("format")?;
                let table_concat: LuaFunction = table_mod.get("concat")?;

                let opt = lua.create_table()?;
                let opt1 = lua.create_table()?;
                opt1.set("text", "Exit")?;
                opt1.set("default_no", true)?;
                let opt2 = lua.create_table()?;
                opt2.set("text", "Continue")?;
                opt2.set("default_yes", true)?;
                opt.set(1, opt1)?;
                opt.set(2, opt2)?;

                let msg_parts = lua.create_table()?;
                let mut msg_idx = 0;
                for entry_val in [&userdir_entry, &datadir_entry] {
                    if let LuaValue::Table(entry) = entry_val {
                        let plugins: LuaTable = entry.get("plugins")?;
                        if plugins.raw_len() > 0 {
                            let msg_list = lua.create_table()?;
                            let mut ml_idx = 0;
                            for p in plugins.sequence_values::<LuaTable>() {
                                let plugin = p?;
                                let file: String = plugin.get("file")?;
                                let vs: String = plugin.get("version_string")?;
                                let formatted: String =
                                    format_fn.call((
                                        "%s[%s]",
                                        file,
                                        vs,
                                    ))?;
                                ml_idx += 1;
                                msg_list.set(ml_idx, formatted)?;
                            }
                            let dir: String = entry.get("dir")?;
                            let encoded: String = home_encode.call(dir)?;
                            let joined: String = table_concat.call((msg_list, "\n"))?;
                            let part: String = format_fn.call((
                                "Plugins from directory \"%s\":\n%s",
                                encoded,
                                joined,
                            ))?;
                            msg_idx += 1;
                            msg_parts.set(msg_idx, part)?;
                        }
                    }
                }

                let mod_vs: String = lua.globals().get("MOD_VERSION_STRING")?;
                let joined_msg: String = table_concat.call((msg_parts, ".\n\n"))?;
                let body: String = format_fn.call((
                    "Some plugins are not loaded due to version mismatch. Expected version %s.\n\n%s.\n\nPlease download a recent version from https://github.com/lite-xl/lite-xl-plugins.",
                    mod_vs,
                    joined_msg,
                ))?;

                let nag_view2: LuaTable = core.get("nag_view")?;
                let show: LuaFunction = nag_view2.get("show")?;
                let callback = lua.create_function(|lua, item: LuaTable| {
                    let text: String = item.get("text")?;
                    if text == "Exit" {
                        let os_mod: LuaTable = lua.globals().get("os")?;
                        let exit_fn: LuaFunction = os_mod.get("exit")?;
                        exit_fn.call::<()>(1)?;
                    }
                    Ok(())
                })?;
                show.call::<()>((
                    nag_view2,
                    "Refused Plugins",
                    body,
                    opt,
                    callback,
                ))?;
            }

            Ok(())
        })?,
    )?;

    Ok(())
}

/// Registers the inline git subsystem during core.init.
fn register_core_git(lua: &Lua) -> LuaResult<()> {
    let require: LuaFunction = lua.globals().get("require")?;
    let config: LuaTable = get_module(lua, "core.config")?;
    let common: LuaTable = get_module(lua, "core.common")?;
    let _git_native: LuaTable = require.call("git_native")?;

    // Merge default git config.
    let merge: LuaFunction = common.get("merge")?;
    let defaults = lua.create_table()?;
    defaults.set("refresh_interval", 5)?;
    defaults.set("show_branch_in_statusbar", true)?;
    defaults.set("treeview_highlighting", true)?;
    let plugins: LuaTable = config.get("plugins")?;
    let existing_git: LuaValue = plugins.get("git")?;
    let merged: LuaTable = merge.call((defaults, existing_git))?;
    plugins.set("git", merged)?;

    // repos table (local to git subsystem).
    let repos = lua.create_table()?;
    let repos_key = lua.create_registry_value(repos)?;

    let git = lua.create_table()?;

    // sync_repo helper — stored in registry for reuse.
    {
        let repos_key2 =
            lua.create_registry_value(lua.registry_value::<LuaTable>(&repos_key)?.clone())?;
        let sync_repo_fn = lua.create_function(move |lua, root: String| {
            let git_native: LuaTable = get_module(lua, "git_native")?;
            let get_state: LuaFunction = git_native.get("get_state")?;
            let state: LuaValue = get_state.call(root.clone())?;
            if state == LuaValue::Nil {
                return Ok(());
            }
            let s = match state {
                LuaValue::Table(t) => t,
                _ => return Ok(()),
            };
            let repos: LuaTable = lua.registry_value(&repos_key2)?;
            let existing: LuaValue = repos.get(root.as_str())?;
            let r: LuaTable = if let LuaValue::Table(t) = existing {
                t
            } else {
                let t = lua.create_table()?;
                t.set("root", root.clone())?;
                repos.set(root.as_str(), t.clone())?;
                t
            };
            for key in &[
                "branch",
                "ahead",
                "behind",
                "detached",
                "dirty",
                "refreshing",
                "last_refresh",
                "error",
                "ordered",
                "files",
            ] {
                let val: LuaValue = s.get(*key)?;
                r.set(*key, val)?;
            }
            Ok(())
        })?;
        let sync_key = lua.create_registry_value(sync_repo_fn)?;

        // git.get_repo(path)
        {
            let repos_key3 =
                lua.create_registry_value(lua.registry_value::<LuaTable>(&repos_key)?.clone())?;
            git.set(
                "get_repo",
                lua.create_function(move |lua, path: String| {
                    let git_native: LuaTable = get_module(lua, "git_native")?;
                    let get_root: LuaFunction = git_native.get("get_root")?;
                    let root: LuaValue = get_root.call(path)?;
                    if root == LuaValue::Nil {
                        return Ok(LuaValue::Nil);
                    }
                    let root_str: String = match root {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => return Ok(LuaValue::Nil),
                    };
                    let repos: LuaTable = lua.registry_value(&repos_key3)?;
                    let existing: LuaValue = repos.get(root_str.as_str())?;
                    if existing != LuaValue::Nil {
                        return Ok(existing);
                    }
                    let t = lua.create_table()?;
                    t.set("root", root_str.clone())?;
                    t.set("branch", "")?;
                    t.set("ahead", 0)?;
                    t.set("behind", 0)?;
                    t.set("detached", false)?;
                    t.set("dirty", false)?;
                    t.set("refreshing", false)?;
                    t.set("last_refresh", 0)?;
                    t.set("error", LuaValue::Nil)?;
                    t.set("ordered", lua.create_table()?)?;
                    t.set("files", lua.create_table()?)?;
                    repos.set(root_str.as_str(), t.clone())?;
                    Ok(LuaValue::Table(t))
                })?,
            )?;
        }

        // git.get_active_repo()
        git.set(
            "get_active_repo",
            lua.create_function(|lua, ()| {
                let core = get_core(lua)?;
                let active_view: LuaValue = core.get("active_view")?;
                let path: LuaValue = if let LuaValue::Table(ref view) = active_view {
                    let doc_val: LuaValue = view.get("doc")?;
                    if let LuaValue::Table(ref doc) = doc_val {
                        doc.get("abs_filename")?
                    } else {
                        LuaValue::Nil
                    }
                } else {
                    LuaValue::Nil
                };
                let path = if path == LuaValue::Nil {
                    let root_project_fn: LuaFunction = core.get("root_project")?;
                    let rp: LuaValue = root_project_fn.call(())?;
                    if let LuaValue::Table(ref p) = rp {
                        p.get("path")?
                    } else {
                        LuaValue::Nil
                    }
                } else {
                    path
                };
                if path == LuaValue::Nil {
                    return Ok(LuaValue::Nil);
                }
                let git_mod: LuaTable = get_module(lua, "core.git")?;
                let get_repo: LuaFunction = git_mod.get("get_repo")?;
                get_repo.call(path)
            })?,
        )?;

        // git.get_file_status(path)
        git.set(
            "get_file_status",
            lua.create_function(|lua, path: String| {
                let git_native: LuaTable = get_module(lua, "git_native")?;
                let gfs: LuaFunction = git_native.get("get_file_status")?;
                gfs.call::<LuaValue>(path)
            })?,
        )?;

        // git.refresh(path, force?)
        git.set(
            "refresh",
            lua.create_function(|lua, (path, force): (String, Option<bool>)| {
                let git_native: LuaTable = get_module(lua, "git_native")?;
                let config: LuaTable = get_module(lua, "core.config")?;
                let plugins: LuaTable = config.get("plugins")?;
                let git_conf: LuaTable = plugins.get("git")?;
                let interval: LuaValue = git_conf.get("refresh_interval")?;
                let interval_num: f64 = match interval {
                    LuaValue::Integer(i) => i as f64,
                    LuaValue::Number(n) => n,
                    _ => 5.0,
                };
                let get_root: LuaFunction = git_native.get("get_root")?;
                let root: LuaValue = get_root.call(path)?;
                if root == LuaValue::Nil {
                    return Ok(LuaValue::Nil);
                }
                let maybe_refresh: LuaFunction = git_native.get("maybe_refresh")?;
                maybe_refresh.call::<()>((root.clone(), force.unwrap_or(false), interval_num))?;
                let git_mod: LuaTable = get_module(lua, "core.git")?;
                let get_repo: LuaFunction = git_mod.get("get_repo")?;
                get_repo.call(root)
            })?,
        )?;

        // git.run(path, args, on_complete?)
        git.set(
            "run",
            lua.create_function(
                |lua, (path, args, on_complete): (String, LuaTable, Option<LuaFunction>)| {
                    let git_native: LuaTable = get_module(lua, "git_native")?;
                    let get_root: LuaFunction = git_native.get("get_root")?;
                    let root: LuaValue = get_root.call(path)?;
                    if root == LuaValue::Nil {
                        if let Some(cb) = on_complete {
                            cb.call::<()>((false, "", "Not inside a Git repository"))?;
                        }
                        return Ok(());
                    }
                    let start_command: LuaFunction = git_native.get("start_command")?;
                    let handle: LuaValue = start_command.call((root.clone(), args.clone()))?;

                    // Get first arg to check if it's "branch".
                    let first_arg: LuaValue = args.get(1)?;
                    let is_branch = if let LuaValue::String(s) = &first_arg {
                        s.to_str()? == "branch"
                    } else {
                        false
                    };

                    let handle_key = lua.create_registry_value(handle)?;
                    let root_key = lua.create_registry_value(root)?;
                    let on_complete_key = on_complete
                        .map(|f| lua.create_registry_value(f))
                        .transpose()?;

                    // Tick-mode thread: polls git command each cycle without yielding.
                    let tick_fn = lua.create_function(move |lua, ()| -> LuaResult<LuaValue> {
                        let git_native: LuaTable = get_module(lua, "git_native")?;
                        let check: LuaFunction = git_native.get("check_command")?;
                        let handle: LuaValue = lua.registry_value(&handle_key)?;
                        let result: LuaValue = check.call(handle)?;
                        if let LuaValue::Table(ref rt) = result {
                            let ok: bool = rt.get(1)?;
                            let stdout: LuaValue = rt.get(2)?;
                            let stderr: LuaValue = rt.get(3)?;
                            let root: LuaValue = lua.registry_value(&root_key)?;
                            if ok && !is_branch {
                                let start_refresh: LuaFunction = git_native.get("start_refresh")?;
                                start_refresh.call::<()>(root.clone())?;
                            }
                            if let Some(ref cb_key) = on_complete_key {
                                let cb: LuaFunction = lua.registry_value(cb_key)?;
                                cb.call::<()>((ok, stdout, stderr, root))?;
                            }
                            let core = get_core(lua)?;
                            core.set("redraw", true)?;
                            return Ok(LuaValue::Nil);
                        }
                        Ok(LuaValue::Number(0.05))
                    })?;
                    add_tick_thread(lua, tick_fn)?;
                    Ok(())
                },
            )?,
        )?;

        // git.list_branches(path, on_complete)
        git.set(
            "list_branches",
            lua.create_function(|lua, (path, on_complete): (String, LuaFunction)| {
                let git_mod: LuaTable = get_module(lua, "core.git")?;
                let run: LuaFunction = git_mod.get("run")?;
                let args = lua.create_table()?;
                args.set(1, "branch")?;
                args.set(2, "--all")?;
                args.set(3, "--format=%(refname:short)")?;
                let cb_key = lua.create_registry_value(on_complete)?;
                let callback = lua.create_function(
                    move |lua, (ok, stdout, stderr): (bool, String, LuaValue)| {
                        let on_complete: LuaFunction = lua.registry_value(&cb_key)?;
                        if !ok {
                            on_complete.call::<()>((LuaValue::Nil, stderr))?;
                            return Ok(());
                        }
                        let branches = lua.create_table()?;
                        let seen = lua.create_table()?;
                        let mut idx = 0;
                        for line in stdout.lines() {
                            if !line.is_empty() {
                                let already: LuaValue = seen.get(line)?;
                                if already == LuaValue::Nil {
                                    seen.set(line, true)?;
                                    idx += 1;
                                    branches.set(idx, line.to_string())?;
                                }
                            }
                        }
                        let table_mod: LuaTable = lua.globals().get("table")?;
                        let sort: LuaFunction = table_mod.get("sort")?;
                        sort.call::<()>(branches.clone())?;
                        on_complete.call::<()>(branches)?;
                        Ok(())
                    },
                )?;
                run.call::<()>((path, args, callback))?;
                Ok(())
            })?,
        )?;

        // git.stage(path, on_complete?)
        git.set(
            "stage",
            lua.create_function(|lua, (path, on_complete): (String, Option<LuaFunction>)| {
                let git_mod: LuaTable = get_module(lua, "core.git")?;
                let gfs: LuaFunction = git_mod.get("get_file_status")?;
                let entry: LuaValue = gfs.call(path.clone())?;
                let common: LuaTable = get_module(lua, "core.common")?;
                let basename: LuaFunction = common.get("basename")?;
                let rel: String = if let LuaValue::Table(ref e) = entry {
                    e.get("rel")?
                } else {
                    basename.call(path.clone())?
                };
                let run: LuaFunction = git_mod.get("run")?;
                let args = lua.create_table()?;
                args.set(1, "add")?;
                args.set(2, "--")?;
                args.set(3, rel)?;
                run.call::<()>((path, args, on_complete))?;
                Ok(())
            })?,
        )?;

        // git.unstage(path, on_complete?)
        git.set(
            "unstage",
            lua.create_function(|lua, (path, on_complete): (String, Option<LuaFunction>)| {
                let git_mod: LuaTable = get_module(lua, "core.git")?;
                let gfs: LuaFunction = git_mod.get("get_file_status")?;
                let entry: LuaValue = gfs.call(path.clone())?;
                let common: LuaTable = get_module(lua, "core.common")?;
                let basename: LuaFunction = common.get("basename")?;
                let rel: String = if let LuaValue::Table(ref e) = entry {
                    e.get("rel")?
                } else {
                    basename.call(path.clone())?
                };
                let run: LuaFunction = git_mod.get("run")?;
                let args = lua.create_table()?;
                args.set(1, "reset")?;
                args.set(2, "HEAD")?;
                args.set(3, "--")?;
                args.set(4, rel)?;
                run.call::<()>((path, args, on_complete))?;
                Ok(())
            })?,
        )?;

        // git.diff_file(path, cached?, on_complete?)
        git.set(
            "diff_file",
            lua.create_function(
                |lua, (path, cached, on_complete): (String, Option<bool>, Option<LuaFunction>)| {
                    let git_mod: LuaTable = get_module(lua, "core.git")?;
                    let gfs: LuaFunction = git_mod.get("get_file_status")?;
                    let entry: LuaValue = gfs.call(path.clone())?;
                    let common: LuaTable = get_module(lua, "core.common")?;
                    let basename: LuaFunction = common.get("basename")?;
                    let rel: String = if let LuaValue::Table(ref e) = entry {
                        e.get("rel")?
                    } else {
                        basename.call(path.clone())?
                    };
                    let run: LuaFunction = git_mod.get("run")?;
                    let args = lua.create_table()?;
                    let mut idx = 1;
                    args.set(idx, "diff")?;
                    if cached.unwrap_or(false) {
                        idx += 1;
                        args.set(idx, "--cached")?;
                    }
                    idx += 1;
                    args.set(idx, "--")?;
                    idx += 1;
                    args.set(idx, rel)?;
                    run.call::<()>((path, args, on_complete))?;
                    Ok(())
                },
            )?,
        )?;

        // git.diff_repo(path, cached?, on_complete?)
        git.set(
            "diff_repo",
            lua.create_function(
                |lua, (path, cached, on_complete): (String, Option<bool>, Option<LuaFunction>)| {
                    let git_mod: LuaTable = get_module(lua, "core.git")?;
                    let run: LuaFunction = git_mod.get("run")?;
                    let args = lua.create_table()?;
                    args.set(1, "diff")?;
                    if cached.unwrap_or(false) {
                        args.set(2, "--cached")?;
                    }
                    run.call::<()>((path, args, on_complete))?;
                    Ok(())
                },
            )?,
        )?;

        // Store git module.
        let loaded: LuaTable = lua.globals().get::<LuaTable>("package")?.get("loaded")?;
        loaded.set("core.git", git)?;

        // Tick-mode thread for polling git updates. Never terminates.
        let sync_key2 =
            lua.create_registry_value(lua.registry_value::<LuaFunction>(&sync_key)?.clone())?;
        let poll_tick = lua.create_function(move |lua, ()| -> LuaResult<LuaValue> {
            let git_native: LuaTable = get_module(lua, "git_native")?;
            let poll: LuaFunction = git_native.get("poll_updates")?;
            let updated: LuaValue = poll.call(())?;
            if let LuaValue::Table(ref roots) = updated {
                let sync: LuaFunction = lua.registry_value(&sync_key2)?;
                for root in roots.sequence_values::<String>() {
                    let r = root?;
                    sync.call::<()>(r)?;
                }
                let core = get_core(lua)?;
                core.set("redraw", true)?;
            }
            Ok(LuaValue::Number(0.1))
        })?;
        add_tick_thread(lua, poll_tick)?;
    }

    // Require git UI and commands.
    require.call::<()>("core.git.ui")?;
    require.call::<LuaValue>("core.commands.git")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin loader
// ---------------------------------------------------------------------------

fn register_lazy_command_plugin_defs(lua: &Lua, table: &LuaTable) -> LuaResult<()> {
    // markdown_preview
    let mp = lua.create_table()?;
    mp.set("predicate", "core.docview")?;
    let mp_cmds = lua.create_table()?;
    mp_cmds.set(1, "markdown-preview:toggle")?;
    mp.set("commands", mp_cmds)?;
    table.set("markdown_preview", mp)?;

    // projectsearch
    let ps = lua.create_table()?;
    let ps_cmds = lua.create_table()?;
    ps_cmds.set(1, "project-search:find")?;
    ps_cmds.set(2, "project-search:find-regex")?;
    ps_cmds.set(3, "project-search:fuzzy-find")?;
    ps.set("commands", ps_cmds)?;
    table.set("projectsearch", ps)?;

    // projectreplace
    let pr = lua.create_table()?;
    let pr_cmds = lua.create_table()?;
    pr_cmds.set(1, "project-search:replace")?;
    pr_cmds.set(2, "project-search:replace-regex")?;
    pr.set("commands", pr_cmds)?;
    table.set("projectreplace", pr)?;

    // remotessh
    let rs = lua.create_table()?;
    let rs_cmds = lua.create_table()?;
    rs_cmds.set(1, "remote-ssh:open-project")?;
    rs_cmds.set(2, "remote-ssh:add-project")?;
    rs.set("commands", rs_cmds)?;
    table.set("remotessh", rs)?;

    Ok(())
}

fn register_plugin_loader(
    lua: &Lua,
    core: &LuaTable,
    _state_key: &LuaRegistryKey,
    lazy_plugins_key: &LuaRegistryKey,
    lazy_handlers_key: &LuaRegistryKey,
    lazy_loaded_key: &LuaRegistryKey,
) -> LuaResult<()> {
    // parse_plugin_details(path, file, mod_version_regex, priority_regex)
    core.set(
        "parse_plugin_details",
        lua.create_function(
            |lua,
             (path, file, mod_version_regex, priority_regex): (
                String,
                String,
                LuaValue,
                LuaValue,
            )| {
                // Check for generated sidecar files.
                if path.ends_with(".luac")
                    || path.ends_with(".lazy.json")
                    || file.ends_with(".luac")
                    || file.ends_with(".lazy.json")
                {
                    return Ok(LuaValue::Nil);
                }

                let io_mod: LuaTable = lua.globals().get("io")?;
                let open: LuaFunction = io_mod.get("open")?;
                let f_val: LuaValue = open.call((file.clone(), "r"))?;
                if f_val == LuaValue::Nil {
                    return Ok(LuaValue::Boolean(false));
                }

                let pcall: LuaFunction = lua.globals().get("pcall")?;
                let mut priority: LuaValue = LuaValue::Boolean(false);
                let mut version_match = false;
                let mut major: Option<i64> = None;
                let mut minor: i64 = 0;
                let mut patch: i64 = 0;

                let mod_ver_major: i64 = lua.globals().get("MOD_VERSION_MAJOR")?;
                let mod_ver_minor: i64 = lua.globals().get("MOD_VERSION_MINOR")?;
                let mod_ver_patch: i64 = lua.globals().get("MOD_VERSION_PATCH")?;

                // Read lines from file.
                let lines_fn: LuaFunction = match &f_val {
                    LuaValue::UserData(ud) => ud.get("lines")?,
                    LuaValue::Table(t) => t.get("lines")?,
                    _ => return Ok(LuaValue::Boolean(false)),
                };
                let iter: LuaFunction = lines_fn.call(f_val.clone())?;

                loop {
                    let line: LuaValue = iter.call(())?;
                    if line == LuaValue::Nil {
                        break;
                    }

                    if major.is_none() {
                        if let LuaValue::Table(ref regex_obj) = mod_version_regex {
                            let match_fn: LuaFunction = regex_obj.get("match")?;
                            let result: LuaMultiValue =
                                pcall.call((match_fn.clone(), regex_obj.clone(), line.clone()))?;
                            let vals: Vec<LuaValue> = result.into_vec();
                            let ok = vals
                                .first()
                                .and_then(|v| {
                                    if let LuaValue::Boolean(b) = v {
                                        Some(*b)
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(false);
                            if ok && vals.len() >= 2 {
                                if let Some(maj_val) = vals.get(1) {
                                    if *maj_val != LuaValue::Nil {
                                        let tonumber: LuaFunction =
                                            lua.globals().get("tonumber")?;
                                        let m: i64 = tonumber
                                            .call::<LuaValue>(maj_val.clone())?
                                            .as_integer()
                                            .unwrap_or(0);
                                        let mi: i64 = if let Some(v) = vals.get(2) {
                                            tonumber
                                                .call::<LuaValue>(v.clone())?
                                                .as_integer()
                                                .unwrap_or(0)
                                        } else {
                                            0
                                        };
                                        let p: i64 = if let Some(v) = vals.get(3) {
                                            tonumber
                                                .call::<LuaValue>(v.clone())?
                                                .as_integer()
                                                .unwrap_or(0)
                                        } else {
                                            0
                                        };
                                        major = Some(m);
                                        minor = mi;
                                        patch = p;

                                        version_match = m == mod_ver_major
                                            && mi <= mod_ver_minor
                                            && p <= mod_ver_patch;
                                    }
                                }
                            }
                        }
                    }

                    if priority == LuaValue::Boolean(false) {
                        if let LuaValue::Table(ref regex_obj) = priority_regex {
                            let match_fn: LuaFunction = regex_obj.get("match")?;
                            let result: LuaMultiValue =
                                pcall.call((match_fn.clone(), regex_obj.clone(), line.clone()))?;
                            let vals: Vec<LuaValue> = result.into_vec();
                            let ok = vals
                                .first()
                                .and_then(|v| {
                                    if let LuaValue::Boolean(b) = v {
                                        Some(*b)
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(false);
                            if ok {
                                if let Some(p_val) = vals.get(1) {
                                    if *p_val != LuaValue::Nil {
                                        let tonumber: LuaFunction =
                                            lua.globals().get("tonumber")?;
                                        priority = tonumber.call(p_val.clone())?;
                                    }
                                }
                            }
                        }
                    }

                    if version_match {
                        break;
                    }
                }

                // Close file.
                let close_fn: LuaFunction = match &f_val {
                    LuaValue::UserData(ud) => ud.get("close")?,
                    LuaValue::Table(t) => t.get("close")?,
                    _ => return Ok(LuaValue::Boolean(false)),
                };
                close_fn.call::<()>(f_val)?;

                // Check bundled plugin path.
                let datadir: String = lua.globals().get("DATADIR")?;
                let pathsep: String = lua.globals().get("PATHSEP")?;
                let bundled_prefix = format!("{}{}plugins", datadir, pathsep);
                if path.starts_with(&bundled_prefix) || file.starts_with(&bundled_prefix) {
                    version_match = true;
                }

                let version = lua.create_table()?;
                if let Some(m) = major {
                    version.set(1, m)?;
                    version.set(2, minor)?;
                    version.set(3, patch)?;
                }

                let common: LuaTable = get_module(lua, "core.common")?;
                let basename: LuaFunction = common.get("basename")?;
                let name: String = basename.call(path)?;

                let version_string = if major.is_some() {
                    let table_mod: LuaTable = lua.globals().get("table")?;
                    let concat: LuaFunction = table_mod.get("concat")?;
                    concat.call::<String>((version.clone(), "."))?
                } else {
                    "unknown".to_string()
                };

                let priority_val = if priority == LuaValue::Boolean(false) {
                    LuaValue::Integer(100)
                } else {
                    priority
                };

                let details = lua.create_table()?;
                details.set("name", name)?;
                details.set("file", file)?;
                details.set("version_match", version_match)?;
                details.set("version", version)?;
                details.set("priority", priority_val)?;
                details.set("version_string", version_string)?;

                Ok(LuaValue::Table(details))
            },
        )?,
    )?;

    // get_plugin_details(path)
    core.set(
        "get_plugin_details",
        lua.create_function(|lua, path: String| {
            let system: LuaTable = lua.globals().get("system")?;
            let get_file_info: LuaFunction = system.get("get_file_info")?;
            let pathsep: String = lua.globals().get("PATHSEP")?;
            let info: LuaValue = get_file_info.call(path.clone())?;
            let mut file = path.clone();
            let mut info_val = info;
            if let LuaValue::Table(ref t) = info_val {
                let ftype: String = t.get("type")?;
                if ftype == "dir" {
                    file = format!("{}{}init.lua", path, pathsep);
                    info_val = get_file_info.call(file.clone())?;
                }
            }

            if info_val == LuaValue::Nil {
                return Ok(LuaValue::Nil);
            }

            let core = get_core(lua)?;
            let regex_mod: LuaTable = lua.globals().get("regex")?;
            let compile: LuaFunction = regex_mod.get("compile")?;
            let mod_ver_re: LuaValue =
                compile.call("--.*mod-version:(\\d+)(?:\\.(\\d+))?(?:\\.(\\d+))?(?:$|\\s)")?;
            let prio_re: LuaValue = compile.call("\\-\\-.*priority\\s*:\\s*(\\-?[\\d\\.]+)")?;

            let parse: LuaFunction = core.get("parse_plugin_details")?;
            // Strip .lua extension for name.
            let name_path = if path.ends_with(".lua") {
                path[..path.len() - 4].to_string()
            } else {
                path
            };
            let details: LuaValue = parse.call((name_path, file, mod_ver_re, prio_re))?;

            if let LuaValue::Table(ref dt) = details {
                // Set load function to require_lua_plugin.
                let load_fn = lua.create_function(|lua, plugin: LuaTable| {
                    let name: String = plugin.get("name")?;
                    let require: LuaFunction = lua.globals().get("require")?;
                    require.call::<LuaValue>(format!("plugins.{}", name))
                })?;
                dt.set("load", load_fn)?;
            }

            Ok(details)
        })?,
    )?;

    // add_plugins(plugins)
    core.set(
        "add_plugins",
        lua.create_function(|lua, plugins: LuaTable| {
            let core = get_core(lua)?;
            let plugin_list: LuaTable = core.get("plugin_list")?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            let insert: LuaFunction = table_mod.get("insert")?;

            for v in plugins.sequence_values::<LuaValue>() {
                let val = v?;
                insert.call::<()>((plugin_list.clone(), val))?;
            }

            let sort: LuaFunction = table_mod.get("sort")?;
            let cmp = lua.create_function(|_, (a, b): (LuaTable, LuaTable)| {
                let pa: LuaValue = a.get("priority")?;
                let pb: LuaValue = b.get("priority")?;
                let pa_num = match pa {
                    LuaValue::Integer(i) => i as f64,
                    LuaValue::Number(n) => n,
                    _ => 100.0,
                };
                let pb_num = match pb {
                    LuaValue::Integer(i) => i as f64,
                    LuaValue::Number(n) => n,
                    _ => 100.0,
                };
                if (pa_num - pb_num).abs() > f64::EPSILON {
                    return Ok(pa_num < pb_num);
                }
                let na: String = a.get::<Option<String>>("name")?.unwrap_or_default();
                let nb: String = b.get::<Option<String>>("name")?.unwrap_or_default();
                Ok(na < nb)
            })?;
            sort.call::<()>((plugin_list, cmp))?;
            Ok(())
        })?,
    )?;

    // load_plugins()
    {
        let lazy_plugins_key2 =
            lua.create_registry_value(lua.registry_value::<LuaTable>(lazy_plugins_key)?.clone())?;
        let lazy_handlers_key2 =
            lua.create_registry_value(lua.registry_value::<LuaTable>(lazy_handlers_key)?.clone())?;
        let lazy_loaded_key2 =
            lua.create_registry_value(lua.registry_value::<LuaTable>(lazy_loaded_key)?.clone())?;

        core.set(
            "load_plugins",
            lua.create_function(move |lua, ()| {
                let core = get_core(lua)?;
                let config: LuaTable = get_module(lua, "core.config")?;
                let common: LuaTable = get_module(lua, "core.common")?;
                let system_t: LuaTable = lua.globals().get("system")?;
                let table_mod: LuaTable = lua.globals().get("table")?;
                let insert: LuaFunction = table_mod.get("insert")?;
                let pathsep: String = lua.globals().get("PATHSEP")?;
                let userdir: String = lua.globals().get("USERDIR")?;
                let datadir: String = lua.globals().get("DATADIR")?;
                let mut no_errors = true;
                let refused_list = lua.create_table()?;
                let ud_entry = lua.create_table()?;
                ud_entry.set("dir", userdir.clone())?;
                ud_entry.set("plugins", lua.create_table()?)?;
                let dd_entry = lua.create_table()?;
                dd_entry.set("dir", datadir.clone())?;
                dd_entry.set("plugins", lua.create_table()?)?;
                refused_list.set("userdir", ud_entry)?;
                refused_list.set("datadir", dd_entry)?;

                let files = lua.create_table()?;
                let ordered = lua.create_table()?;

                // User config.
                let get_config_fn: LuaFunction = core.get("_get_user_config_filename")?;
                let config_filename: String = get_config_fn.call(())?;
                let user_config = lua.create_table()?;
                user_config.set("priority", -3)?;
                // load_user_config_if_exists
                let load_user_config = lua.create_function(|lua, plugin: LuaTable| {
                    let system_t: LuaTable = lua.globals().get("system")?;
                    let get_file_info: LuaFunction = system_t.get("get_file_info")?;
                    let file: String = plugin.get("file")?;
                    let info: LuaValue = get_file_info.call(file.clone())?;
                    if info != LuaValue::Nil {
                        let rawset: LuaFunction = lua.globals().get("rawset")?;
                        rawset.call::<()>((
                            lua.globals(),
                            "__lite_anvil_user_config_loaded",
                            true,
                        ))?;
                        let dofile: LuaFunction = lua.globals().get("dofile")?;
                        let result: LuaValue = dofile.call(file)?;
                        let require: LuaFunction = lua.globals().get("require")?;
                        let style: LuaTable = require.call("core.style")?;
                        let apply: LuaFunction = style.get("apply_config")?;
                        apply.call::<()>(())?;
                        return Ok(result);
                    }
                    Ok(LuaValue::Nil)
                })?;
                user_config.set("load", load_user_config)?;
                user_config.set("version_match", true)?;
                user_config.set("file", config_filename)?;
                user_config.set("name", "User Config")?;
                ordered.set(1, user_config)?;

                // User module.
                let get_init_fn: LuaFunction = core.get("_get_user_init_filename")?;
                let init_filename: String = get_init_fn.call(())?;
                let user_module = lua.create_table()?;
                user_module.set("priority", -2)?;
                let load_user_module = lua.create_function(|lua, plugin: LuaTable| {
                    let system_t: LuaTable = lua.globals().get("system")?;
                    let get_file_info: LuaFunction = system_t.get("get_file_info")?;
                    let file: String = plugin.get("file")?;
                    let info: LuaValue = get_file_info.call(file.clone())?;
                    if info == LuaValue::Nil {
                        return Ok(LuaValue::Nil);
                    }
                    let dofile: LuaFunction = lua.globals().get("dofile")?;
                    let result: LuaValue = dofile.call(file)?;
                    let name: String = plugin.get("name")?;
                    if name == "User Module" {
                        let require: LuaFunction = lua.globals().get("require")?;
                        let style: LuaTable = require.call("core.style")?;
                        let apply: LuaFunction = style.get("apply_config")?;
                        apply.call::<()>(())?;
                    }
                    Ok(result)
                })?;
                user_module.set("load", load_user_module.clone())?;
                user_module.set("version_match", true)?;
                user_module.set("file", init_filename)?;
                user_module.set("name", "User Module")?;
                ordered.set(2, user_module)?;

                // Project module.
                let root_project_fn: LuaFunction = core.get("root_project")?;
                let rp: LuaValue = root_project_fn.call(())?;
                let mut ordered_len: i64 = 2;
                if let LuaValue::Table(ref rp_t) = rp {
                    let rp_path: LuaValue = rp_t.get("path")?;
                    if let LuaValue::String(ref p) = rp_path {
                        let project_module = lua.create_table()?;
                        project_module.set("priority", -1)?;
                        project_module.set("load", load_user_module)?;
                        project_module.set("version_match", true)?;
                        project_module.set(
                            "file",
                            format!("{}{}.lite_project.lua", p.to_str()?, pathsep),
                        )?;
                        project_module.set("name", "Project Module")?;
                        ordered_len += 1;
                        ordered.set(ordered_len, project_module)?;
                    }
                }

                // Rust-native bundled plugins.
                let package: LuaTable = lua.globals().get("package")?;
                let native_plugins: LuaValue = package.get("native_plugins")?;
                if let LuaValue::Table(ref np) = native_plugins {
                    for name in np.sequence_values::<String>() {
                        let name = name?;
                        files.set(format!("{}.lua", name), true)?;
                        let entry = lua.create_table()?;
                        entry.set("priority", 0)?;
                        let require_plugin = lua.create_function(|lua, plugin: LuaTable| {
                            let name: String = plugin.get("name")?;
                            let require: LuaFunction = lua.globals().get("require")?;
                            require.call::<LuaValue>(format!("plugins.{}", name))
                        })?;
                        entry.set("load", require_plugin)?;
                        entry.set("version_match", true)?;
                        entry.set(
                            "file",
                            format!("{}{}plugins{}{}.lua", datadir, pathsep, pathsep, name),
                        )?;
                        entry.set("name", name)?;
                        ordered_len += 1;
                        ordered.set(ordered_len, entry)?;
                    }
                }

                // Scan plugin directories.
                let list_dir: LuaFunction = system_t.get("list_dir")?;
                for root_dir in &[datadir.clone(), userdir.clone()] {
                    let plugin_dir = format!("{}{}plugins", root_dir, pathsep);
                    let entries: LuaValue = list_dir.call(plugin_dir.clone())?;
                    if let LuaValue::Table(ref entries_t) = entries {
                        for filename in entries_t.sequence_values::<String>() {
                            let filename = filename?;
                            let already: LuaValue = files.get(filename.as_str())?;
                            if already == LuaValue::Nil || already == LuaValue::Boolean(false) {
                                let full_path = format!("{}{}{}", plugin_dir, pathsep, filename);
                                let get_details: LuaFunction = core.get("get_plugin_details")?;
                                let details: LuaValue = get_details.call(full_path)?;
                                if let LuaValue::Table(ref dt) = details {
                                    ordered_len += 1;
                                    ordered.set(ordered_len, dt.clone())?;
                                }
                            }
                            files.set(filename.as_str(), plugin_dir.clone())?;
                        }
                    }
                }

                let add_plugins: LuaFunction = core.get("add_plugins")?;
                add_plugins.call::<()>(ordered)?;

                // Load plugins.
                let load_start: f64 = {
                    let get_time: LuaFunction = system_t.get("get_time")?;
                    get_time.call(())?
                };
                let plugin_list: LuaTable = core.get("plugin_list")?;
                let plugin_count = plugin_list.raw_len();
                let log_quiet: LuaFunction = core.get("log_quiet")?;
                let try_fn: LuaFunction = core.get("try")?;
                let dirname: LuaFunction = common.get("dirname")?;
                let skip_version: LuaValue = config.get("skip_plugins_version")?;
                let skip_version_check = skip_version == LuaValue::Boolean(true);

                let lazy_plugins: LuaTable = lua.registry_value(&lazy_plugins_key2)?;
                let lazy_handlers: LuaTable = lua.registry_value(&lazy_handlers_key2)?;
                let lazy_loaded: LuaTable = lua.registry_value(&lazy_loaded_key2)?;

                let plugins_conf: LuaTable = config.get("plugins")?;

                for i in 1..=plugin_count {
                    let plugin_val: LuaValue = plugin_list.get(i)?;
                    let plugin = match plugin_val {
                        LuaValue::Table(t) => t,
                        _ => continue,
                    };
                    let name: String = match plugin.get::<LuaValue>("name")? {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => continue,
                    };
                    let version_match: bool = plugin
                        .get::<LuaValue>("version_match")?
                        .eq(&LuaValue::Boolean(true));
                    let file: String = match plugin.get::<LuaValue>("file")? {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => continue,
                    };

                    if !skip_version_check && !version_match {
                        let vs: String = plugin
                            .get::<Option<String>>("version_string")?
                            .unwrap_or_else(|| "unknown".to_string());
                        let dir: String = dirname.call(file.clone())?;
                        log_quiet.call::<()>((
                            "Version mismatch for plugin %q[%s] from %s",
                            name.clone(),
                            vs.clone(),
                            dir,
                        ))?;
                        let rlist = if file.starts_with(&userdir) {
                            "userdir"
                        } else {
                            "datadir"
                        };
                        let rl: LuaTable = refused_list.get(rlist)?;
                        let rl_plugins: LuaTable = rl.get("plugins")?;
                        insert.call::<()>((rl_plugins, plugin.clone()))?;
                        continue;
                    }

                    // Check LSP skip.
                    if name == "lsp" {
                        let lsp_conf: LuaValue = config.get("lsp")?;
                        if let LuaValue::Table(ref lc) = lsp_conf {
                            let load_on_startup: LuaValue = lc.get("load_on_startup")?;
                            if load_on_startup == LuaValue::Boolean(false) {
                                log_quiet.call::<()>((
                                    "Skipped plugin %q due to config.lsp.load_on_startup = false",
                                    name.clone(),
                                ))?;
                                continue;
                            }
                        }
                    }

                    let plugin_conf: LuaValue = plugins_conf.get(name.as_str())?;
                    if plugin_conf == LuaValue::Boolean(false) {
                        continue;
                    }

                    // Lazy language plugins.
                    if name.starts_with("language_") {
                        let require: LuaFunction = lua.globals().get("require")?;
                        let syntax: LuaTable = require.call("core.syntax")?;
                        let register_lazy: LuaFunction = syntax.get("register_lazy_plugin")?;
                        register_lazy.call::<()>(plugin.clone())?;
                        continue;
                    }

                    // Lazy command plugins.
                    let lazy_spec: LuaValue = lazy_plugins.get(name.as_str())?;
                    if let LuaValue::Table(ref spec) = lazy_spec {
                        let commands: LuaTable = spec.get("commands")?;
                        let predicate: LuaValue = spec.get("predicate")?;
                        let command_mod: LuaTable = get_module(lua, "core.command")?;
                        let add_cmd: LuaFunction = command_mod.get("add")?;
                        let map = lua.create_table()?;

                        for cmd in commands.sequence_values::<String>() {
                            let command_name = cmd?;
                            let plugin_clone = plugin.clone();
                            let lazy_loaded_clone = lazy_loaded.clone();
                            let lazy_handlers_clone = lazy_handlers.clone();
                            let cmd_name_clone = command_name.clone();
                            let handler =
                                lua.create_function(move |lua, args: LuaMultiValue| {
                                    let name: String = plugin_clone.get("name")?;
                                    let already: LuaValue = lazy_loaded_clone.get(name.as_str())?;
                                    if already == LuaValue::Nil
                                        || already == LuaValue::Boolean(false)
                                    {
                                        lazy_loaded_clone.set(name.as_str(), true)?;
                                        let core = get_core(lua)?;
                                        let try_fn: LuaFunction = core.get("try")?;
                                        let load_fn: LuaFunction = plugin_clone.get("load")?;
                                        let start_time: f64 = {
                                            let system_t: LuaTable = lua.globals().get("system")?;
                                            let gt: LuaFunction = system_t.get("get_time")?;
                                            gt.call(())?
                                        };
                                        let result: LuaMultiValue =
                                            try_fn.call((load_fn, plugin_clone.clone()))?;
                                        let vals: Vec<LuaValue> = result.into_vec();
                                        let ok = vals
                                            .first()
                                            .and_then(|v| {
                                                if let LuaValue::Boolean(b) = v {
                                                    Some(*b)
                                                } else {
                                                    None
                                                }
                                            })
                                            .unwrap_or(false);
                                        if !ok {
                                            return Ok(LuaValue::Nil);
                                        }
                                        let end_time: f64 = {
                                            let system_t: LuaTable = lua.globals().get("system")?;
                                            let gt: LuaFunction = system_t.get("get_time")?;
                                            gt.call(())?
                                        };
                                        let common: LuaTable = get_module(lua, "core.common")?;
                                        let dirname_fn: LuaFunction = common.get("dirname")?;
                                        let file: String = plugin_clone.get("file")?;
                                        let dir: String = dirname_fn.call(file)?;
                                        let log_quiet: LuaFunction = core.get("log_quiet")?;
                                        log_quiet.call::<()>((
                                            "Lazy-loaded plugin %q from %s in %.1fms",
                                            name.clone(),
                                            dir,
                                            (end_time - start_time) * 1000.0,
                                        ))?;
                                        let config: LuaTable = get_module(lua, "core.config")?;
                                        let plugins_c: LuaTable = config.get("plugins")?;
                                        let pc: LuaValue = plugins_c.get(name.as_str())?;
                                        if let LuaValue::Table(ref pct) = pc {
                                            let onload: LuaValue = pct.get("onload")?;
                                            if let LuaValue::Function(f) = onload {
                                                let try2: LuaFunction = core.get("try")?;
                                                let loaded =
                                                    vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                                                try2.call::<()>((f, loaded))?;
                                            }
                                        }
                                    }
                                    // Now perform the real command.
                                    let command_mod: LuaTable = get_module(lua, "core.command")?;
                                    let cmd_map: LuaTable = command_mod.get("map")?;
                                    let loaded_cmd: LuaValue =
                                        cmd_map.get(cmd_name_clone.as_str())?;
                                    if let LuaValue::Table(ref lc) = loaded_cmd {
                                        let perform: LuaValue = lc.get("perform")?;
                                        let existing_handler: LuaValue =
                                            lazy_handlers_clone.get(cmd_name_clone.as_str())?;
                                        if perform != existing_handler {
                                            let cmd_perform: LuaFunction =
                                                command_mod.get("perform")?;
                                            return cmd_perform
                                                .call::<LuaValue>((cmd_name_clone.clone(), args));
                                        }
                                    }
                                    let core2 = get_core(lua)?;
                                    let warn: LuaFunction = core2.get("warn")?;
                                    let pname: String = plugin_clone.get("name")?;
                                    warn.call::<()>((
                                        "Lazy command %q did not register after loading plugin %q",
                                        cmd_name_clone.clone(),
                                        pname,
                                    ))?;
                                    Ok(LuaValue::Nil)
                                })?;
                            lazy_handlers.set(command_name.as_str(), handler.clone())?;
                            map.set(command_name, handler)?;
                        }

                        add_cmd.call::<()>((predicate, map))?;
                        continue;
                    }

                    // Normal plugin loading.
                    let load_fn: LuaFunction = plugin.get("load")?;
                    let result: LuaMultiValue = try_fn.call((load_fn, plugin.clone()))?;
                    let vals: Vec<LuaValue> = result.into_vec();
                    let ok = vals
                        .first()
                        .and_then(|v| {
                            if let LuaValue::Boolean(b) = v {
                                Some(*b)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(false);

                    if ok {
                        let pc: LuaValue = plugins_conf.get(name.as_str())?;
                        if let LuaValue::Table(ref pct) = pc {
                            let onload: LuaValue = pct.get("onload")?;
                            if let LuaValue::Function(f) = onload {
                                let loaded = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                                try_fn.call::<()>((f, loaded))?;
                            }
                        }
                    } else {
                        no_errors = false;
                    }
                }

                let end_time: f64 = {
                    let get_time: LuaFunction = system_t.get("get_time")?;
                    get_time.call(())?
                };
                log_quiet.call::<()>((
                    "Loaded all plugins in %.1fms",
                    (end_time - load_start) * 1000.0,
                ))?;

                Ok(LuaMultiValue::from_vec(vec![
                    LuaValue::Boolean(no_errors),
                    LuaValue::Table(refused_list),
                ]))
            })?,
        )?;
    }

    // reload_module(name)
    core.set(
        "reload_module",
        lua.create_function(|lua, name: String| {
            let package: LuaTable = lua.globals().get("package")?;
            let loaded: LuaTable = package.get("loaded")?;
            let old: LuaValue = loaded.get(name.as_str())?;
            loaded.set(name.as_str(), LuaValue::Nil)?;
            let require: LuaFunction = lua.globals().get("require")?;
            let new: LuaValue = require.call(name.clone())?;
            if let LuaValue::Table(ref old_t) = old {
                if let LuaValue::Table(ref new_t) = new {
                    for pair in new_t.pairs::<LuaValue, LuaValue>() {
                        let (k, v) = pair?;
                        old_t.set(k, v)?;
                    }
                }
                loaded.set(name.as_str(), old)?;
            }
            if name.starts_with("colors.") {
                let style: LuaTable = require.call("core.style")?;
                let apply: LuaFunction = style.get("apply_config")?;
                apply.call::<()>(())?;
            }
            Ok(())
        })?,
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn register_logging(lua: &Lua, core: &LuaTable, state_key: &LuaRegistryKey) -> LuaResult<()> {
    let _ = state_key;

    // custom_log(level, show, backtrace, fmt, ...)
    core.set(
        "custom_log",
        lua.create_function(|lua, args: LuaMultiValue| {
            let vals: Vec<LuaValue> = args.into_vec();
            let level: String = match vals.first() {
                Some(LuaValue::String(s)) => s.to_str()?.to_string(),
                _ => "INFO".to_string(),
            };
            let show = matches!(vals.get(1), Some(LuaValue::Boolean(true)));
            let backtrace = matches!(vals.get(2), Some(LuaValue::Boolean(true)));

            // Format text using string.format.
            let string_mod: LuaTable = lua.globals().get("string")?;
            let format_fn: LuaFunction = string_mod.get("format")?;
            let fmt_args: Vec<LuaValue> = vals.into_iter().skip(3).collect();
            let text: String = format_fn
                .call::<LuaValue>(LuaMultiValue::from_vec(fmt_args))?
                .as_string()
                .and_then(|s| s.to_str().ok().map(|s| s.to_string()))
                .unwrap_or_default();

            let core = get_core(lua)?;
            if show {
                let style: LuaTable = get_module(lua, "core.style")?;
                let log: LuaTable = style.get("log")?;
                let s: LuaTable = log.get(level.as_str())?;
                let status_view: LuaValue = core.get("status_view")?;
                if let LuaValue::Table(ref sv) = status_view {
                    let icon: LuaValue = s.get("icon")?;
                    let color: LuaValue = s.get("color")?;
                    let show_msg: LuaFunction = sv.get("show_message")?;
                    show_msg.call::<()>((sv.clone(), icon, color, text.clone()))?;
                }
            }

            let debug_mod: LuaTable = lua.globals().get("debug")?;
            let getinfo: LuaFunction = debug_mod.get("getinfo")?;
            let info: LuaTable = getinfo.call((2, "Sl"))?;
            let short_src: String = info.get("short_src")?;
            let currentline: i64 = info.get("currentline")?;
            let at = if short_src == "[C]" && currentline == 0 {
                String::new()
            } else {
                format!("{}:{}", short_src, currentline)
            };

            let traceback_info: LuaValue = if backtrace {
                let traceback: LuaFunction = debug_mod.get("traceback")?;
                let tb: String = traceback.call(("", 2))?;
                let cleaned = tb.replace('\t', "");
                LuaValue::String(lua.create_string(cleaned.as_bytes())?)
            } else {
                LuaValue::Nil
            };

            let os_mod: LuaTable = lua.globals().get("os")?;
            let time_fn: LuaFunction = os_mod.get("time")?;
            let time: LuaValue = time_fn.call(())?;

            let item = lua.create_table()?;
            item.set("level", level)?;
            item.set("text", text)?;
            item.set("time", time)?;
            item.set("at", at)?;
            item.set("info", traceback_info)?;

            let log_items: LuaTable = core.get("log_items")?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            let insert: LuaFunction = table_mod.get("insert")?;
            insert.call::<()>((log_items.clone(), item.clone()))?;

            let config: LuaTable = get_module(lua, "core.config")?;
            let max_log: i64 = config.get("max_log_items")?;
            if log_items.raw_len() as i64 > max_log {
                let remove: LuaFunction = table_mod.get("remove")?;
                remove.call::<()>((log_items, 1))?;
            }

            Ok(item)
        })?,
    )?;

    // log(...)
    core.set(
        "log",
        lua.create_function(|lua, args: LuaMultiValue| {
            let core = get_core(lua)?;
            let custom_log: LuaFunction = core.get("custom_log")?;
            let mut full_args = vec![
                LuaValue::String(lua.create_string("INFO")?),
                LuaValue::Boolean(true),
                LuaValue::Boolean(false),
            ];
            full_args.extend(args.into_vec());
            custom_log.call::<LuaValue>(LuaMultiValue::from_vec(full_args))
        })?,
    )?;

    // log_quiet(...)
    core.set(
        "log_quiet",
        lua.create_function(|lua, args: LuaMultiValue| {
            let core = get_core(lua)?;
            let custom_log: LuaFunction = core.get("custom_log")?;
            let mut full_args = vec![
                LuaValue::String(lua.create_string("INFO")?),
                LuaValue::Boolean(false),
                LuaValue::Boolean(false),
            ];
            full_args.extend(args.into_vec());
            custom_log.call::<LuaValue>(LuaMultiValue::from_vec(full_args))
        })?,
    )?;

    // warn(...)
    core.set(
        "warn",
        lua.create_function(|lua, args: LuaMultiValue| {
            let core = get_core(lua)?;
            let custom_log: LuaFunction = core.get("custom_log")?;
            let mut full_args = vec![
                LuaValue::String(lua.create_string("WARN")?),
                LuaValue::Boolean(true),
                LuaValue::Boolean(true),
            ];
            full_args.extend(args.into_vec());
            custom_log.call::<LuaValue>(LuaMultiValue::from_vec(full_args))
        })?,
    )?;

    // error(...)
    core.set(
        "error",
        lua.create_function(|lua, args: LuaMultiValue| {
            let core = get_core(lua)?;
            let custom_log: LuaFunction = core.get("custom_log")?;
            let mut full_args = vec![
                LuaValue::String(lua.create_string("ERROR")?),
                LuaValue::Boolean(true),
                LuaValue::Boolean(true),
            ];
            full_args.extend(args.into_vec());
            custom_log.call::<LuaValue>(LuaMultiValue::from_vec(full_args))
        })?,
    )?;

    // get_log(i?)
    core.set(
        "get_log",
        lua.create_function(|lua, i: LuaValue| {
            let core = get_core(lua)?;
            let log_items: LuaTable = core.get("log_items")?;
            let os_mod: LuaTable = lua.globals().get("os")?;
            let date_fn: LuaFunction = os_mod.get("date")?;
            let string_mod: LuaTable = lua.globals().get("string")?;
            let format_fn: LuaFunction = string_mod.get("format")?;

            let format_item = |item: LuaTable| -> LuaResult<String> {
                let time: LuaValue = item.get("time")?;
                let level: String = item.get("level")?;
                let text: String = item.get("text")?;
                let at: String = item.get("at")?;
                let date_str: String = date_fn.call((LuaValue::Nil, time))?;
                let mut result: String =
                    format_fn.call(("%s [%s] %s at %s", date_str, level, text, at))?;
                let info: LuaValue = item.get("info")?;
                if let LuaValue::String(ref s) = info {
                    result =
                        format_fn.call::<String>(("%s\n%s\n", result, s.to_str()?.to_string()))?;
                }
                Ok(result)
            };

            if i == LuaValue::Nil {
                let r = lua.create_table()?;
                let mut idx = 0;
                for item in log_items.sequence_values::<LuaTable>() {
                    let item = item?;
                    idx += 1;
                    r.set(idx, format_item(item)?)?;
                }
                let table_mod: LuaTable = lua.globals().get("table")?;
                let concat: LuaFunction = table_mod.get("concat")?;
                let result: String = concat.call((r, "\n"))?;
                return Ok(LuaValue::String(lua.create_string(result.as_bytes())?));
            }

            let item: LuaTable = if let LuaValue::Integer(n) = i {
                log_items.get(n)?
            } else if let LuaValue::Table(t) = i {
                t
            } else {
                return Ok(LuaValue::Nil);
            };

            let result = format_item(item)?;
            Ok(LuaValue::String(lua.create_string(result.as_bytes())?))
        })?,
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Doc functions
// ---------------------------------------------------------------------------

fn register_doc_fns(lua: &Lua, core: &LuaTable) -> LuaResult<()> {
    // open_doc(filename?, options?)
    core.set(
        "open_doc",
        lua.create_function(
            |lua, (filename, options): (Option<String>, Option<LuaTable>)| {
                let core = get_core(lua)?;
                let common: LuaTable = get_module(lua, "core.common")?;
                let config: LuaTable = get_module(lua, "core.config")?;
                let system_t: LuaTable = lua.globals().get("system")?;
                let table_mod: LuaTable = lua.globals().get("table")?;
                let insert: LuaFunction = table_mod.get("insert")?;
                let string_mod: LuaTable = lua.globals().get("string")?;
                let format_fn: LuaFunction = string_mod.get("format")?;
                let require: LuaFunction = lua.globals().get("require")?;

                let options = options.unwrap_or(lua.create_table()?);
                let mut new_file = true;
                let mut abs_filename: LuaValue = LuaValue::Nil;
                let mut open_options: LuaValue = LuaValue::Nil;

                if let Some(ref fname) = filename {
                    let root_project_fn: LuaFunction = core.get("root_project")?;
                    let rp: LuaValue = root_project_fn.call(())?;

                    let normalize_path: LuaFunction = common.get("normalize_path")?;
                    let normalized: String = if let LuaValue::Table(ref p) = rp {
                        let np: LuaFunction = p.get("normalize_path")?;
                        np.call((p.clone(), fname.clone()))?
                    } else {
                        normalize_path.call(fname.clone())?
                    };

                    let abs: String = if let LuaValue::Table(ref p) = rp {
                        let ap: LuaFunction = p.get("absolute_path")?;
                        ap.call((p.clone(), normalized.clone()))?
                    } else {
                        let abs_fn: LuaFunction = system_t.get("absolute_path")?;
                        abs_fn.call(normalized.clone())?
                    };

                    abs_filename = LuaValue::String(lua.create_string(abs.as_bytes())?);

                    let get_file_info: LuaFunction = system_t.get("get_file_info")?;
                    let info: LuaValue = get_file_info.call(abs.clone())?;
                    new_file = info == LuaValue::Nil;

                    if let LuaValue::Table(ref info_t) = info {
                        let ftype: LuaValue = info_t.get("type")?;
                        if ftype == LuaValue::String(lua.create_string("file")?) {
                            let size: f64 = info_t.get("size")?;
                            let size_mb = size / 1e6;
                            let large_file: LuaValue = config.get("large_file")?;
                            if let LuaValue::Table(ref lf) = large_file {
                                let soft: f64 = lf
                                    .get::<LuaValue>("soft_limit_mb")?
                                    .as_number()
                                    .unwrap_or(f64::INFINITY);
                                if size_mb >= soft {
                                    let hard: f64 = lf
                                        .get::<LuaValue>("hard_limit_mb")?
                                        .as_number()
                                        .unwrap_or(f64::INFINITY);
                                    let read_only: LuaValue = lf.get("read_only")?;
                                    let plain_text: LuaValue = lf.get("plain_text")?;
                                    let opts = lua.create_table()?;
                                    opts.set("large_file", true)?;
                                    opts.set("file_size", size)?;
                                    opts.set("hard_limited", size_mb >= hard)?;
                                    opts.set("read_only", read_only != LuaValue::Boolean(false))?;
                                    opts.set("plain_text", plain_text != LuaValue::Boolean(false))?;
                                    open_options = LuaValue::Table(opts);
                                }
                            }
                        }
                    }

                    // Check if doc already exists.
                    let docs: LuaTable = core.get("docs")?;
                    for doc in docs.sequence_values::<LuaTable>() {
                        let doc = doc?;
                        let doc_abs: LuaValue = doc.get("abs_filename")?;
                        if let LuaValue::String(ref da) = doc_abs {
                            if da.to_str()? == abs {
                                return Ok(LuaValue::Table(doc));
                            }
                        }
                    }

                    // Trigger syntax detection.
                    let is_plain = if let LuaValue::Table(ref oo) = open_options {
                        let pt: LuaValue = oo.get("plain_text")?;
                        pt == LuaValue::Boolean(true)
                    } else {
                        false
                    };
                    if !is_plain {
                        let mut header = String::new();
                        let lazy_restore: LuaValue = options.get("lazy_restore")?;
                        if !new_file && lazy_restore != LuaValue::Boolean(true) {
                            let io_mod: LuaTable = lua.globals().get("io")?;
                            let io_open: LuaFunction = io_mod.get("open")?;
                            let fp: LuaValue = io_open.call((abs.clone(), "rb"))?;
                            if fp != LuaValue::Nil {
                                let read_fn: LuaFunction = match &fp {
                                    LuaValue::UserData(ud) => ud.get("read")?,
                                    LuaValue::Table(t) => t.get("read")?,
                                    _ => return Err(LuaError::runtime("cannot read file")),
                                };
                                let data: LuaValue = read_fn.call((fp.clone(), 256))?;
                                header = match data {
                                    LuaValue::String(s) => s.to_str()?.to_string(),
                                    _ => String::new(),
                                };
                                let close_fn: LuaFunction = match &fp {
                                    LuaValue::UserData(ud) => ud.get("close")?,
                                    LuaValue::Table(t) => t.get("close")?,
                                    _ => return Err(LuaError::runtime("cannot close file")),
                                };
                                close_fn.call::<()>(fp)?;
                            }
                        }
                        let syntax: LuaTable = require.call("core.syntax")?;
                        let get_syntax: LuaFunction = syntax.get("get")?;
                        get_syntax.call::<()>((abs.clone(), header))?;
                    }
                }

                // Merge lazy_restore into open_options.
                let lazy_restore: LuaValue = options.get("lazy_restore")?;
                if lazy_restore == LuaValue::Boolean(true) {
                    let merge: LuaFunction = common.get("merge")?;
                    let base = if open_options == LuaValue::Nil {
                        lua.create_table()?
                    } else if let LuaValue::Table(t) = open_options {
                        t
                    } else {
                        lua.create_table()?
                    };
                    let extra = lua.create_table()?;
                    extra.set("lazy_restore", true)?;
                    open_options = LuaValue::Table(merge.call((base, extra))?);
                }

                // Create new doc.
                let doc_cls: LuaValue = require.call("core.doc")?;
                let doc: LuaValue = match doc_cls {
                    LuaValue::Table(ref t) => {
                        let mt: Option<LuaTable> = t.metatable();
                        if let Some(mt) = mt {
                            let cf: LuaValue = mt.get("__call")?;
                            if let LuaValue::Function(f) = cf {
                                f.call((
                                    t.clone(),
                                    filename.clone(),
                                    abs_filename.clone(),
                                    new_file,
                                    open_options,
                                ))?
                            } else {
                                LuaValue::Nil
                            }
                        } else {
                            LuaValue::Nil
                        }
                    }
                    _ => LuaValue::Nil,
                };

                let docs: LuaTable = core.get("docs")?;
                insert.call::<()>((docs, doc.clone()))?;

                if let LuaValue::Table(ref dt) = doc {
                    let doc_abs: LuaValue = dt.get("abs_filename")?;
                    if let LuaValue::String(ref s) = doc_abs {
                        let update: LuaFunction = core.get("_update_recent_file")?;
                        update.call::<()>(s.to_str()?.to_string())?;
                    }

                    let large_mode: LuaValue = dt.get("large_file_mode")?;
                    if large_mode == LuaValue::Boolean(true) {
                        let size: f64 = dt
                            .get::<LuaValue>("large_file_size")?
                            .as_number()
                            .unwrap_or(0.0);
                        let size_mb_str = format!("{:.1}", size / 1e6);
                        let hard: LuaValue = dt.get("hard_limited")?;
                        let mode_str = if hard == LuaValue::Boolean(true) {
                            "degraded"
                        } else {
                            "large-file"
                        };
                        let get_name: LuaFunction = dt.get("get_name")?;
                        let doc_name: String = get_name.call(dt.clone())?;
                        let style: LuaTable = get_module(lua, "core.style")?;
                        let warn_color: LuaValue = style.get("warn")?;
                        let status_view: LuaTable = core.get("status_view")?;
                        let show_msg: LuaFunction = status_view.get("show_message")?;
                        let msg: String = format_fn.call((
                            "Opened %s in %s mode (%s MB)",
                            doc_name,
                            mode_str.to_string(),
                            size_mb_str,
                        ))?;
                        show_msg.call::<()>((status_view, "i", warn_color, msg))?;
                    }
                }

                Ok(doc)
            },
        )?,
    )?;

    // get_views_referencing_doc(doc)
    core.set(
        "get_views_referencing_doc",
        lua.create_function(|lua, doc: LuaTable| {
            let core = get_core(lua)?;
            let res = lua.create_table()?;
            let root_view: LuaTable = core.get("root_view")?;
            let root_node: LuaTable = root_view.get("root_node")?;
            let get_children: LuaFunction = root_node.get("get_children")?;
            let views: LuaTable = get_children.call(root_node)?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            let insert: LuaFunction = table_mod.get("insert")?;
            let rawequal: LuaFunction = lua.globals().get("rawequal")?;
            for view in views.sequence_values::<LuaTable>() {
                let view = view?;
                let view_doc: LuaValue = view.get("doc")?;
                if let LuaValue::Table(ref vd) = view_doc {
                    let eq: bool = rawequal.call((vd.clone(), doc.clone()))?;
                    if eq {
                        insert.call::<()>((res.clone(), view))?;
                    }
                }
            }
            Ok(res)
        })?,
    )?;

    // confirm_close_docs(docs?, close_fn, ...)
    core.set(
        "confirm_close_docs",
        lua.create_function(|lua, args: LuaMultiValue| {
            let core = get_core(lua)?;
            let vals: Vec<LuaValue> = args.into_vec();

            let (docs, close_fn_idx) = if vals.is_empty() {
                return Err(LuaError::runtime("confirm_close_docs requires arguments"));
            } else if let LuaValue::Table(_) = &vals[0] {
                (vals[0].clone(), 1)
            } else {
                (core.get::<LuaValue>("docs")?, 0)
            };

            let close_fn = vals.get(close_fn_idx).cloned().unwrap_or(LuaValue::Nil);
            let extra_args: Vec<LuaValue> = vals.into_iter().skip(close_fn_idx + 1).collect();

            let docs_t = match docs {
                LuaValue::Table(t) => t,
                _ => core.get("docs")?,
            };

            let mut dirty_count = 0i64;
            let mut dirty_name = String::new();
            for doc in docs_t.sequence_values::<LuaTable>() {
                let doc = doc?;
                let is_dirty: LuaFunction = doc.get("is_dirty")?;
                let dirty: bool = is_dirty.call(doc.clone())?;
                if dirty {
                    dirty_count += 1;
                    let get_name: LuaFunction = doc.get("get_name")?;
                    dirty_name = get_name.call(doc)?;
                }
            }

            // Skip confirmation when already exiting (user already said yes).
            let exiting: bool = core.get::<Option<bool>>("_exiting")?.unwrap_or(false);

            if dirty_count > 0 && !exiting {
                let string_mod: LuaTable = lua.globals().get("string")?;
                let format_fn: LuaFunction = string_mod.get("format")?;
                let text: String = if dirty_count == 1 {
                    format_fn.call(("\"%s\" has unsaved changes. Quit anyway?", dirty_name))?
                } else {
                    format_fn.call(("%d docs have unsaved changes. Quit anyway?", dirty_count))?
                };

                let opt = lua.create_table()?;
                let yes = lua.create_table()?;
                yes.set("text", "Yes")?;
                yes.set("default_yes", true)?;
                let no = lua.create_table()?;
                no.set("text", "No")?;
                no.set("default_no", true)?;
                opt.set(1, yes)?;
                opt.set(2, no)?;

                let close_fn_key = lua.create_registry_value(close_fn)?;
                let extra_key =
                    lua.create_registry_value(LuaMultiValue::from_vec(extra_args).into_vec())?;
                let callback = lua.create_function(move |lua, item: LuaTable| {
                    let text: String = item.get("text")?;
                    if text == "Yes" {
                        let close_fn: LuaValue = lua.registry_value(&close_fn_key)?;
                        if let LuaValue::Function(f) = close_fn {
                            let extra: Vec<LuaValue> = lua.registry_value(&extra_key)?;
                            f.call::<()>(LuaMultiValue::from_vec(extra))?;
                        }
                    }
                    Ok(())
                })?;

                let nag_view: LuaTable = core.get("nag_view")?;
                let show: LuaFunction = nag_view.get("show")?;
                show.call::<()>((nag_view, "Unsaved Changes", text, opt, callback))?;
            } else {
                if let LuaValue::Function(f) = close_fn {
                    f.call::<()>(LuaMultiValue::from_vec(extra_args))?;
                }
            }

            Ok(())
        })?,
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// View functions
// ---------------------------------------------------------------------------

fn register_view_fns(lua: &Lua, core: &LuaTable) -> LuaResult<()> {
    // set_visited(filename)
    core.set(
        "set_visited",
        lua.create_function(|lua, filename: String| {
            let core = get_core(lua)?;
            let visited: LuaTable = core.get("visited_files")?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            let remove: LuaFunction = table_mod.get("remove")?;
            let insert: LuaFunction = table_mod.get("insert")?;
            let len = visited.raw_len();
            for i in 1..=len {
                let v: String = visited.get(i)?;
                if v == filename {
                    remove.call::<()>((visited.clone(), i))?;
                    break;
                }
            }
            insert.call::<()>((visited, 1, filename))?;
            Ok(())
        })?,
    )?;

    // set_active_view(view)
    core.set(
        "set_active_view",
        lua.create_function(|lua, view: LuaTable| {
            let core = get_core(lua)?;
            let ime: LuaTable = get_module(lua, "core.ime")?;
            let stop: LuaFunction = ime.get("stop")?;
            stop.call::<()>(())?;

            let rawequal: LuaFunction = lua.globals().get("rawequal")?;
            let active_view: LuaValue = core.get("active_view")?;
            let is_same: bool = if let LuaValue::Table(ref av) = active_view {
                rawequal.call((av.clone(), view.clone()))?
            } else {
                false
            };

            if !is_same {
                let window: LuaValue = core.get("window")?;
                if window != LuaValue::Nil {
                    let system_t: LuaTable = lua.globals().get("system")?;
                    let text_input: LuaFunction = system_t.get("text_input")?;
                    let supports: LuaFunction = view.get("supports_text_input")?;
                    let supported: bool = supports.call(view.clone())?;
                    text_input.call::<()>((window, supported))?;
                }

                if let LuaValue::Table(ref av) = active_view {
                    let force_focus: LuaValue = av.get("force_focus")?;
                    if force_focus == LuaValue::Boolean(true) {
                        core.set("next_active_view", view)?;
                        return Ok(());
                    }
                }

                core.set("next_active_view", LuaValue::Nil)?;
                let doc_val: LuaValue = view.get("doc")?;
                if let LuaValue::Table(ref doc) = doc_val {
                    let fname: LuaValue = doc.get("filename")?;
                    if let LuaValue::String(ref s) = fname {
                        let set_visited: LuaFunction = core.get("set_visited")?;
                        set_visited.call::<()>(s.to_str()?.to_string())?;
                    }
                }
                core.set("last_active_view", active_view)?;
                core.set("active_view", view.clone())?;

                let doc_name = view
                    .get::<Option<LuaTable>>("doc")
                    .ok()
                    .flatten()
                    .and_then(|d| d.get::<Option<String>>("abs_filename").ok().flatten());
                log::info!(
                    "core.set_active_view -> {}",
                    doc_name.as_deref().unwrap_or("<no file>")
                );
            }

            Ok(())
        })?,
    )?;

    // show_title_bar(show)
    core.set(
        "show_title_bar",
        lua.create_function(|lua, show: bool| {
            let core = get_core(lua)?;
            let title_view: LuaTable = core.get("title_view")?;
            title_view.set("visible", show)?;
            Ok(())
        })?,
    )?;

    // push_clip_rect(x, y, w, h)
    core.set(
        "push_clip_rect",
        lua.create_function(|lua, (x, y, w, h): (f64, f64, f64, f64)| {
            let core = get_core(lua)?;
            let clip_stack: LuaTable = core.get("clip_rect_stack")?;
            let len = clip_stack.raw_len();
            let last: LuaTable = clip_stack.get(len)?;
            let x2: f64 = last.get(1)?;
            let y2: f64 = last.get(2)?;
            let w2: f64 = last.get(3)?;
            let h2: f64 = last.get(4)?;
            let r = x + w;
            let b = y + h;
            let r2 = x2 + w2;
            let b2 = y2 + h2;
            let nx = x.max(x2);
            let ny = y.max(y2);
            let nb = b.min(b2);
            let nr = r.min(r2);
            let nw = nr - nx;
            let nh = nb - ny;
            let new_rect = lua.create_table()?;
            new_rect.set(1, nx)?;
            new_rect.set(2, ny)?;
            new_rect.set(3, nw)?;
            new_rect.set(4, nh)?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            let insert: LuaFunction = table_mod.get("insert")?;
            insert.call::<()>((clip_stack, new_rect))?;
            let renderer: LuaTable = lua.globals().get("renderer")?;
            let set_clip: LuaFunction = renderer.get("set_clip_rect")?;
            set_clip.call::<()>((nx, ny, nw, nh))?;
            Ok(())
        })?,
    )?;

    // pop_clip_rect()
    core.set(
        "pop_clip_rect",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let clip_stack: LuaTable = core.get("clip_rect_stack")?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            let remove: LuaFunction = table_mod.get("remove")?;
            remove.call::<()>(clip_stack.clone())?;
            let len = clip_stack.raw_len();
            let last: LuaTable = clip_stack.get(len)?;
            let x: f64 = last.get(1)?;
            let y: f64 = last.get(2)?;
            let w: f64 = last.get(3)?;
            let h: f64 = last.get(4)?;
            let renderer: LuaTable = lua.globals().get("renderer")?;
            let set_clip: LuaFunction = renderer.get("set_clip_rect")?;
            set_clip.call::<()>((x, y, w, h))?;
            Ok(())
        })?,
    )?;

    // root_project()
    core.set(
        "root_project",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let projects: LuaTable = core.get("projects")?;
            let first: LuaValue = projects.get(1)?;
            Ok(first)
        })?,
    )?;

    // project_for_path(path)
    core.set(
        "project_for_path",
        lua.create_function(|lua, path: String| {
            let core = get_core(lua)?;
            let projects: LuaTable = core.get("projects")?;
            for project in projects.sequence_values::<LuaTable>() {
                let project = project?;
                let proj_path: String = project.get("path")?;
                if proj_path.contains(&path) {
                    return Ok(LuaValue::Table(project));
                }
            }
            Ok(LuaValue::Nil)
        })?,
    )?;

    // normalize_to_project_dir(path) — deprecated
    core.set(
        "normalize_to_project_dir",
        lua.create_function(|lua, path: String| -> LuaResult<LuaValue> {
            let core = get_core(lua)?;
            let deprecation_log: LuaFunction = core.get("deprecation_log")?;
            deprecation_log.call::<()>("core.normalize_to_project_dir")?;
            let root_project_fn: LuaFunction = core.get("root_project")?;
            let rp: LuaValue = root_project_fn.call(())?;
            if let LuaValue::Table(ref p) = rp {
                let np: LuaFunction = p.get("normalize_path")?;
                return np.call((p.clone(), path));
            }
            let common: LuaTable = get_module(lua, "core.common")?;
            let normalize: LuaFunction = common.get("normalize_path")?;
            normalize.call(path)
        })?,
    )?;

    // project_absolute_path(path) — deprecated
    core.set(
        "project_absolute_path",
        lua.create_function(|lua, path: String| -> LuaResult<LuaValue> {
            let core = get_core(lua)?;
            let deprecation_log: LuaFunction = core.get("deprecation_log")?;
            deprecation_log.call::<()>("core.project_absolute_path")?;
            let root_project_fn: LuaFunction = core.get("root_project")?;
            let rp: LuaValue = root_project_fn.call(())?;
            if let LuaValue::Table(ref p) = rp {
                let ap: LuaFunction = p.get("absolute_path")?;
                return ap.call((p.clone(), path));
            }
            let system_t: LuaTable = lua.globals().get("system")?;
            let abs: LuaFunction = system_t.get("absolute_path")?;
            abs.call(path)
        })?,
    )?;

    // Plugin helper functions.
    core.set(
        "plugin_open_doc",
        lua.create_function(|lua, path_or_doc: LuaValue| -> LuaResult<LuaValue> {
            let core = get_core(lua)?;
            let root_view: LuaTable = core.get("root_view")?;
            let rv_open: LuaFunction = root_view.get("open_doc")?;
            if let LuaValue::String(ref s) = path_or_doc {
                let open_doc: LuaFunction = core.get("open_doc")?;
                let doc: LuaValue = open_doc.call(s.to_str()?.to_string())?;
                return rv_open.call((root_view, doc));
            }
            rv_open.call((root_view, path_or_doc))
        })?,
    )?;

    core.set(
        "plugin_children",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let root_view: LuaTable = core.get("root_view")?;
            let root_node: LuaTable = root_view.get("root_node")?;
            let get_children: LuaFunction = root_node.get("get_children")?;
            get_children.call::<LuaValue>(root_node)
        })?,
    )?;

    core.set(
        "plugin_get_node_for_view",
        lua.create_function(|lua, view: LuaValue| {
            let core = get_core(lua)?;
            let root_view: LuaTable = core.get("root_view")?;
            let root_node: LuaTable = root_view.get("root_node")?;
            let get_node: LuaFunction = root_node.get("get_node_for_view")?;
            get_node.call::<LuaValue>((root_node, view))
        })?,
    )?;

    core.set(
        "plugin_update_layout",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let root_view: LuaTable = core.get("root_view")?;
            let root_node: LuaTable = root_view.get("root_node")?;
            let update: LuaFunction = root_node.get("update_layout")?;
            update.call::<LuaValue>(root_node)
        })?,
    )?;

    core.set(
        "plugin_root_size",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let root_view: LuaTable = core.get("root_view")?;
            let size: LuaTable = root_view.get("size")?;
            let x: LuaValue = size.get("x")?;
            let y: LuaValue = size.get("y")?;
            Ok(LuaMultiValue::from_vec(vec![x, y]))
        })?,
    )?;

    core.set(
        "plugin_enter_prompt",
        lua.create_function(|lua, (label, options): (String, LuaValue)| {
            let core = get_core(lua)?;
            let cv: LuaTable = core.get("command_view")?;
            let enter: LuaFunction = cv.get("enter")?;
            enter.call::<LuaValue>((cv, label, options))
        })?,
    )?;

    core.set(
        "plugin_update_prompt_suggestions",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let cv: LuaTable = core.get("command_view")?;
            let update: LuaFunction = cv.get("update_suggestions")?;
            update.call::<LuaValue>(cv)
        })?,
    )?;

    core.set(
        "plugin_add_status_item",
        lua.create_function(|lua, item: LuaValue| {
            let core = get_core(lua)?;
            let sv: LuaTable = core.get("status_view")?;
            let add: LuaFunction = sv.get("add_item")?;
            add.call::<LuaValue>((sv, item))
        })?,
    )?;

    core.set(
        "plugin_show_status_message",
        lua.create_function(|lua, (icon, color, text): (LuaValue, LuaValue, LuaValue)| {
            let core = get_core(lua)?;
            let sv: LuaTable = core.get("status_view")?;
            let show: LuaFunction = sv.get("show_message")?;
            show.call::<LuaValue>((sv, icon, color, text))
        })?,
    )?;

    core.set(
        "plugin_show_status_tooltip",
        lua.create_function(|lua, text: String| {
            let core = get_core(lua)?;
            let style: LuaTable = get_module(lua, "core.style")?;
            let text_color: LuaValue = style.get("text")?;
            let tooltip = lua.create_table()?;
            tooltip.set(1, text_color)?;
            tooltip.set(2, text)?;
            let sv: LuaTable = core.get("status_view")?;
            sv.set("tooltip", tooltip.clone())?;
            sv.set("tooltip_mode", true)?;
            Ok(tooltip)
        })?,
    )?;

    core.set(
        "plugin_remove_status_tooltip",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let sv: LuaTable = core.get("status_view")?;
            sv.set("tooltip", lua.create_table()?)?;
            sv.set("tooltip_mode", false)?;
            Ok(())
        })?,
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

fn register_event_handling(lua: &Lua, core: &LuaTable) -> LuaResult<()> {
    // on_event(type, ...)
    core.set(
        "on_event",
        lua.create_function(|lua, args: LuaMultiValue| {
            let core = get_core(lua)?;
            let keymap: LuaTable = get_module(lua, "core.keymap")?;
            let ime: LuaTable = get_module(lua, "core.ime")?;
            let vals: Vec<LuaValue> = args.into_vec();
            let event_type: String = match vals.first() {
                Some(LuaValue::String(s)) => s.to_str()?.to_string(),
                _ => return Ok(LuaValue::Boolean(false)),
            };
            let event_args: Vec<LuaValue> = vals.into_iter().skip(1).collect();
            let root_view: LuaTable = core.get("root_view")?;

            let mut did_keymap = false;

            match event_type.as_str() {
                "textinput" => {
                    let on_text: LuaFunction = root_view.get("on_text_input")?;
                    let mut call_args = vec![LuaValue::Table(root_view)];
                    call_args.extend(event_args);
                    on_text.call::<()>(LuaMultiValue::from_vec(call_args))?;
                }
                "textediting" => {
                    let on_edit: LuaFunction = ime.get("on_text_editing")?;
                    on_edit.call::<()>(LuaMultiValue::from_vec(event_args))?;
                }
                "keypressed" => {
                    let editing: LuaValue = ime.get("editing")?;
                    if editing == LuaValue::Boolean(true) {
                        return Ok(LuaValue::Boolean(false));
                    }
                    let on_key: LuaFunction = keymap.get("on_key_pressed")?;
                    did_keymap = on_key.call(LuaMultiValue::from_vec(event_args))?;
                }
                "keyreleased" => {
                    let on_key: LuaFunction = keymap.get("on_key_released")?;
                    on_key.call::<()>(LuaMultiValue::from_vec(event_args))?;
                }
                "mousemoved" => {
                    let on_move: LuaFunction = root_view.get("on_mouse_moved")?;
                    let mut call_args = vec![LuaValue::Table(root_view)];
                    call_args.extend(event_args);
                    on_move.call::<()>(LuaMultiValue::from_vec(call_args))?;
                }
                "mousepressed" => {
                    let on_press: LuaFunction = root_view.get("on_mouse_pressed")?;
                    let mut call_args = vec![LuaValue::Table(root_view.clone())];
                    call_args.extend(event_args.clone());
                    let handled: bool = on_press.call(LuaMultiValue::from_vec(call_args))?;
                    if !handled {
                        let on_mp: LuaFunction = keymap.get("on_mouse_pressed")?;
                        did_keymap = on_mp.call(LuaMultiValue::from_vec(event_args))?;
                    }
                }
                "mousereleased" => {
                    let on_rel: LuaFunction = root_view.get("on_mouse_released")?;
                    let mut call_args = vec![LuaValue::Table(root_view)];
                    call_args.extend(event_args);
                    on_rel.call::<()>(LuaMultiValue::from_vec(call_args))?;
                }
                "mouseleft" => {
                    let on_left: LuaFunction = root_view.get("on_mouse_left")?;
                    on_left.call::<()>(root_view)?;
                }
                "mousewheel" => {
                    let on_wheel: LuaFunction = root_view.get("on_mouse_wheel")?;
                    let mut call_args = vec![LuaValue::Table(root_view.clone())];
                    call_args.extend(event_args.clone());
                    let handled: bool = on_wheel.call(LuaMultiValue::from_vec(call_args))?;
                    if !handled {
                        let on_mw: LuaFunction = keymap.get("on_mouse_wheel")?;
                        did_keymap = on_mw.call(LuaMultiValue::from_vec(event_args))?;
                    }
                }
                "touchpressed" => {
                    let on_tp: LuaFunction = root_view.get("on_touch_pressed")?;
                    let mut call_args = vec![LuaValue::Table(root_view)];
                    call_args.extend(event_args);
                    on_tp.call::<()>(LuaMultiValue::from_vec(call_args))?;
                }
                "touchreleased" => {
                    let on_tr: LuaFunction = root_view.get("on_touch_released")?;
                    let mut call_args = vec![LuaValue::Table(root_view)];
                    call_args.extend(event_args);
                    on_tr.call::<()>(LuaMultiValue::from_vec(call_args))?;
                }
                "touchmoved" => {
                    let on_tm: LuaFunction = root_view.get("on_touch_moved")?;
                    let mut call_args = vec![LuaValue::Table(root_view)];
                    call_args.extend(event_args);
                    on_tm.call::<()>(LuaMultiValue::from_vec(call_args))?;
                }
                "resized" => {
                    let system_t: LuaTable = lua.globals().get("system")?;
                    let get_wm: LuaFunction = system_t.get("get_window_mode")?;
                    let window: LuaValue = core.get("window")?;
                    let mode: LuaValue = get_wm.call(window)?;
                    core.set("window_mode", mode)?;
                }
                "minimized" | "maximized" | "restored" => {
                    let mode = if event_type == "restored" {
                        "normal"
                    } else {
                        event_type.as_str()
                    };
                    core.set("window_mode", mode)?;
                }
                "filedropped" => {
                    let on_fd: LuaFunction = root_view.get("on_file_dropped")?;
                    let mut call_args = vec![LuaValue::Table(root_view)];
                    call_args.extend(event_args);
                    on_fd.call::<()>(LuaMultiValue::from_vec(call_args))?;
                }
                "dialogfinished" => {
                    let id = event_args.first().cloned().unwrap_or(LuaValue::Nil);
                    let status = event_args.get(1).cloned().unwrap_or(LuaValue::Nil);
                    let result = event_args.get(2).cloned().unwrap_or(LuaValue::Nil);
                    let dialogs: LuaTable = core.get("active_file_dialogs")?;
                    let callback: LuaValue = dialogs.get(id.clone())?;
                    if callback == LuaValue::Nil {
                        let error_fn: LuaFunction = core.get("error")?;
                        error_fn.call::<()>(("Invalid dialog id %d", id))?;
                    } else {
                        dialogs.set(id, LuaValue::Nil)?;
                        if let LuaValue::Function(cb) = callback {
                            cb.call::<()>((status, result))?;
                        }
                    }
                }
                "focuslost" => {
                    keymap.set("modkeys", lua.create_table()?)?;
                    let on_fl: LuaFunction = root_view.get("on_focus_lost")?;
                    let mut call_args = vec![LuaValue::Table(root_view)];
                    call_args.extend(event_args);
                    on_fl.call::<()>(LuaMultiValue::from_vec(call_args))?;
                }
                "focusgained" => {
                    let on_fg: LuaFunction = root_view.get("on_focus_gained")?;
                    let mut call_args = vec![LuaValue::Table(root_view)];
                    call_args.extend(event_args);
                    on_fg.call::<()>(LuaMultiValue::from_vec(call_args))?;
                }
                "quit" => {
                    let nag: LuaTable = core.get("nag_view")?;
                    let nag_visible: bool = nag
                        .get::<LuaValue>("visible")?
                        .as_boolean()
                        .unwrap_or(false);
                    if !nag_visible {
                        let quit: LuaFunction = core.get("quit")?;
                        quit.call::<()>(())?;
                    }
                }
                _ => {}
            }

            Ok(LuaValue::Boolean(did_keymap))
        })?,
    )?;

    // compose_window_title(title?)
    core.set(
        "compose_window_title",
        lua.create_function(|_, title: Option<String>| {
            let title = title.unwrap_or_default();
            if title.is_empty() {
                Ok("Lite-Anvil".to_string())
            } else {
                Ok(format!("{} - Lite-Anvil", title))
            }
        })?,
    )?;

    // step()
    core.set(
        "step",
        lua.create_function(move |lua, ()| {
            let core = get_core(lua)?;

            // Cache on first call via a named registry slot.
            let poll_event: LuaFunction =
                match lua.named_registry_value::<LuaValue>("_step_poll")? {
                    LuaValue::Function(f) => f,
                    _ => {
                        let s: LuaTable = lua.globals().get("system")?;
                        let f: LuaFunction = s.get("poll_event")?;
                        lua.set_named_registry_value("_step_poll", f.clone())?;
                        f
                    }
                };
            let try_fn: LuaFunction = match lua.named_registry_value::<LuaValue>("_step_try")? {
                LuaValue::Function(f) => f,
                _ => {
                    let f: LuaFunction = core.get("try")?;
                    lua.set_named_registry_value("_step_try", f.clone())?;
                    f
                }
            };
            let renderer: LuaTable = match lua.named_registry_value::<LuaValue>("_step_renderer")? {
                LuaValue::Table(t) => t,
                _ => {
                    let t: LuaTable = lua.globals().get("renderer")?;
                    lua.set_named_registry_value("_step_renderer", t.clone())?;
                    t
                }
            };

            let on_event: LuaFunction = core.get("on_event")?;

            let mut did_keymap = false;

            loop {
                let result: LuaMultiValue = poll_event.call(())?;
                let vals: Vec<LuaValue> = result.into_vec();
                let event_type = match vals.first() {
                    Some(LuaValue::String(s)) => s.to_str()?.to_string(),
                    _ => break,
                };

                if event_type == "textinput" && did_keymap {
                    did_keymap = false;
                } else if event_type == "mousemoved" {
                    try_fn.call::<()>((on_event.clone(), LuaMultiValue::from_vec(vals)))?;
                } else if event_type == "enteringforeground" {
                    core.set("redraw", true)?;
                    break;
                } else {
                    let result: LuaMultiValue =
                        try_fn.call((on_event.clone(), LuaMultiValue::from_vec(vals)))?;
                    let result_vals: Vec<LuaValue> = result.into_vec();
                    if let Some(LuaValue::Boolean(true)) = result_vals.get(1) {
                        did_keymap = true;
                    }
                }
                core.set("redraw", true)?;
            }

            // Get window size.
            let window: LuaValue = core.get("window")?;
            let (width, height): (i64, i64) = match &window {
                LuaValue::Table(t) => t.call_method("get_size", ())?,
                LuaValue::UserData(ud) => ud.call_method("get_size", ())?,
                _ => return Ok(LuaValue::Boolean(false)),
            };

            // Update root view.
            let root_view: LuaTable = core.get("root_view")?;
            let size: LuaTable = root_view.get("size")?;
            size.set("x", width)?;
            size.set("y", height)?;
            root_view.call_method::<()>("update", ())?;

            let redraw: bool = core.get::<LuaValue>("redraw")?.eq(&LuaValue::Boolean(true));
            if !redraw {
                return Ok(LuaValue::Boolean(false));
            }
            core.set("redraw", false)?;

            // Close unreferenced docs.
            let close_unref: LuaFunction = core.get("_close_unreferenced_docs")?;
            close_unref.call::<()>(())?;

            // Update window title only if changed.
            let active_view: LuaValue = core.get("active_view")?;
            let current_title = if let LuaValue::Table(ref av) = active_view {
                let title_val: LuaValue = av.call_method("get_name", ())?;
                match title_val {
                    LuaValue::String(s) => {
                        let s = s.to_str()?.to_string();
                        if s == "---" { String::new() } else { s }
                    }
                    _ => String::new(),
                }
            } else {
                String::new()
            };

            let old_title = match core.get::<LuaValue>("window_title")? {
                LuaValue::String(s) => s.to_str()?.to_string(),
                _ => String::new(),
            };
            if current_title != old_title {
                let compose: LuaFunction = core.get("compose_window_title")?;
                let new_title: String = compose.call(current_title.clone())?;
                let system_t: LuaTable = lua.globals().get("system")?;
                let set_title: LuaFunction = system_t.get("set_window_title")?;
                set_title.call::<()>((window.clone(), new_title))?;
                core.set("window_title", current_title)?;
            }

            // Draw.
            let begin_frame: LuaFunction = renderer.get("begin_frame")?;
            begin_frame.call::<()>(window)?;
            let clip_stack: LuaTable = core.get("clip_rect_stack")?;
            let first_rect = lua.create_table()?;
            first_rect.set(1, 0)?;
            first_rect.set(2, 0)?;
            first_rect.set(3, width)?;
            first_rect.set(4, height)?;
            clip_stack.set(1, first_rect)?;
            let set_clip: LuaFunction = renderer.get("set_clip_rect")?;
            set_clip.call::<()>((0, 0, width, height))?;
            root_view.call_method::<()>("draw", ())?;
            let end_frame: LuaFunction = renderer.get("end_frame")?;
            end_frame.call::<()>(())?;

            Ok(LuaValue::Boolean(true))
        })?,
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Thread scheduler (run_threads as coroutine.wrap)
// ---------------------------------------------------------------------------

fn register_thread_scheduler(
    lua: &Lua,
    core: &LuaTable,
    state_key: &LuaRegistryKey,
) -> LuaResult<()> {
    let state_key2 =
        lua.create_registry_value(lua.registry_value::<LuaTable>(state_key)?.clone())?;

    // add_thread(f, weak_ref?, ...)
    // Creates coroutine directly from f. Extra args are stored and passed on first resume.
    // Error handling happens in _run_threads_tick via pcall on resume.
    core.set(
        "add_thread",
        lua.create_function(move |lua, args: LuaMultiValue| {
            let core = get_core(lua)?;
            let vals: Vec<LuaValue> = args.into_vec();
            let f = match vals.first() {
                Some(v) => v.clone(),
                None => return Err(LuaError::runtime("add_thread requires a function")),
            };
            let weak_ref = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
            let extra_args: Vec<LuaValue> = vals.into_iter().skip(2).collect();

            let state: LuaTable = lua.registry_value(&state_key2)?;
            let key = if weak_ref == LuaValue::Nil {
                let counter: i64 = state.get("thread_counter")?;
                let new_counter = counter + 1;
                state.set("thread_counter", new_counter)?;
                LuaValue::Integer(new_counter)
            } else {
                weak_ref
            };

            let threads: LuaTable = core.get("threads")?;
            let existing: LuaValue = threads.get(key.clone())?;
            if existing != LuaValue::Nil {
                return Err(LuaError::runtime("Duplicate thread reference"));
            }

            let coroutine_mod: LuaTable = lua.globals().get("coroutine")?;
            let create: LuaFunction = coroutine_mod.get("create")?;

            let cr: LuaValue = create.call(f)?;
            let thread_entry = lua.create_table()?;
            thread_entry.set("cr", cr)?;
            thread_entry.set("wake", 0)?;
            if !extra_args.is_empty() {
                let args_table = lua.create_table()?;
                for (i, arg) in extra_args.into_iter().enumerate() {
                    args_table.set((i + 1) as i64, arg)?;
                }
                thread_entry.set("args", args_table)?;
            }
            threads.set(key.clone(), thread_entry)?;
            Ok(key)
        })?,
    )?;

    // Register session hooks.
    core.set(
        "register_session_load_hook",
        lua.create_function(|lua, (name, hook): (String, LuaFunction)| {
            let core = get_core(lua)?;
            let hooks: LuaTable = core.get("session_load_hooks")?;
            hooks.set(name, hook.clone())?;
            Ok(hook)
        })?,
    )?;

    core.set(
        "register_session_save_hook",
        lua.create_function(|lua, (name, hook): (String, LuaFunction)| {
            let core = get_core(lua)?;
            let hooks: LuaTable = core.get("session_save_hooks")?;
            hooks.set(name, hook.clone())?;
            Ok(hook)
        })?,
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

fn register_run_loop(lua: &Lua, core: &LuaTable) -> LuaResult<()> {
    // _run_threads_tick: processes one iteration of the thread scheduler.
    // Returns (minimal_time_to_wake, threads_done).
    // Handles two thread modes:
    //   - coroutine mode: thread entry has "cr" field, resumed with coroutine.resume
    //   - tick mode: thread entry has "tick" field (function), called directly each cycle
    // On first resume of a coroutine, passes stored "args" from the thread entry.
    // Errors from coroutine resume are caught and reported via core.error.
    core.set(
        "_run_threads_tick",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let config: LuaTable = get_module(lua, "core.config")?;
            let fps: f64 = config.get("fps")?;
            let max_time = 1.0 / fps - 0.004;
            let math_mod: LuaTable = lua.globals().get("math")?;
            let math_huge: f64 = math_mod.get("huge")?;
            let mut minimal_time_to_wake = math_huge;

            let threads: LuaTable = core.get("threads")?;
            let system_t: LuaTable = lua.globals().get("system")?;
            let get_time: LuaFunction = system_t.get("get_time")?;
            let resume: LuaFunction = lua.globals().get::<LuaTable>("coroutine")?.get("resume")?;
            let status_fn: LuaFunction =
                lua.globals().get::<LuaTable>("coroutine")?.get("status")?;

            // Collect thread keys.
            let pairs: LuaFunction = lua.globals().get("pairs")?;
            let thread_keys = lua.create_table()?;
            let mut key_count = 0i64;
            let iter: LuaMultiValue = pairs.call(threads.clone())?;
            let iter_vals: Vec<LuaValue> = iter.into_vec();
            let next_fn = match iter_vals.first() {
                Some(LuaValue::Function(f)) => f.clone(),
                _ => return Ok((minimal_time_to_wake, true)),
            };
            let mut next_key = LuaValue::Nil;
            loop {
                let result: LuaMultiValue = next_fn.call((threads.clone(), next_key.clone()))?;
                let vals: Vec<LuaValue> = result.into_vec();
                let k = vals.first().cloned().unwrap_or(LuaValue::Nil);
                if k == LuaValue::Nil {
                    break;
                }
                key_count += 1;
                thread_keys.set(key_count, k.clone())?;
                next_key = k;
            }

            for i in 1..=key_count {
                let k: LuaValue = thread_keys.get(i)?;
                thread_keys.set(i, LuaValue::Nil)?;
                let thread: LuaValue = threads.get(k.clone())?;
                let now: f64 = get_time.call(())?;

                if let LuaValue::Table(ref t) = thread {
                    let wake: f64 = t.get("wake")?;
                    if wake < now {
                        // Check for tick-mode threads (Rust functions called repeatedly).
                        let tick_val: LuaValue = t.get("tick")?;
                        if let LuaValue::Function(ref tick_fn) = tick_val {
                            let result: LuaValue = tick_fn.call(())?;
                            match result {
                                LuaValue::Nil | LuaValue::Boolean(false) => {
                                    threads.set(k, LuaValue::Nil)?;
                                }
                                _ => {
                                    let wait: f64 = match result {
                                        LuaValue::Number(n) => n,
                                        LuaValue::Integer(n) => n as f64,
                                        _ => 1.0 / 30.0,
                                    };
                                    t.set("wake", now + wait)?;
                                    if wait < minimal_time_to_wake {
                                        minimal_time_to_wake = wait;
                                    }
                                }
                            }
                        } else {
                            // Coroutine-mode thread.
                            let cr: LuaValue = t.get("cr")?;

                            // On first resume, pass stored args.
                            let args_val: LuaValue = t.get("args")?;
                            let result: LuaMultiValue =
                                if let LuaValue::Table(ref args_tbl) = args_val {
                                    let mut resume_args = vec![cr.clone()];
                                    for arg in args_tbl.clone().sequence_values::<LuaValue>() {
                                        resume_args.push(arg?);
                                    }
                                    t.set("args", LuaValue::Nil)?;
                                    resume.call(LuaMultiValue::from_vec(resume_args))?
                                } else {
                                    resume.call(cr.clone())?
                                };
                            let vals: Vec<LuaValue> = result.into_vec();

                            // vals[0] = ok (bool from coroutine.resume), vals[1..] = yielded values
                            let ok = matches!(vals.first(), Some(LuaValue::Boolean(true)));
                            if !ok {
                                // Resume failed: report error via core.error and remove thread.
                                let err_msg = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                                let error_fn: LuaResult<LuaFunction> = core.get("error");
                                if let Ok(error_fn) = error_fn {
                                    if let Err(e) = error_fn
                                        .call::<LuaValue>(("Error running thread: %s", err_msg))
                                    {
                                        log::warn!("core.error callback failed: {e}");
                                    }
                                }
                                threads.set(k, LuaValue::Nil)?;
                            } else {
                                let wait: f64 = vals
                                    .get(1)
                                    .and_then(|v| match v {
                                        LuaValue::Number(n) => Some(*n),
                                        LuaValue::Integer(n) => Some(*n as f64),
                                        _ => None,
                                    })
                                    .unwrap_or(1.0 / 30.0);

                                let st: String = status_fn.call(cr)?;
                                if st == "dead" {
                                    threads.set(k, LuaValue::Nil)?;
                                } else {
                                    t.set("wake", now + wait)?;
                                    if wait < minimal_time_to_wake {
                                        minimal_time_to_wake = wait;
                                    }
                                }
                            }
                        }
                    } else if wake - now < minimal_time_to_wake {
                        minimal_time_to_wake = wake - now;
                    }
                }

                let now2: f64 = get_time.call(())?;
                let frame_start: f64 = core.get("frame_start")?;
                if now2 - frame_start > max_time {
                    return Ok((minimal_time_to_wake, false));
                }
            }

            Ok((minimal_time_to_wake, true))
        })?,
    )?;

    // run_threads is simply _run_threads_tick — processes all threads in one call.
    // The original used coroutine.wrap for mid-frame yielding, but that requires
    // yielding from Lua (not C/Rust). The tick function already handles the
    // max_time budget check and returns early when the frame budget is exhausted.
    let tick_fn: LuaFunction = core.get("_run_threads_tick")?;
    let run_threads_key = lua.create_registry_value(tick_fn)?;

    // core.run()
    core.set(
        "run",
        lua.create_function(move |lua, ()| {
            let core = get_core(lua)?;
            let system_t: LuaTable = lua.globals().get("system")?;
            let config: LuaTable = get_module(lua, "core.config")?;
            let get_time: LuaFunction = system_t.get("get_time")?;

            let run_threads: LuaFunction = lua.registry_value(&run_threads_key)?;

            let mut next_step: Option<f64> = None;
            let mut last_frame_time: Option<f64> = None;
            let mut run_threads_full: i64 = 0;

            // Cache hot-path values to avoid repeated Lua lookups.
            let step_fn: LuaFunction = core.get("step")?;
            let fps: f64 = config.get("fps")?;
            let blink_period: f64 = config.get("blink_period")?;
            let wait_event_fn: LuaFunction = system_t.get("wait_event")?;
            let has_focus_fn: LuaFunction = system_t.get("window_has_focus")?;
            let window_val: LuaValue = core.get("window")?;
            let frame_budget = 1.0 / fps - 0.002;
            let frame_interval = 1.0 / fps;

            let quit_fn: LuaFunction = core.get("quit")?;

            loop {
                if crate::signal::shutdown_requested() {
                    crate::signal::clear_shutdown();
                    quit_fn.call::<()>(())?;
                }

                let frame_start: f64 = get_time.call(())?;
                core.set("frame_start", frame_start)?;

                let mut did_redraw = false;
                let mut did_step = false;

                let redraw: bool = core.get::<LuaValue>("redraw")?.eq(&LuaValue::Boolean(true));
                let force_draw = redraw
                    && last_frame_time.is_some()
                    && (frame_start - last_frame_time.unwrap_or(0.0)) > (1.0 / fps);

                if force_draw || next_step.is_none() || frame_start >= next_step.unwrap_or(0.0) {
                    let stepped: bool = step_fn.call(())?;
                    if stepped {
                        did_redraw = true;
                        last_frame_time = Some(frame_start);
                    }
                    next_step = None;
                    did_step = true;
                }

                // Run threads AFTER event processing — skip if frame budget exhausted.
                let now_post_step: f64 = get_time.call(())?;
                let result: LuaMultiValue = if now_post_step - frame_start < frame_budget {
                    run_threads.call(())?
                } else {
                    LuaMultiValue::from_vec(vec![LuaValue::Number(0.0), LuaValue::Boolean(false)])
                };
                let rt_vals: Vec<LuaValue> = result.into_vec();
                let time_to_wake: f64 = rt_vals
                    .first()
                    .and_then(|v| match v {
                        LuaValue::Number(n) => Some(*n),
                        LuaValue::Integer(n) => Some(*n as f64),
                        _ => None,
                    })
                    .unwrap_or(1.0);
                let threads_done = matches!(rt_vals.get(1), Some(LuaValue::Boolean(true)));

                if threads_done {
                    run_threads_full += 1;
                }

                let restart: LuaValue = core.get("restart_request")?;
                let quit: LuaValue = core.get("quit_request")?;
                if restart == LuaValue::Boolean(true) || quit == LuaValue::Boolean(true) {
                    break;
                }

                if !did_redraw {
                    let focused: bool = has_focus_fn.call(window_val.clone())?;

                    if focused || !did_step || run_threads_full < 2 {
                        let now2: f64 = get_time.call(())?;
                        if next_step.is_none() {
                            let blink_start: f64 = core.get("blink_start")?;
                            let t = now2 - blink_start;
                            let h = blink_period / 2.0;
                            let dt = (t / h).ceil() * h - t;
                            let cursor_time = dt + frame_interval;
                            next_step = Some(now2 + cursor_time);
                        }
                        let wait_time = (next_step.unwrap_or(0.0) - now2).min(time_to_wake);
                        let got_event: bool = wait_event_fn.call(wait_time)?;
                        if got_event {
                            next_step = None;
                        }
                    } else {
                        wait_event_fn.call::<()>(())?;
                        next_step = None;
                    }
                } else {
                    run_threads_full = 0;
                    // Check for pending events without blocking. If more
                    // events arrived during the draw, skip the sleep so the
                    // next step drains them immediately (key repeat latency).
                    let has_pending: bool = wait_event_fn.call(0.0)?;
                    if has_pending {
                        next_step = None;
                        continue;
                    }
                    let now3: f64 = get_time.call(())?;
                    let elapsed = now3 - frame_start;
                    let next_frame = (frame_interval - elapsed).max(0.0);
                    next_step = next_step.or(Some(now3 + next_frame));
                    if next_frame > 0.001 {
                        let wait_time = next_frame.min(time_to_wake);
                        let got_event: bool = wait_event_fn.call(wait_time)?;
                        if got_event {
                            next_step = None;
                        }
                    }
                }
            }
            Ok(())
        })?,
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Miscellaneous functions
// ---------------------------------------------------------------------------

fn register_misc(
    lua: &Lua,
    core: &LuaTable,
    state_key: &LuaRegistryKey,
    _prefix_key: &LuaRegistryKey,
) -> LuaResult<()> {
    let _ = state_key;

    // try(fn, ...) — pre-build the error handler and cache xpcall.
    {
        let xpcall: LuaFunction = lua.globals().get("xpcall")?;
        let error_fn: LuaFunction = core.get("error")?;
        let debug_mod: LuaTable = lua.globals().get("debug")?;
        let traceback: LuaFunction = debug_mod.get("traceback")?;
        let err_key = lua.create_registry_value(error_fn)?;
        let tb_key = lua.create_registry_value(traceback)?;
        let handler = lua.create_function(move |lua, msg: LuaValue| {
            let error_fn: LuaFunction = lua.registry_value(&err_key)?;
            let item: LuaTable = error_fn.call(("%s", msg.clone()))?;
            let traceback: LuaFunction = lua.registry_value(&tb_key)?;
            let tb: String = traceback.call(("", 2))?;
            item.set("info", tb.replace('\t', ""))?;
            Ok(msg)
        })?;
        let handler_key = lua.create_registry_value(handler)?;
        let xpcall_key = lua.create_registry_value(xpcall)?;

        core.set(
            "try",
            lua.create_function(move |lua, args: LuaMultiValue| {
                let vals: Vec<LuaValue> = args.into_vec();
                let func = match vals.first() {
                    Some(v) => v.clone(),
                    None => return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)])),
                };
                let fn_args: Vec<LuaValue> = vals.into_iter().skip(1).collect();

                let xpcall: LuaFunction = lua.registry_value(&xpcall_key)?;
                let handler: LuaFunction = lua.registry_value(&handler_key)?;

                let mut call_args = vec![func, LuaValue::Function(handler)];
                call_args.extend(fn_args);
                let result: LuaMultiValue = xpcall.call(LuaMultiValue::from_vec(call_args))?;
                let result_vals: Vec<LuaValue> = result.into_vec();
                let ok = matches!(result_vals.first(), Some(LuaValue::Boolean(true)));

                if ok {
                    let res = result_vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                    Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(true), res]))
                } else {
                    let err = result_vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                    Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false), err]))
                }
            })?,
        )?;
    } // end try() block

    // exit(quit_fn, force?)
    // When force is false or absent, shows a confirmation dialog if any
    // documents have unsaved changes. When force is true, exits immediately.
    core.set(
        "exit",
        lua.create_function(|lua, (quit_fn, force): (LuaFunction, Option<bool>)| {
            let core = get_core(lua)?;
            let do_exit = lua.create_function(move |lua, ()| {
                let core = get_core(lua)?;
                core.set("_exiting", true)?;
                let save: LuaFunction = core.get("_save_session")?;
                save.call::<()>(())?;
                let delete_temp: LuaFunction = core.get("delete_temp_files")?;
                delete_temp.call::<()>(())?;
                let projects: LuaTable = core.get("projects")?;
                while projects.raw_len() > 0 {
                    let last: LuaValue = projects.get(projects.raw_len())?;
                    let remove: LuaFunction = core.get("remove_project")?;
                    remove.call::<()>((last, true))?;
                }
                quit_fn.call::<()>(())?;
                Ok(())
            })?;
            if force.unwrap_or(false) {
                do_exit.call::<()>(())?;
            } else {
                let confirm: LuaFunction = core.get("confirm_close_docs")?;
                confirm.call::<()>(do_exit)?;
            }
            Ok(())
        })?,
    )?;

    // quit(force?)
    core.set(
        "quit",
        lua.create_function(|lua, force: Option<bool>| {
            let core = get_core(lua)?;
            let exit_fn: LuaFunction = core.get("exit")?;
            let quit_fn = lua.create_function(|lua, ()| {
                let core = get_core(lua)?;
                core.set("quit_request", true)?;
                Ok(())
            })?;
            exit_fn.call::<()>((quit_fn, force))?;
            Ok(())
        })?,
    )?;

    // restart()
    core.set(
        "restart",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let exit_fn: LuaFunction = core.get("exit")?;
            let restart_fn = lua.create_function(|lua, ()| {
                let core = get_core(lua)?;
                core.set("restart_request", true)?;
                let window: LuaValue = core.get("window")?;
                if let LuaValue::Table(ref w) = window {
                    let persist: LuaFunction = w.get("_persist")?;
                    persist.call::<()>(w.clone())?;
                } else if let LuaValue::UserData(ref w) = window {
                    let persist: LuaFunction = w.get("_persist")?;
                    persist.call::<()>(window.clone())?;
                }
                Ok(())
            })?;
            exit_fn.call::<()>(restart_fn)?;
            Ok(())
        })?,
    )?;

    // blink_reset()
    core.set(
        "blink_reset",
        lua.create_function(|lua, ()| {
            let core = get_core(lua)?;
            let system_t: LuaTable = lua.globals().get("system")?;
            let get_time: LuaFunction = system_t.get("get_time")?;
            let now: f64 = get_time.call(())?;
            core.set("blink_start", now)?;
            Ok(())
        })?,
    )?;

    // File dialogs.
    // _open_dialog(dialog_type, window, callback, options?)
    core.set(
        "_open_dialog",
        lua.create_function(
            |lua,
             (dialog_type, window, callback, options): (
                String,
                LuaValue,
                LuaFunction,
                Option<LuaValue>,
            )| {
                let core = get_core(lua)?;
                let system_t: LuaTable = lua.globals().get("system")?;
                let dialog_fn: LuaFunction = match dialog_type.as_str() {
                    "openfile" => system_t.get("open_file_dialog")?,
                    "opendirectory" => system_t.get("open_directory_dialog")?,
                    "savefile" => system_t.get("save_file_dialog")?,
                    _ => return Err(LuaError::runtime("Invalid dialog type")),
                };
                let dialogs: LuaTable = core.get("active_file_dialogs")?;

                // Increment tag using the core table field.
                let last_tag: i64 = core
                    .get::<LuaValue>("_last_file_dialog_tag")?
                    .as_integer()
                    .unwrap_or(0);
                let new_tag = last_tag + 1;
                core.set("_last_file_dialog_tag", new_tag)?;

                dialogs.set(new_tag, callback)?;
                dialog_fn.call::<()>((window, new_tag, options))?;
                Ok(())
            },
        )?,
    )?;

    core.set("_last_file_dialog_tag", 0i64)?;

    // open_file_dialog(window, callback, options?)
    core.set(
        "open_file_dialog",
        lua.create_function(
            |lua, (window, callback, options): (LuaValue, LuaFunction, Option<LuaValue>)| {
                let core = get_core(lua)?;
                let open_dialog: LuaFunction = core.get("_open_dialog")?;
                open_dialog.call::<()>(("openfile", window, callback, options))?;
                Ok(())
            },
        )?,
    )?;

    // open_directory_dialog(window, callback, options?)
    core.set(
        "open_directory_dialog",
        lua.create_function(
            |lua, (window, callback, options): (LuaValue, LuaFunction, Option<LuaValue>)| {
                let core = get_core(lua)?;
                let open_dialog: LuaFunction = core.get("_open_dialog")?;
                open_dialog.call::<()>(("opendirectory", window, callback, options))?;
                Ok(())
            },
        )?,
    )?;

    // save_file_dialog(window, callback, options?)
    core.set(
        "save_file_dialog",
        lua.create_function(
            |lua, (window, callback, options): (LuaValue, LuaFunction, Option<LuaValue>)| {
                let core = get_core(lua)?;
                let open_dialog: LuaFunction = core.get("_open_dialog")?;
                open_dialog.call::<()>(("savefile", window, callback, options))?;
                Ok(())
            },
        )?,
    )?;

    // request_cursor(value)
    core.set(
        "request_cursor",
        lua.create_function(|lua, value: LuaValue| {
            let core = get_core(lua)?;
            core.set("cursor_change_req", value)?;
            Ok(())
        })?,
    )?;

    // on_error(err)
    core.set(
        "on_error",
        lua.create_function(|lua, err: LuaValue| {
            let core = get_core(lua)?;
            let userdir: String = lua.globals().get("USERDIR")?;
            let pathsep: String = lua.globals().get("PATHSEP")?;
            let io_mod: LuaTable = lua.globals().get("io")?;
            let open: LuaFunction = io_mod.get("open")?;
            let tostring: LuaFunction = lua.globals().get("tostring")?;
            let err_str: String = tostring.call(err)?;

            let error_path = format!("{}{}error.txt", userdir, pathsep);
            let fp: LuaValue = open.call((error_path, "wb"))?;
            if fp != LuaValue::Nil {
                let write_fn: LuaFunction = match &fp {
                    LuaValue::UserData(ud) => ud.get("write")?,
                    LuaValue::Table(t) => t.get("write")?,
                    _ => return Ok(()),
                };
                let debug_mod: LuaTable = lua.globals().get("debug")?;
                let traceback: LuaFunction = debug_mod.get("traceback")?;
                let tb: String = traceback.call(("", 4))?;
                write_fn.call::<()>((fp.clone(), format!("Error: {}\n", err_str)))?;
                write_fn.call::<()>((fp.clone(), format!("{}\n", tb)))?;
                let close_fn: LuaFunction = match &fp {
                    LuaValue::UserData(ud) => ud.get("close")?,
                    LuaValue::Table(t) => t.get("close")?,
                    _ => return Ok(()),
                };
                close_fn.call::<()>(fp)?;
            }

            // Save dirty documents.
            let docs: LuaTable = core.get("docs")?;
            for doc in docs.sequence_values::<LuaTable>() {
                let doc = doc?;
                let is_dirty: LuaFunction = doc.get("is_dirty")?;
                let dirty: bool = is_dirty.call(doc.clone())?;
                if dirty {
                    let filename: LuaValue = doc.get("filename")?;
                    if let LuaValue::String(ref f) = filename {
                        let save: LuaFunction = doc.get("save")?;
                        let backup = format!("{}~", f.to_str()?);
                        save.call::<()>((doc, backup))?;
                    }
                }
            }
            Ok(())
        })?,
    )?;

    // deprecation_log(kind)
    {
        let state_key3 =
            lua.create_registry_value(lua.registry_value::<LuaTable>(state_key)?.clone())?;
        core.set(
            "deprecation_log",
            lua.create_function(move |lua, kind: String| {
                let state: LuaTable = lua.registry_value(&state_key3)?;
                let alerted: LuaTable = state.get("alerted_deprecations")?;
                let already: LuaValue = alerted.get(kind.as_str())?;
                if already == LuaValue::Boolean(true) {
                    return Ok(());
                }
                alerted.set(kind.as_str(), true)?;
                let core = get_core(lua)?;
                let warn: LuaFunction = core.get("warn")?;
                warn.call::<()>((
                    "Used deprecated functionality [%s]. Check if your plugins are up to date.",
                    kind,
                ))?;
                Ok(())
            })?,
        )?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Static content builders (no Lua strings — these are Rust &str constants)
// ---------------------------------------------------------------------------

fn build_default_config_content() -> &'static str {
    "-- put user settings here\n\
     -- this module will be loaded during editor startup\n\
     -- it will be automatically reloaded when saved\n\
     \n\
     local core = require \"core\"\n\
     local keymap = require \"core.keymap\"\n\
     local config = require \"core.config\"\n\
     local style = require \"core.style\"\n\
     \n\
     ------------------------------ Themes ----------------------------------------\n\
     \n\
     -- light theme:\n\
     -- core.reload_module(\"colors.summer\")\n\
     \n\
     --------------------------- Key bindings -------------------------------------\n\
     \n\
     -- key binding:\n\
     -- keymap.add { [\"ctrl+escape\"] = \"core:quit\" }\n\
     \n\
     -- pass 'true' for second parameter to overwrite an existing binding\n\
     -- keymap.add({ [\"ctrl+pageup\"] = \"root:switch-to-previous-tab\" }, true)\n\
     -- keymap.add({ [\"ctrl+pagedown\"] = \"root:switch-to-next-tab\" }, true)\n\
     \n\
     ------------------------------- Fonts ----------------------------------------\n\
     \n\
     -- DATADIR is the location of the installed Lite-Anvil Lua code, default color\n\
     -- schemes and fonts.\n\
     -- USERDIR is the location of the Lite-Anvil configuration directory.\n\
     --\n\
     -- Fonts can be customized entirely from this file.\n\
     -- Available font options:\n\
     -- antialiasing = \"none\" | \"grayscale\" | \"subpixel\"\n\
     -- hinting      = \"none\" | \"slight\" | \"full\"\n\
     -- bold         = true | false\n\
     -- italic       = true | false\n\
     -- underline    = true | false\n\
     -- smoothing    = true | false\n\
     -- strikethrough= true | false\n\
     --\n\
     -- Example:\n\
     -- config.fonts.ui = {\n\
     --   paths = {\n\
     --     USERDIR .. \"/fonts/YourUIFont.ttf\",\n\
     --     DATADIR .. \"/fonts/Lilex-Regular.ttf\",\n\
     --   },\n\
     --   size = 15,\n\
     --   options = { antialiasing = \"grayscale\", hinting = \"slight\" }\n\
     -- }\n\
     --\n\
     -- config.fonts.code = {\n\
     --   path = USERDIR .. \"/fonts/YourMono.ttf\",\n\
     --   size = 15,\n\
     --   options = { hinting = \"full\" }\n\
     -- }\n\
     --\n\
     -- config.fonts.syntax[\"comment\"] = {\n\
     --   path = USERDIR .. \"/fonts/YourMonoItalic.ttf\",\n\
     --   size = 15,\n\
     --   options = { italic = true }\n\
     -- }\n\
     \n\
     ------------------------------- Theme ----------------------------------------\n\
     \n\
     -- Built-in themes:\n\
     -- config.theme = \"default\"\n\
     -- config.theme = \"fall\"\n\
     -- config.theme = \"summer\"\n\
     -- config.theme = \"textadept\"\n\
     --\n\
     -- You can override any theme color here. These keys map to `style.*`.\n\
     -- Syntax token colors go under `config.colors.syntax`.\n\
     -- Log colors go under `config.colors.log`.\n\
     --\n\
     -- config.colors.background = \"#1f2128\"\n\
     -- config.colors.text = \"#d7dae0\"\n\
     -- config.colors.selection = \"#364055\"\n\
     -- config.colors.guide = \"#4c566a\"\n\
     -- config.colors.syntax.keyword = \"#ff7a90\"\n\
     -- config.colors.syntax.string = \"#ffd479\"\n\
     -- config.colors.syntax.comment = \"#7f8c98\"\n\
     -- config.colors.log.ERROR = { icon = \"!\", color = \"#ff5f56\" }\n\
     --\n\
     -- UI sizing is customizable here too:\n\
     -- config.ui.caret_width = 2\n\
     -- config.ui.padding_x = 14\n\
     -- config.ui.padding_y = 7\n\
     \n\
     ------------------------------ Plugins ----------------------------------------\n\
     \n\
     -- disable plugin loading setting config entries:\n\
     \n\
     -- disable plugin detectindent, otherwise it is enabled by default:\n\
     -- config.plugins.detectindent = false\n\
     --\n\
     -- disable LSP startup while keeping the plugin installed:\n\
     -- config.lsp.load_on_startup = false\n\
     -- disable semantic token overlays while keeping LSP features:\n\
     -- config.lsp.semantic_highlighting = false\n\
     -- disable inline diagnostics while keeping LSP features:\n\
     -- config.lsp.inline_diagnostics = false\n\
     -- or disable the plugin entirely:\n\
     -- config.plugins.lsp = false\n\
     --\n\
     -- terminal plugin examples:\n\
     -- config.plugins.terminal.shell = os.getenv(\"SHELL\") or \"bash\"\n\
     -- config.plugins.terminal.shell_args = {}\n\
     -- config.plugins.terminal.scrollback = 10000\n\
     -- config.plugins.terminal.color_scheme = \"Dracula\"\n\
     -- config.plugins.terminal.close_on_exit = false\n\
     --\n\
     -- gitignore integration examples:\n\
     -- config.gitignore.enabled = true\n\
     -- config.gitignore.additional_patterns = { \"%.log$\", \"^dist/\" }\n\
     --\n\
     -- git plugin examples:\n\
     -- config.plugins.git.refresh_interval = 5\n\
     -- config.plugins.git.show_branch_in_statusbar = true\n\
     -- config.plugins.git.treeview_highlighting = true\n\
     --\n\
     -- minimap (code overview sidebar):\n\
     -- config.plugins.minimap = { enabled = true }\n\
     -- config.plugins.minimap.width = 120\n\
     -- config.plugins.minimap.line_height = 4\n\
     --\n\
     -- project replace examples:\n\
     -- config.plugins.projectreplace.backup_originals = true\n\
     \n\
     -------------------------- Editor Settings ------------------------------------\n\
     \n\
     -- draw a vertical marker at config.line_limit:\n\
     -- config.long_line_indicator = true\n\
     \n\
     ---------------------------- Miscellaneous -------------------------------------\n\
     \n\
     -- modify list of files to ignore when indexing the project:\n\
     -- config.ignore_files = {\n\
     --   -- folders\n\
     --   \"^%.svn/\",        \"^%.git/\",   \"^%.hg/\",        \"^CVS/\", \"^%.Trash/\", \"^%.Trash%-.*/\",\n\
     --   \"^node_modules/\", \"^%.cache/\", \"^__pycache__/\",\n\
     --   -- files\n\
     --   \"%.pyc$\",         \"%.pyo$\",       \"%.exe$\",        \"%.dll$\",   \"%.obj$\", \"%.o$\",\n\
     --   \"%.a$\",           \"%.lib$\",       \"%.so$\",         \"%.dylib$\", \"%.ncb$\", \"%.sdf$\",\n\
     --   \"%.suo$\",         \"%.pdb$\",       \"%.idb$\",        \"%.class$\", \"%.psd$\", \"%.db$\",\n\
     --   \"^desktop%.ini$\", \"^%.DS_Store$\", \"^%.directory$\",\n\
     -- }\n\
     \n"
}

fn build_project_init_content() -> &'static str {
    "-- Put project's module settings here.\n\
     -- This module will be loaded when opening a project, after the user module\n\
     -- configuration.\n\
     -- It will be automatically reloaded when saved.\n\
     \n\
     local config = require \"core.config\"\n\
     \n\
     -- you can add some patterns to ignore files within the project\n\
     -- config.ignore_files = {\"^%.\", <some-patterns>}\n\
     \n\
     -- Patterns are normally applied to the file's or directory's name, without\n\
     -- its path. See below about how to apply filters on a path.\n\
     --\n\
     -- Here some examples:\n\
     --\n\
     -- \"^%.\" matches any file of directory whose basename begins with a dot.\n\
     --\n\
     -- When there is an '/' or a '/$' at the end, the pattern will only match\n\
     -- directories. When using such a pattern a final '/' will be added to the name\n\
     -- of any directory entry before checking if it matches.\n\
     --\n\
     -- \"^%.git/\" matches any directory named \".git\" anywhere in the project.\n\
     --\n\
     -- If a \"/\" appears anywhere in the pattern (except when it appears at the end or\n\
     -- is immediately followed by a '$'), then the pattern will be applied to the full\n\
     -- path of the file or directory. An initial \"/\" will be prepended to the file's\n\
     -- or directory's path to indicate the project's root.\n\
     --\n\
     -- \"^/node_modules/\" will match a directory named \"node_modules\" at the project's root.\n\
     -- \"^/build.*/\" will match any top level directory whose name begins with \"build\".\n\
     -- \"^/subprojects/.+/\" will match any directory inside a top-level folder named \"subprojects\".\n\
     \n\
     -- You may activate some plugins on a per-project basis to override the user's settings.\n\
     -- config.plugins.trimwitespace = true\n"
}
