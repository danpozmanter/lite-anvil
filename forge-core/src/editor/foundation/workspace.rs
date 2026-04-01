use mlua::prelude::*;

const STORAGE_MODULE: &str = "ws";

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn get_metatable(lua: &Lua, value: LuaValue) -> LuaResult<LuaValue> {
    let getmetatable: LuaFunction = lua.globals().get("getmetatable")?;
    getmetatable.call(value)
}

fn call_callable(lua: &Lua, callable: LuaValue, args: LuaMultiValue) -> LuaResult<LuaValue> {
    let helper: LuaFunction = lua
        .load("return function(callable, ...) return callable(...) end")
        .eval()?;
    let mut all = LuaMultiValue::new();
    all.push_front(callable);
    for arg in args {
        all.push_back(arg);
    }
    helper.call(all)
}

fn is_terminal_view(view: &LuaTable) -> LuaResult<bool> {
    let tostring_fn = match view.get::<Option<LuaFunction>>("__tostring")? {
        Some(func) => func,
        None => return Ok(false),
    };
    let name: String = tostring_fn.call(view.clone())?;
    Ok(name == "TerminalView")
}

fn bool_field(table: &LuaTable, key: &str) -> LuaResult<bool> {
    Ok(table.get::<Option<bool>>(key)?.unwrap_or(false))
}

fn scroll_snapshot(lua: &Lua, view: &LuaTable) -> LuaResult<LuaTable> {
    let scroll: LuaTable = view.get("scroll")?;
    let to: LuaTable = scroll.get("to")?;
    let out = lua.create_table()?;
    out.set(
        "x",
        to.get::<Option<LuaValue>>("x")?
            .unwrap_or(LuaValue::Integer(0)),
    )?;
    out.set(
        "y",
        to.get::<Option<LuaValue>>("y")?
            .unwrap_or(LuaValue::Integer(0)),
    )?;
    Ok(out)
}

fn active_equals(core: &LuaTable, view: &LuaTable) -> LuaResult<bool> {
    Ok(core.get::<LuaValue>("active_view")? == LuaValue::Table(view.clone()))
}

fn save_view(lua: &Lua, core: &LuaTable, view: &LuaTable) -> LuaResult<Option<LuaTable>> {
    let doc_view = require_table(lua, "core.docview")?;
    let log_view = require_table(lua, "core.logview")?;
    let package: LuaTable = lua.globals().get("package")?;
    let loaded: LuaTable = package.get("loaded")?;
    let mt = get_metatable(lua, LuaValue::Table(view.clone()))?;

    if mt == LuaValue::Table(doc_view.clone()) {
        let doc: LuaTable = view.get("doc")?;
        let out = lua.create_table()?;
        out.set("type", "doc")?;
        out.set("active", active_equals(core, view)?)?;
        out.set("filename", doc.get::<LuaValue>("filename")?)?;
        let selection: LuaMultiValue = doc.call_method("get_selection", ())?;
        let sel = lua.create_table()?;
        for (idx, value) in selection.into_iter().enumerate() {
            sel.raw_set((idx + 1) as i64, value)?;
        }
        out.set("selection", sel)?;
        out.set("scroll", scroll_snapshot(lua, view)?)?;
        out.set("crlf", doc.get::<LuaValue>("crlf")?)?;
        if bool_field(&doc, "new_file")? {
            let text: LuaValue = doc.call_method(
                "get_text",
                (
                    1,
                    1,
                    LuaValue::Number(f64::INFINITY),
                    LuaValue::Number(f64::INFINITY),
                ),
            )?;
            out.set("text", text)?;
        }
        return Ok(Some(out));
    }

    if is_terminal_view(view)? {
        let handle: Option<LuaAnyUserData> = view.get("handle")?;
        if let Some(handle) = handle {
            let running = handle.call_method::<bool>("running", ())?;
            if !running {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }
        let out = lua.create_table()?;
        out.set("type", "terminal")?;
        out.set("active", active_equals(core, view)?)?;
        out.set("cwd", view.get::<LuaValue>("cwd")?)?;
        out.set("title", view.get::<LuaValue>("title")?)?;
        out.set("placement", view.get::<LuaValue>("open_placement")?)?;
        out.set("color_scheme", view.get::<LuaValue>("color_scheme")?)?;
        out.set("scroll", scroll_snapshot(lua, view)?)?;
        return Ok(Some(out));
    }

    if mt == LuaValue::Table(log_view) {
        return Ok(None);
    }

    for pair in loaded.pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair?;
        if value == mt {
            if let LuaValue::String(name) = key {
                let out = lua.create_table()?;
                out.set("type", "view")?;
                out.set("active", active_equals(core, view)?)?;
                out.set("module", name)?;
                let scroll = scroll_snapshot(lua, view)?;
                let wrapper = lua.create_table()?;
                wrapper.set("x", scroll.get::<LuaValue>("x")?)?;
                wrapper.set("y", scroll.get::<LuaValue>("y")?)?;
                let to = lua.create_table()?;
                to.set("x", scroll.get::<LuaValue>("x")?)?;
                to.set("y", scroll.get::<LuaValue>("y")?)?;
                wrapper.set("to", to)?;
                out.set("scroll", wrapper)?;
                return Ok(Some(out));
            }
        }
    }

    Ok(None)
}

