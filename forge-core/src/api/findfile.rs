use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn set_config_defaults(lua: &Lua) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let common = require_table(lua, "core.common")?;

    let fps: f64 = config.get("fps").unwrap_or(60.0);
    let defaults = lua.create_table()?;
    defaults.set("file_limit", 20000)?;
    defaults.set("max_search_time", 10.0f64)?;
    defaults.set("interval", 0)?;
    defaults.set("max_loop_time", 0.5 / fps)?;

    let merged: LuaTable =
        common.call_function("merge", (defaults, plugins.get::<LuaValue>("findfile")?))?;
    plugins.set("findfile", merged)?;
    Ok(())
}

fn register_find_file_command(lua: &Lua) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;
    let cmds = lua.create_table()?;

    let find_fn = lua.create_function(|lua, ()| {
        let core = require_table(lua, "core")?;
        let projects: LuaTable = core.get("projects")?;

        if projects.raw_len() == 0 {
            let command = require_table(lua, "core.command")?;
            command.call_function::<()>("perform", "core:open-file")?;
            return Ok(());
        }

        let config = require_table(lua, "core.config")?;
        let plugins: LuaTable = config.get("plugins")?;
        let findfile_cfg: LuaTable = plugins.get("findfile")?;
        let file_limit: usize = findfile_cfg.get("file_limit")?;

        let native_project_model = require_table(lua, "project_model")?;
        let native_picker = require_table(lua, "picker")?;

        // Collect project roots.
        let roots = lua.create_table()?;
        let n = projects.raw_len();
        for i in 1..=n {
            let proj: LuaTable = projects.get(i)?;
            let path: String = proj.get("path")?;
            roots.set(i, path)?;
        }

        // config.file_size_limit and project_scan.exclude_dirs for the native call.
        let file_size_limit: f64 = config.get("file_size_limit").unwrap_or(10.0);
        let scan_cfg: Option<LuaTable> = config.get("project_scan")?;
        let exclude_dirs: LuaValue = scan_cfg
            .as_ref()
            .and_then(|t| t.get("exclude_dirs").ok())
            .unwrap_or(LuaValue::Nil);

        let opts = lua.create_table()?;
        opts.set("max_size_bytes", (file_size_limit * 1e6) as i64)?;
        opts.set("max_files", file_limit as i64)?;
        opts.set("exclude_dirs", exclude_dirs)?;

        let cached: LuaTable =
            native_project_model.call_function("get_all_files", (roots.clone(), opts))?;

        // Build the file list from cached results.
        let files: LuaTable = lua.create_table()?;
        let common = require_table(lua, "core.common")?;
        let cached_len = cached.raw_len();
        let mut count = 0usize;
        for i in 1..=cached_len {
            if count >= file_limit {
                break;
            }
            let filename: String = cached.get(i)?;
            // Find which project this belongs to and compute relative path.
            for j in 1..=n {
                let proj: LuaTable = projects.get(j)?;
                let path: String = proj.get("path")?;
                let belongs: bool =
                    common.call_function("path_belongs_to", (filename.clone(), path.clone()))?;
                if belongs {
                    let info = lua.create_table()?;
                    info.set("type", "file")?;
                    info.set("size", 0)?;
                    info.set("filename", filename.clone())?;
                    let ignored: bool = proj.call_method("is_ignored", (info, filename.clone()))?;
                    if !ignored {
                        let display = if j == 1 {
                            // Relative to first project: strip leading path + separator.
                            filename[path.len() + 1..].to_owned()
                        } else {
                            common.call_function("home_encode", filename.clone())?
                        };
                        count += 1;
                        files.set(count, display)?;
                    }
                    break;
                }
            }
        }

        let files_key = lua.create_registry_value(files)?;
        let native_picker_key = lua.create_registry_value(native_picker)?;

        let visited_files: LuaValue = core.get("visited_files")?;
        let visited_key = lua.create_registry_value(visited_files)?;

        let complete_flag = lua.create_table()?;
        complete_flag.set("done", false)?;
        let complete_key = lua.create_registry_value(complete_flag)?;

        let opts = lua.create_table()?;

        opts.set(
            "submit",
            lua.create_function(move |lua, (text, item): (String, LuaValue)| {
                let text = match &item {
                    LuaValue::Table(t) => t.get::<String>("text").unwrap_or(text),
                    _ => text,
                };
                let common = require_table(lua, "core.common")?;
                let expanded: String = common.call_function("home_expand", text)?;
                let core = require_table(lua, "core")?;
                let doc: LuaTable = core.call_function("open_doc", expanded)?;
                let root_view: LuaTable = core.get("root_view")?;
                root_view.call_method::<()>("open_doc", doc)?;
                let complete: LuaTable = lua.registry_value(&complete_key)?;
                complete.set("done", true)?;
                Ok(())
            })?,
        )?;

        let fk2 = lua.create_registry_value(lua.registry_value::<LuaTable>(&files_key)?)?;
        let pk2 = lua.create_registry_value(lua.registry_value::<LuaTable>(&native_picker_key)?)?;
        let vk2 = lua.create_registry_value(lua.registry_value::<LuaValue>(&visited_key)?)?;
        let original_files_key_cell = std::sync::Arc::new(parking_lot::Mutex::new(None::<LuaTable>));
        let ofc = original_files_key_cell.clone();
        opts.set(
            "suggest",
            lua.create_function(move |lua, text: String| {
                let files: LuaTable = lua.registry_value(&fk2)?;
                let picker: LuaTable = lua.registry_value(&pk2)?;
                let visited: LuaValue = lua.registry_value(&vk2)?;

                if text.is_empty() {
                    let cached = ofc.lock();
                    if let Some(ref t) = *cached {
                        return Ok(LuaValue::Table(t.clone()));
                    }
                }

                let ranked: LuaTable = picker.call_function(
                    "rank_strings",
                    (
                        files,
                        text.clone(),
                        true,
                        if text.is_empty() { visited } else { LuaValue::Nil },
                    ),
                )?;
                if text.is_empty() {
                    *ofc.lock() = Some(ranked.clone());
                }
                Ok(LuaValue::Table(ranked))
            })?,
        )?;

        opts.set(
            "cancel",
            lua.create_function(move |_lua, ()| Ok(()))?,
        )?;

        let command_view: LuaTable = core.get("command_view")?;
        command_view.call_method::<()>("enter", ("Open File From Project", opts))?;

        Ok(())
    })?;

    cmds.set("core:find-file", find_fn)?;
    command.call_function::<()>("add", (LuaValue::Nil, cmds))?;

    Ok(())
}

fn register_keymap(lua: &Lua) -> LuaResult<()> {
    let keymap = require_table(lua, "core.keymap")?;
    let platform: String = lua.globals().get("PLATFORM").unwrap_or_default();
    let bindings = lua.create_table()?;
    let key = if platform == "Mac OS X" {
        "cmd+shift+o"
    } else {
        "ctrl+shift+o"
    };
    bindings.set(key, "core:find-file")?;
    keymap.call_function::<()>("add", bindings)?;
    Ok(())
}

/// Registers `plugins.findfile`: config defaults, `core:find-file` command, and keymap binding.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.findfile",
        lua.create_function(|lua, ()| {
            set_config_defaults(lua)?;
            register_find_file_command(lua)?;
            register_keymap(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )
}
