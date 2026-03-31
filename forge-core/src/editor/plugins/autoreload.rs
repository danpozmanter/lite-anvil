use mlua::prelude::*;
use std::sync::Arc;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Shared autoreload state held in a Lua table so it participates in the Lua GC.
/// The `times` and `timers` fields use weak-keyed sub-tables (doc → value).
fn create_state_table(lua: &Lua) -> LuaResult<LuaTable> {
    let state = lua.create_table()?;

    let make_weak = |lua: &Lua| -> LuaResult<LuaTable> {
        let t = lua.create_table()?;
        let mt = lua.create_table()?;
        mt.set("__mode", "k")?;
        t.set_metatable(Some(mt))?;
        Ok(t)
    };

    state.set("times", make_weak(lua)?)?;
    state.set("visible", make_weak(lua)?)?;
    state.set("timers", make_weak(lua)?)?;
    state.set("watched_dirs", lua.create_table()?)?;

    let dirwatch = require_table(lua, "core.dirwatch")?;
    let watch: LuaTable = dirwatch.call_function("new", ())?;
    state.set("watch", watch)?;

    Ok(state)
}

fn update_time(lua: &Lua, doc: &LuaTable, state: &LuaTable) -> LuaResult<()> {
    let times: LuaTable = state.get("times")?;
    let abs_filename: Option<String> = doc.get("abs_filename")?;
    let path = abs_filename.or_else(|| doc.get::<Option<String>>("filename").ok().flatten());
    let path = path.unwrap_or_default();

    let system: LuaTable = lua.globals().get("system")?;
    let info: Option<LuaTable> = system.call_function("get_file_info", path)?;
    let mtime: LuaValue = info
        .and_then(|t| t.get("modified").ok())
        .unwrap_or(LuaValue::Nil);
    times.raw_set(doc.clone(), mtime)?;
    Ok(())
}

fn reload_doc(lua: &Lua, doc: &LuaTable, state: &LuaTable) -> LuaResult<()> {
    doc.call_method::<()>("reload", ())?;
    update_time(lua, doc, state)?;
    let core = require_table(lua, "core")?;
    core.set("redraw", true)?;
    let filename: Option<String> = doc.get("filename")?;
    core.call_function::<()>(
        "log_quiet",
        format!("Auto-reloaded doc \"{}\"", filename.unwrap_or_default()),
    )?;
    Ok(())
}

fn keep_current_doc(doc: &LuaTable, state: &LuaTable) -> LuaResult<()> {
    let times: LuaTable = state.get("times")?;
    // Restore times[doc] from doc.deferred_reload_mtime if present.
    let deferred_mtime: LuaValue = doc.get("deferred_reload_mtime")?;
    let existing: LuaValue = times.raw_get(doc.clone())?;
    let mtime = if !matches!(deferred_mtime, LuaValue::Nil) {
        deferred_mtime
    } else {
        existing
    };
    times.raw_set(doc.clone(), mtime)?;
    doc.set("deferred_reload", false)?;
    doc.set("deferred_reload_mtime", LuaValue::Nil)?;
    doc.set("reload_prompt_queued", false)?;
    Ok(())
}

fn check_prompt_reload(lua: &Lua, doc: &LuaTable, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let deferred: bool = doc.get("deferred_reload").unwrap_or(false);
    let queued: bool = doc.get("reload_prompt_queued").unwrap_or(false);
    if !deferred || queued {
        return Ok(());
    }
    doc.set("reload_prompt_queued", true)?;

    let core = require_table(lua, "core")?;
    let nag_view: LuaTable = core.get("nag_view")?;
    let filename: String = doc.get("filename").unwrap_or_default();
    let message = format!(
        "{} has changed on disk.\nReload from the filesystem and overwrite your current \
         unsaved changes, or keep the current version?",
        filename
    );

    let style = require_table(lua, "core.style")?;
    let font: LuaValue = style.get("font")?;
    let buttons = lua.create_table()?;

    let keep_btn = lua.create_table()?;
    keep_btn.set("font", font.clone())?;
    keep_btn.set("text", "Keep Current")?;
    keep_btn.set("default_yes", true)?;
    buttons.push(keep_btn)?;

    let reload_btn = lua.create_table()?;
    reload_btn.set("font", font)?;
    reload_btn.set("text", "Reload from Disk")?;
    reload_btn.set("default_no", true)?;
    buttons.push(reload_btn)?;

    let doc_key = lua.create_registry_value(doc.clone())?;
    let doc_key = Arc::new(doc_key);
    let sk = state_key.clone();
    let callback = lua.create_function(move |lua, item: LuaTable| {
        let text: String = item.get("text")?;
        let doc: LuaTable = lua.registry_value(&doc_key)?;
        let state: LuaTable = lua.registry_value(&sk)?;
        if text == "Reload from Disk" {
            doc.set("reload_prompt_queued", false)?;
            reload_doc(lua, &doc, &state)?;
        } else {
            keep_current_doc(&doc, &state)?;
        }
        Ok(())
    })?;

    nag_view.call_method::<()>("show", ("File Changed", message, buttons, callback))?;
    Ok(())
}