fn save_node(lua: &Lua, core: &LuaTable, node: &LuaTable) -> LuaResult<Option<LuaTable>> {
    let node_type: String = node.get("type")?;
    if node_type == "leaf" {
        if node.get::<Option<LuaValue>>("locked")?.is_some() {
            return Ok(None);
        }
        let out = lua.create_table()?;
        out.set("type", "leaf")?;
        let views: LuaTable = node.get("views")?;
        let saved = lua.create_table()?;
        let active_view: LuaValue = node.get("active_view")?;
        let mut saved_idx = 0i64;
        for view in views.sequence_values::<LuaTable>() {
            let view = view?;
            if let Some(entry) = save_view(lua, core, &view)? {
                saved_idx += 1;
                let v: LuaTable = entry;
                if active_view == LuaValue::Table(view.clone()) {
                    out.set("active_view", saved_idx)?;
                }
                saved.raw_set(saved_idx, v)?;
            }
        }
        if saved_idx == 0 {
            return Ok(None);
        }
        out.set("views", saved)?;
        return Ok(Some(out));
    }

    let a: LuaTable = node.get("a")?;
    let b: LuaTable = node.get("b")?;
    let saved_a = save_node(lua, core, &a)?;
    let saved_b = save_node(lua, core, &b)?;
    match (saved_a, saved_b) {
        (None, None) => Ok(None),
        (Some(a), None) => Ok(Some(a)),
        (None, Some(b)) => Ok(Some(b)),
        (Some(a), Some(b)) => {
            let out = lua.create_table()?;
            out.set("type", node_type)?;
            out.set("divider", node.get::<LuaValue>("divider")?)?;
            out.set("a", a)?;
            out.set("b", b)?;
            Ok(Some(out))
        }
    }
}

// `lua` is passed through to recursive calls as required by mlua signatures.
#[allow(clippy::only_used_in_recursion)]
fn has_no_locked_children(lua: &Lua, node: &LuaTable) -> LuaResult<bool> {
    if node.get::<Option<LuaValue>>("locked")?.is_some() {
        return Ok(false);
    }
    let node_type: String = node.get("type")?;
    if node_type == "leaf" {
        return Ok(true);
    }
    let a: LuaTable = node.get("a")?;
    let b: LuaTable = node.get("b")?;
    Ok(has_no_locked_children(lua, &a)? && has_no_locked_children(lua, &b)?)
}

fn get_unlocked_root(lua: &Lua, node: &LuaTable) -> LuaResult<Option<LuaTable>> {
    let node_type: String = node.get("type")?;
    if node_type == "leaf" {
        return if node.get::<Option<LuaValue>>("locked")?.is_none() {
            Ok(Some(node.clone()))
        } else {
            Ok(None)
        };
    }
    if has_no_locked_children(lua, node)? {
        return Ok(Some(node.clone()));
    }
    let a: LuaTable = node.get("a")?;
    if let Some(root) = get_unlocked_root(lua, &a)? {
        return Ok(Some(root));
    }
    let b: LuaTable = node.get("b")?;
    get_unlocked_root(lua, &b)
}

fn set_scroll(view: &LuaTable, scroll_src: Option<LuaTable>) -> LuaResult<()> {
    let Some(scroll_src) = scroll_src else {
        return Ok(());
    };
    let scroll: LuaTable = view.get("scroll")?;
    let x = scroll_src
        .get::<Option<LuaValue>>("x")?
        .unwrap_or(LuaValue::Integer(0));
    let y = scroll_src
        .get::<Option<LuaValue>>("y")?
        .unwrap_or(LuaValue::Integer(0));
    scroll.set("x", x.clone())?;
    scroll.set("y", y.clone())?;
    let to: LuaTable = scroll.get("to")?;
    to.set("x", x)?;
    to.set("y", y)?;
    Ok(())
}

fn load_view(
    lua: &Lua,
    core: &LuaTable,
    spec: &LuaTable,
    project_path: &str,
) -> LuaResult<Option<LuaTable>> {
    let spec_type: String = spec.get("type")?;
    if spec_type == "doc" {
        let doc_view = require_table(lua, "core.docview")?;
        let open_doc: LuaFunction = core.get("open_doc")?;
        let filename: LuaValue = spec.get("filename")?;
        let was_new_file = spec.get::<Option<LuaString>>("text")?.is_some();
        let doc: LuaValue = match filename {
            LuaValue::Nil => open_doc.call(())?,
            value => {
                let filename = match value {
                    LuaValue::String(s) => s.to_str()?.to_string(),
                    _ => return Ok(None),
                };
                // Resolve relative paths against the project root.
                let resolved = if std::path::Path::new(&filename).is_absolute() {
                    filename
                } else {
                    format!("{}/{}", project_path, filename)
                };
                let active = bool_field(spec, "active")?;
                if active {
                    match open_doc.call::<LuaValue>(resolved) {
                        Ok(doc) => doc,
                        Err(_) => return Ok(None),
                    }
                } else {
                    let opts = lua.create_table()?;
                    opts.set("lazy_restore", true)?;
                    match open_doc.call::<LuaValue>((resolved, opts)) {
                        Ok(doc) => doc,
                        Err(_) => return Ok(None),
                    }
                }
            }
        };
        let view = match call_callable(
            lua,
            LuaValue::Table(doc_view),
            LuaMultiValue::from_vec(vec![doc]),
        )? {
            LuaValue::Table(t) => t,
            _ => return Ok(None),
        };
        let doc: LuaTable = view.get("doc")?;
        // If the file no longer exists on disk but was a real file (not
        // an unsaved scratch buffer), skip it instead of showing a
        // phantom blank doc.
        if bool_field(&doc, "new_file")? && !was_new_file {
            return Ok(None);
        }
        if bool_field(&doc, "new_file")? {
            if let Some(text) = spec.get::<Option<LuaString>>("text")? {
                doc.call_method::<()>("insert", (1, 1, text.to_str()?.to_string()))?;
                doc.set("crlf", spec.get::<LuaValue>("crlf")?)?;
            }
        }
        let selection: LuaTable = spec.get("selection")?;
        let mut args = LuaMultiValue::new();
        for value in selection.sequence_values::<LuaValue>() {
            args.push_back(value?);
        }
        doc.call_method::<()>("set_selection", args)?;
        let current_sel: LuaMultiValue = doc.call_method("get_selection", ())?;
        let names = ["last_line1", "last_col1", "last_line2", "last_col2"];
        for (idx, value) in current_sel.into_iter().enumerate().take(4) {
            view.set(names[idx], value)?;
        }
        set_scroll(&view, spec.get::<Option<LuaTable>>("scroll")?)?;
        return Ok(Some(view));
    }

    if spec_type == "terminal"
        || (spec_type == "view"
            && spec.get::<Option<String>>("module")? == Some("plugins.terminal.view".to_string()))
    {
        let terminal_view = require_table(lua, "plugins.terminal.view")?;
        let opts = lua.create_table()?;
        opts.set("cwd", spec.get::<LuaValue>("cwd")?)?;
        opts.set("title", spec.get::<LuaValue>("title")?)?;
        opts.set("restored", true)?;
        if spec_type == "terminal" {
            opts.set("placement", spec.get::<LuaValue>("placement")?)?;
            opts.set("color_scheme", spec.get::<LuaValue>("color_scheme")?)?;
        }
        let view = match call_callable(
            lua,
            LuaValue::Table(terminal_view),
            LuaMultiValue::from_vec(vec![LuaValue::Table(opts)]),
        )? {
            LuaValue::Table(t) => t,
            _ => return Ok(None),
        };
        set_scroll(&view, spec.get::<Option<LuaTable>>("scroll")?)?;
        return Ok(Some(view));
    }

    let module: String = spec.get("module")?;
    let require: LuaFunction = lua.globals().get("require")?;
    let callable: LuaValue = require.call(module)?;
    let view = match call_callable(lua, callable, LuaMultiValue::new())? {
        LuaValue::Table(t) => t,
        _ => return Ok(None),
    };
    if spec_type != "terminal" {
        set_scroll(&view, spec.get::<Option<LuaTable>>("scroll")?)?;
    }
    Ok(Some(view))
}