fn flag_doc_changed(
    lua: &Lua,
    doc: &LuaTable,
    mtime: LuaValue,
    prompt_now: bool,
    state_key: Arc<LuaRegistryKey>,
) -> LuaResult<()> {
    doc.set("deferred_reload", true)?;
    doc.set("deferred_reload_mtime", mtime)?;
    if prompt_now {
        check_prompt_reload(lua, doc, state_key)?;
    }
    Ok(())
}

fn check_open_docs(
    lua: &Lua,
    prompt_dirty_docs: bool,
    state: &LuaTable,
    state_key: Arc<LuaRegistryKey>,
) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let docs: LuaTable = core.get("docs")?;
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let ar_cfg: LuaTable = plugins.get("autoreload")?;
    let always_nag: bool = ar_cfg.get("always_show_nagview").unwrap_or(false);
    let times: LuaTable = state.get("times")?;
    let system: LuaTable = lua.globals().get("system")?;

    for pair in docs.sequence_values::<LuaTable>() {
        let doc = pair?;
        let abs_filename: Option<String> = doc.get("abs_filename")?;
        let new_file: bool = doc.get("new_file").unwrap_or(false);
        if let Some(path) = abs_filename {
            if !new_file {
                let info: Option<LuaTable> = system.call_function("get_file_info", path.clone())?;
                if let Some(info) = info {
                    let mtime: LuaValue = info.get("modified")?;
                    let cached: LuaValue = times.raw_get(doc.clone())?;
                    let changed = !lua_values_equal(&mtime, &cached);
                    if changed {
                        let is_dirty: bool = doc.call_method("is_dirty", ())?;
                        if !is_dirty && !always_nag {
                            reload_doc(lua, &doc, state)?;
                        } else {
                            flag_doc_changed(
                                lua,
                                &doc,
                                mtime,
                                prompt_dirty_docs,
                                state_key.clone(),
                            )?;
                        }
                    }
                } else {
                    // File no longer exists on disk.
                    let cached: LuaValue = times.raw_get(doc.clone())?;
                    if cached != LuaValue::Nil {
                        let core: LuaTable = require_table(lua, "core")?;
                        let log_fn: LuaFunction = core.get("log")?;
                        let name: String = doc.call_method("get_name", ())?;
                        log_fn.call::<()>(format!("File deleted from disk: {name}"))?;
                        let nag: LuaTable = core.get("nag_view")?;
                        let msg = format!("\"{path}\" has been deleted from disk.");
                        let buttons = lua.create_table()?;
                        let ok_btn = lua.create_table()?;
                        ok_btn.set("text", "OK")?;
                        ok_btn.set("default_yes", true)?;
                        buttons.raw_set(1, ok_btn)?;
                        nag.call_method::<()>(
                            "show",
                            (
                                "File Deleted",
                                msg,
                                buttons,
                                lua.create_function(|_, _: LuaTable| Ok(()))?,
                            ),
                        )?;
                        times.raw_set(doc, LuaValue::Nil)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn lua_values_equal(a: &LuaValue, b: &LuaValue) -> bool {
    match (a, b) {
        (LuaValue::Nil, LuaValue::Nil) => true,
        (LuaValue::Boolean(x), LuaValue::Boolean(y)) => x == y,
        (LuaValue::Integer(x), LuaValue::Integer(y)) => x == y,
        (LuaValue::Number(x), LuaValue::Number(y)) => x == y,
        (LuaValue::Integer(x), LuaValue::Number(y)) => (*x as f64) == *y,
        (LuaValue::Number(x), LuaValue::Integer(y)) => *x == (*y as f64),
        (LuaValue::String(x), LuaValue::String(y)) => x.as_bytes() == y.as_bytes(),
        (LuaValue::Table(x), LuaValue::Table(y)) => x == y,
        _ => false,
    }
}

fn watch_directory(lua: &Lua, state: &LuaTable, path: &str) -> LuaResult<()> {
    let common = require_table(lua, "core.common")?;
    let dirname: LuaFunction = common.get("dirname")?;
    let dir: String = dirname.call(path.to_owned())?;
    if dir.is_empty() {
        return Ok(());
    }
    let watched_dirs: LuaTable = state.get("watched_dirs")?;
    let count: i64 = watched_dirs.get::<Option<i64>>(dir.as_str())?.unwrap_or(0);
    if count == 0 {
        let watch: LuaTable = state.get("watch")?;
        watch.call_method::<()>("watch", dir.clone())?;
    }
    watched_dirs.set(dir, count + 1)?;
    Ok(())
}

fn unwatch_directory(lua: &Lua, state: &LuaTable, path: &str) -> LuaResult<()> {
    let common = require_table(lua, "core.common")?;
    let dirname: LuaFunction = common.get("dirname")?;
    let dir: String = dirname.call(path.to_owned())?;
    if dir.is_empty() {
        return Ok(());
    }
    let watched_dirs: LuaTable = state.get("watched_dirs")?;
    let count: i64 = watched_dirs.get::<Option<i64>>(dir.as_str())?.unwrap_or(0);
    if count <= 1 {
        let watch: LuaTable = state.get("watch")?;
        watch.call_method::<()>("unwatch", dir.clone())?;
        watched_dirs.set(dir, LuaValue::Nil)?;
    } else {
        watched_dirs.set(dir, count - 1)?;
    }
    Ok(())
}

fn doc_changes_visibility(
    lua: &Lua,
    doc: Option<LuaTable>,
    visibility: bool,
    state: &LuaTable,
    state_key: Arc<LuaRegistryKey>,
) -> LuaResult<()> {
    let Some(doc) = doc else { return Ok(()) };
    let visible: LuaTable = state.get("visible")?;
    let abs_filename: Option<String> = doc.get("abs_filename")?;
    if abs_filename.is_none() {
        return Ok(());
    }
    let path = abs_filename.unwrap();

    let cur_vis: LuaValue = visible.raw_get(doc.clone())?;
    let cur_vis_bool = match &cur_vis {
        LuaValue::Boolean(b) => Some(*b),
        LuaValue::Nil => None,
        _ => None,
    };
    if cur_vis_bool == Some(visibility) {
        return Ok(());
    }

    visible.raw_set(doc.clone(), visibility)?;
    if visibility {
        check_prompt_reload(lua, &doc, state_key)?;
    }
    // Always keep the native watch active for open files; only toggle visibility
    // tracking so prompts are deferred until the user switches back.
    let watch: LuaTable = state.get("watch")?;
    let already_watched: bool = {
        let native_watches: LuaTable = watch.get("native_watches")?;
        let existing: LuaValue = native_watches.get(path.as_str())?;
        !matches!(existing, LuaValue::Nil)
    };
    if !already_watched {
        watch.call_method::<()>("watch", path.clone())?;
        watch_directory(lua, state, &path)?;
    }
    Ok(())
}

fn start_background_thread(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let sk = state_key.clone();
    // One tick of dirwatch work. coroutine.yield cannot be called from a Rust
    // C function (lua_call has no continuation), so the loop+yield live in a
    // thin Lua wrapper below.
    let tick = lua.create_function(move |lua, (): ()| -> LuaResult<()> {
        let state: LuaTable = lua.registry_value(&sk)?;
        let watch: LuaTable = state.get("watch")?;
        let sk2 = sk.clone();
        let callback = lua.create_function(move |lua, file: String| {
            let state: LuaTable = lua.registry_value(&sk2)?;
            let times: LuaTable = state.get("times")?;
            let system: LuaTable = lua.globals().get("system")?;
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let ar_cfg: LuaTable = plugins.get("autoreload")?;
            let always_nag: bool = ar_cfg.get("always_show_nagview").unwrap_or(false);
            let core = require_table(lua, "core")?;
            let docs: LuaTable = core.get("docs")?;

            // Check if the changed path matches any open document directly,
            // or if it is a directory containing open documents.
            for pair in docs.sequence_values::<LuaTable>() {
                let doc = pair?;
                let abs_filename: Option<String> = doc.get("abs_filename")?;
                let Some(ref doc_path) = abs_filename else {
                    continue;
                };
                let matches = *doc_path == file || doc_path.starts_with(&format!("{}/", file));
                if !matches {
                    continue;
                }
                let info: Option<LuaTable> =
                    system.call_function("get_file_info", doc_path.clone())?;
                if let Some(info) = info {
                    let mtime: LuaValue = info.get("modified")?;
                    let cached: LuaValue = times.raw_get(doc.clone())?;
                    if !lua_values_equal(&mtime, &cached) {
                        let is_dirty: bool = doc.call_method("is_dirty", ())?;
                        if !is_dirty && !always_nag {
                            reload_doc(lua, &doc, &state)?;
                        } else {
                            let active_view: LuaTable = core.get("active_view")?;
                            let active_doc: Option<LuaTable> = active_view.get("doc")?;
                            let is_active = active_doc.map(|d| d == doc).unwrap_or(false);
                            flag_doc_changed(lua, &doc, mtime, is_active, sk2.clone())?;
                        }
                    }
                }
            }
            Ok(())
        })?;
        watch.call_method::<()>("check", callback)
    })?;

    // Lua wrapper: loops and yields — only Lua functions may yield in Lua 5.4.
    let thread_fn: LuaFunction = lua
        .load("local t = ...; return function() while true do t(); coroutine.yield(0.05) end end")
        .call::<LuaFunction>(tick)?;

    let core = require_table(lua, "core")?;
    core.get::<LuaFunction>("add_thread")?
        .call::<()>(thread_fn)?;
    Ok(())
}

fn patch_core_set_active_view(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let old: LuaFunction = core.get("set_active_view")?;
    let old_key = lua.create_registry_value(old)?;
    let sk = state_key;

    core.set(
        "set_active_view",
        lua.create_function(move |lua, view: LuaTable| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>(view.clone())?;
            let state: LuaTable = lua.registry_value(&sk)?;
            let doc: Option<LuaTable> = view.get("doc")?;
            doc_changes_visibility(lua, doc, true, &state, sk.clone())?;
            Ok(())
        })?,
    )?;
    Ok(())
}

fn patch_node_set_active_view(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let node = require_table(lua, "core.node")?;
    let old: LuaFunction = node.get("set_active_view")?;
    let old_key = lua.create_registry_value(old)?;
    let sk = state_key;

    node.set(
        "set_active_view",
        lua.create_function(move |lua, (this, view): (LuaTable, LuaTable)| {
            let state: LuaTable = lua.registry_value(&sk)?;
            // Mark previously active view's doc as no longer visible.
            let prev: Option<LuaTable> = this.get("active_view")?;
            if let Some(prev_view) = prev {
                let doc: Option<LuaTable> = prev_view.get("doc")?;
                doc_changes_visibility(lua, doc, false, &state, sk.clone())?;
            }
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>((this, view.clone()))?;
            let doc: Option<LuaTable> = view.get("doc")?;
            doc_changes_visibility(lua, doc.clone(), true, &state, sk.clone())?;
            // Persist active file (skip during exit teardown and non-doc views).
            let core_t = require_table(lua, "core")?;
            let quitting = matches!(core_t.get::<LuaValue>("_exiting")?, LuaValue::Boolean(true));
            if !quitting {
                if let Some(ref d) = doc {
                    let userdir: String = lua.globals().get("USERDIR")?;
                    let dir = std::path::PathBuf::from(&userdir)
                        .join("storage")
                        .join("session");
                    if let Err(e) = std::fs::create_dir_all(&dir) {
                        log::warn!("failed to create session dir: {e}");
                    }
                    let content = if let LuaValue::String(s) =
                        d.get::<LuaValue>("abs_filename")?
                    {
                        format!("\"{}\"", s.to_str()?)
                    } else {
                        String::new()
                    };
                    if let Err(e) = std::fs::write(dir.join("active_file"), &content) {
                        log::warn!("failed to write active_file: {e}");
                    }
                }
            }
            Ok(())
        })?,
    )?;
    Ok(())
}

fn patch_rootview_on_focus_gained(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let rootview = require_table(lua, "core.rootview")?;
    let old: LuaFunction = rootview.get("on_focus_gained")?;
    let old_key = lua.create_registry_value(old)?;
    let sk = state_key;

    rootview.set(
        "on_focus_gained",
        lua.create_function(move |lua, (this, rest): (LuaTable, LuaMultiValue)| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let mut args = LuaMultiValue::new();
            args.push_back(LuaValue::Table(this));
            args.extend(rest);
            old.call::<()>(args)?;
            let state: LuaTable = lua.registry_value(&sk)?;
            check_open_docs(lua, true, &state, sk.clone())?;
            Ok(())
        })?,
    )?;
    Ok(())
}

fn patch_doc_load_save(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let doc = require_table(lua, "core.doc")?;

    // Patch Doc.load
    {
        let old_load: LuaFunction = doc.get("load")?;
        let old_key = lua.create_registry_value(old_load)?;
        let sk = state_key.clone();
        doc.set(
            "load",
            lua.create_function(move |lua, (this, args): (LuaTable, LuaMultiValue)| {
                let old: LuaFunction = lua.registry_value(&old_key)?;
                let mut call_args = LuaMultiValue::new();
                call_args.push_back(LuaValue::Table(this.clone()));
                call_args.extend(args);
                let res: LuaMultiValue = old.call(call_args)?;
                let state: LuaTable = lua.registry_value(&sk)?;
                update_time(lua, &this, &state)?;
                Ok(res)
            })?,
        )?;
    }

    // Patch Doc.save
    {
        let old_save: LuaFunction = doc.get("save")?;
        let old_key = lua.create_registry_value(old_save)?;
        let sk = state_key;
        doc.set(
            "save",
            lua.create_function(move |lua, (this, args): (LuaTable, LuaMultiValue)| {
                let old: LuaFunction = lua.registry_value(&old_key)?;
                let mut call_args = LuaMultiValue::new();
                call_args.push_back(LuaValue::Table(this.clone()));
                call_args.extend(args);
                let res: LuaMultiValue = old.call(call_args)?;

                let state: LuaTable = lua.registry_value(&sk)?;
                let abs_filename: Option<String> = this.get("abs_filename")?;

                if let Err(e) = (|| -> LuaResult<()> {
                    let watch: LuaTable = state.get("watch")?;
                    let times: LuaTable = state.get("times")?;
                    let mtime: LuaValue = times.raw_get(this.clone())?;
                    // If this doc had no mtime yet, it just got a filename — start watching.
                    if matches!(mtime, LuaValue::Nil) {
                        if let Some(ref path) = abs_filename {
                            watch.call_method::<()>("watch", (path.clone(), true))?;
                        }
                    }
                    update_time(lua, &this, &state)?;
                    Ok(())
                })() {
                    let name: String = this.call_method("get_name", ()).unwrap_or_default();
                    if let Ok(core) = require_table(lua, "core") {
                        let _: LuaResult<()> = core.call_function(
                            "error",
                            format!("Post-save autoreload hook failed for {}: {}", name, e),
                        );
                    }
                }
                Ok(res)
            })?,
        )?;
    }
    Ok(())
}

fn patch_doc_on_close(lua: &Lua, state_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let doc = require_table(lua, "core.doc")?;
    let old_on_close: LuaFunction = doc.get("on_close")?;
    let old_key = lua.create_registry_value(old_on_close)?;
    let sk = state_key;

    doc.set(
        "on_close",
        lua.create_function(move |lua, (this, args): (LuaTable, LuaMultiValue)| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let abs_filename: Option<String> = this.get("abs_filename")?;
            if let Some(ref path) = abs_filename {
                let watch: LuaTable = state.get("watch")?;
                watch.call_method::<()>("unwatch", path.clone())?;
                unwatch_directory(lua, &state, path)?;
            }
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let mut call_args = LuaMultiValue::new();
            call_args.push_back(LuaValue::Table(this));
            call_args.extend(args);
            old.call::<LuaMultiValue>(call_args)?;
            Ok(())
        })?,
    )?;
    Ok(())
}

/// Registers `plugins.autoreload`: file-watching via dirwatch with deferred reload prompts,
/// visibility tracking, and focus-gain checks.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.autoreload",
        lua.create_function(|lua, ()| {
            // Config defaults.
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let common = require_table(lua, "core.common")?;
            let defaults = lua.create_table()?;
            defaults.set("always_show_nagview", false)?;

            let spec = lua.create_table()?;
            spec.set("name", "Autoreload")?;
            let entry = lua.create_table()?;
            entry.set("label", "Always Show Nagview")?;
            entry.set(
                "description",
                "Alerts you if an opened file changes externally even if you haven't modified it.",
            )?;
            entry.set("path", "always_show_nagview")?;
            entry.set("type", "toggle")?;
            entry.set("default", false)?;
            spec.push(entry)?;
            defaults.set("config_spec", spec)?;

            let merged: LuaTable = common
                .call_function("merge", (defaults, plugins.get::<LuaValue>("autoreload")?))?;
            plugins.set("autoreload", merged)?;

            // Shared state (weak-keyed tables + dirwatch).
            let state = create_state_table(lua)?;
            let state_key = Arc::new(lua.create_registry_value(state)?);

            start_background_thread(lua, state_key.clone())?;
            patch_core_set_active_view(lua, state_key.clone())?;
            patch_node_set_active_view(lua, state_key.clone())?;
            patch_rootview_on_focus_gained(lua, state_key.clone())?;
            patch_doc_load_save(lua, state_key.clone())?;
            patch_doc_on_close(lua, state_key)?;

            Ok(LuaValue::Boolean(true))
        })?,
    )
}