fn load_node(
    lua: &Lua,
    node: &LuaTable,
    spec: &LuaTable,
    project_path: &str,
) -> LuaResult<Option<LuaTable>> {
    let doc_view = require_table(lua, "core.docview")?;
    let spec_type: String = spec.get("type")?;
    if spec_type == "leaf" {
        let views: LuaTable = spec.get("views")?;
        let mut result: Option<LuaTable> = None;
        let mut active_view: Option<LuaTable> = None;
        let active_index = spec.get::<Option<i64>>("active_view")?;
        for (idx, value) in views.sequence_values::<LuaTable>().enumerate() {
            let value = value?;
            let idx = idx as i64 + 1;
            if let Some(view) =
                load_view(lua, &require_table(lua, "core")?, &value, project_path)?
            {
                if bool_field(&value, "active")? {
                    result = Some(view.clone());
                }
                node.call_method::<()>("add_view", view.clone())?;
                if active_index == Some(idx) {
                    active_view = Some(view.clone());
                }
                let mt = get_metatable(lua, LuaValue::Table(view.clone()))?;
                if mt != LuaValue::Table(doc_view.clone())
                    && value.get::<Option<String>>("type")? != Some("terminal".to_string())
                {
                    if let Some(scroll) = value.get::<Option<LuaTable>>("scroll")? {
                        view.set("scroll", scroll)?;
                    }
                }
            }
        }
        if let Some(active) = active_view {
            node.call_method::<()>("set_active_view", active)?;
        }
        return Ok(result);
    }

    let dir = if spec_type == "hsplit" {
        "right"
    } else {
        "down"
    };
    node.call_method::<LuaValue>("split", dir)?;
    node.set("divider", spec.get::<LuaValue>("divider")?)?;
    let a: LuaTable = node.get("a")?;
    let b: LuaTable = node.get("b")?;
    let spec_a: LuaTable = spec.get("a")?;
    let spec_b: LuaTable = spec.get("b")?;
    let res1 = load_node(lua, &a, &spec_a, project_path)?;
    let res2 = load_node(lua, &b, &spec_b, project_path)?;
    let a_empty = a.call_method::<bool>("is_empty", ())?;
    let b_empty = b.call_method::<bool>("is_empty", ())?;
    let a_primary = bool_field(&a, "is_primary_node")?;
    let b_primary = bool_field(&b, "is_primary_node")?;
    if a_empty && !a_primary {
        node.call_method::<()>("consume", b)?;
    } else if b_empty && !b_primary {
        node.call_method::<()>("consume", a)?;
    }
    Ok(res1.or(res2))
}

fn save_directories(lua: &Lua, core: &LuaTable) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    let root_project: Option<LuaFunction> = core.get("root_project")?;
    let Some(root_project) = root_project else {
        return Ok(out);
    };
    let project: Option<LuaTable> = root_project.call(())?;
    let Some(project) = project else {
        return Ok(out);
    };
    let project_path: Option<String> = project.get("path")?;
    let Some(project_path) = project_path else {
        return Ok(out);
    };
    let common = require_table(lua, "core.common")?;
    let relative_path: LuaFunction = common.get("relative_path")?;
    let projects: LuaTable = core.get("projects")?;
    let mut idx = 1i64;
    for (i, value) in projects.sequence_values::<LuaTable>().enumerate() {
        if i == 0 {
            continue;
        }
        let project = value?;
        let path: String = project.get("path")?;
        let rel: String = relative_path.call((project_path.clone(), path))?;
        out.raw_set(idx, rel)?;
        idx += 1;
    }
    Ok(out)
}

fn workspace_keys(lua: &Lua, project_dir: &str) -> LuaResult<Vec<String>> {
    let common = require_table(lua, "core.common")?;
    let basename: LuaFunction = common.get("basename")?;
    let storage = require_table(lua, "core.storage")?;
    let keys_fn: LuaFunction = storage.get("keys")?;
    let keys: LuaTable = keys_fn.call(STORAGE_MODULE)?;
    let base: String = basename.call(project_dir)?;
    let mut out = Vec::new();
    for value in keys.sequence_values::<String>() {
        let key = value?;
        if let Some(rest) = key.strip_prefix(&base) {
            if let Some(rest) = rest.strip_prefix('-') {
                if rest.parse::<u64>().is_ok() {
                    out.push(key);
                }
            }
        }
    }
    Ok(out)
}

/// Loads the stored workspace for a project without deleting it from storage.
fn load_stored_workspace(lua: &Lua, project_dir: &str) -> LuaResult<Option<LuaTable>> {
    let storage = require_table(lua, "core.storage")?;
    let load: LuaFunction = storage.get("load")?;
    for key in workspace_keys(lua, project_dir)? {
        if let Ok(Some(workspace)) = load.call::<Option<LuaTable>>((STORAGE_MODULE, key)) {
            let path: Option<String> = workspace.get("path")?;
            if path.as_deref() == Some(project_dir) {
                return Ok(Some(workspace));
            }
        }
    }
    Ok(None)
}

/// Removes stored workspace data for a project from storage.
fn clear_stored_workspace(lua: &Lua, project_dir: &str) -> LuaResult<()> {
    let storage = require_table(lua, "core.storage")?;
    let load: LuaFunction = storage.get("load")?;
    let clear: LuaFunction = storage.get("clear")?;
    for key in workspace_keys(lua, project_dir)? {
        if let Ok(Some(workspace)) = load.call::<Option<LuaTable>>((STORAGE_MODULE, key.clone())) {
            let path: Option<String> = workspace.get("path")?;
            if path.as_deref() == Some(project_dir) {
                clear.call::<()>((STORAGE_MODULE, key))?;
                return Ok(());
            }
        }
    }
    Ok(())
}

fn save_workspace(lua: &Lua) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let root_project: LuaFunction = core.get("root_project")?;
    let project: Option<LuaTable> = root_project.call(())?;
    let Some(project) = project else {
        return Ok(());
    };
    let project_path: Option<String> = project.get("path")?;
    let Some(project_path) = project_path else {
        return Ok(());
    };

    let common = require_table(lua, "core.common")?;
    let basename: LuaFunction = common.get("basename")?;
    let project_base: String = basename.call(project_path.clone())?;
    let storage = require_table(lua, "core.storage")?;
    let load: LuaFunction = storage.get("load")?;
    let clear: LuaFunction = storage.get("clear")?;
    let save: LuaFunction = storage.get("save")?;
    for key in workspace_keys(lua, &project_base)? {
        if let Ok(Some(workspace)) = load.call::<Option<LuaTable>>((STORAGE_MODULE, key.clone())) {
            let path: Option<String> = workspace.get("path")?;
            if path.as_deref() == Some(project_path.as_str()) {
                clear.call::<()>((STORAGE_MODULE, key))?;
            }
        }
    }

    let root_view: LuaTable = core.get("root_view")?;
    let root_node: LuaTable = root_view.get("root_node")?;
    let Some(documents) = save_node(lua, &core, &root_node)? else {
        return Ok(());
    };
    let payload = lua.create_table()?;
    payload.set("path", project_path.clone())?;
    payload.set("documents", documents)?;
    payload.set("directories", save_directories(lua, &core)?)?;
    save.call::<()>((STORAGE_MODULE, format!("{project_base}-1"), payload))?;
    Ok(())
}

fn load_workspace(lua: &Lua) -> LuaResult<bool> {
    let core = require_table(lua, "core")?;
    let root_project: LuaFunction = core.get("root_project")?;
    let project: Option<LuaTable> = root_project.call(())?;
    let Some(project) = project else {
        return Ok(false);
    };
    let project_path: Option<String> = project.get("path")?;
    let Some(project_path) = project_path else {
        return Ok(false);
    };
    let Some(workspace) = load_stored_workspace(lua, &project_path)? else {
        return Ok(false);
    };
    let documents: Option<LuaTable> = workspace.get("documents")?;
    let Some(documents) = documents else {
        return Ok(false);
    };
    core.set("skip_session_restore_open_files", true)?;
    let root_view: LuaTable = core.get("root_view")?;
    let root_node: LuaTable = root_view.get("root_node")?;
    let Some(root) = get_unlocked_root(lua, &root_node)? else {
        return Ok(false);
    };
    let active_view = load_node(lua, &root, &documents, &project_path)?;
    if let Some(active_view) = active_view {
        let set_active_view: LuaFunction = core.get("set_active_view")?;
        set_active_view.call::<()>(active_view)?;
    }
    if let Some(directories) = workspace.get::<Option<LuaTable>>("directories")? {
        let add_project: LuaFunction = core.get("add_project")?;
        let absolute_path: LuaFunction = require_table(lua, "system")?.get("absolute_path")?;
        for dir_name in directories.sequence_values::<String>() {
            let abs: String = absolute_path.call(dir_name?)?;
            add_project.call::<()>(abs)?;
        }
    }
    // Only delete from storage after restore succeeded.
    clear_stored_workspace(lua, &project_path)?;
    Ok(true)
}

fn install(lua: &Lua) -> LuaResult<LuaTable> {
    let core = require_table(lua, "core")?;
    if core
        .get::<Option<bool>>("__workspace_native_installed")?
        .unwrap_or(false)
    {
        return lua.create_table();
    }
    core.set("__workspace_native_installed", true)?;

    // Register palette commands.
    let command = require_table(lua, "core.command")?;
    let cmds = lua.create_table()?;

    cmds.set(
        "workspace:clear-project-memory",
        lua.create_function(|lua, ()| {
            let storage = require_table(lua, "core.storage")?;
            storage.get::<LuaFunction>("clear")?.call::<()>(STORAGE_MODULE)?;
            let core = require_table(lua, "core")?;
            core.get::<LuaFunction>("log")?.call::<()>(
                "Cleared all saved workspace/project memory.",
            )?;
            Ok(())
        })?,
    )?;

    cmds.set(
        "workspace:clear-recents",
        lua.create_function(|lua, ()| {
            let core = require_table(lua, "core")?;
            core.set("recent_projects", lua.create_table()?)?;
            core.set("recent_files", lua.create_table()?)?;
            core.get::<LuaFunction>("log")?.call::<()>(
                "Cleared recent projects and files.",
            )?;
            Ok(())
        })?,
    )?;

    command.call_function::<()>("add", (LuaValue::Nil, cmds))?;

    let register_session_load_hook: LuaFunction = core.get("register_session_load_hook")?;
    let load_hook = lua.create_function(|lua, _: LuaMultiValue| {
        if let Err(e) = load_workspace(lua) {
            log::warn!("failed to load workspace: {e}");
        }
        Ok(())
    })?;
    register_session_load_hook.call::<()>(("workspace", load_hook))?;

    let old_set_project: LuaFunction = core.get("set_project")?;
    let set_project_wrapper = lua.create_function(move |lua, project: LuaValue| {
        if let Err(e) = save_workspace(lua) {
            log::warn!("failed to save workspace on set_project: {e}");
        }
        let result = old_set_project.call::<LuaValue>(project)?;
        if let Err(e) = load_workspace(lua) {
            log::warn!("failed to load workspace on set_project: {e}");
        }
        Ok(result)
    })?;
    core.set("set_project", set_project_wrapper)?;

    let old_exit: LuaFunction = core.get("exit")?;
    let exit_wrapper =
        lua.create_function(move |lua, (quit_fn, force): (LuaFunction, Option<bool>)| {
            if let Err(e) = save_workspace(lua) {
                log::warn!("failed to save workspace on exit: {e}");
            }
            old_exit.call::<()>((quit_fn, force.unwrap_or(false)))
        })?;
    core.set("exit", exit_wrapper)?;

    lua.create_table()
}

pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    let loader = lua.create_function(|lua, ()| install(lua))?;
    preload.set("plugins.workspace", loader)?;
    Ok(())
}
