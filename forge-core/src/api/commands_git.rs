use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Returns the best available path for git operations: active doc path, then project root.
fn active_path(lua: &Lua) -> LuaResult<Option<String>> {
    let core: LuaTable = require_table(lua, "core")?;
    let view: LuaValue = core.get("active_view")?;
    if let LuaValue::Table(v) = &view {
        let doc: Option<LuaTable> = v.get("doc")?;
        if let Some(doc) = doc {
            let abs: Option<String> = doc.get("abs_filename")?;
            if abs.is_some() {
                return Ok(abs);
            }
        }
    }
    let root_project: LuaValue = core.get("root_project")?;
    if let LuaValue::Function(f) = root_project {
        let proj: LuaValue = f.call(())?;
        if let LuaValue::Table(p) = proj {
            return p.get("path");
        }
    }
    Ok(None)
}

/// Shows a success status message or logs an error.
fn show_git_result(
    lua: &Lua,
    ok: bool,
    stderr: &str,
    success: Option<&str>,
) -> LuaResult<()> {
    let core: LuaTable = require_table(lua, "core")?;
    if ok {
        if let Some(msg) = success {
            let style: LuaTable = require_table(lua, "core.style")?;
            let color: LuaValue = style.get("text")?;
            let sv: LuaTable = core.get("status_view")?;
            sv.call_method::<()>("show_message", ("i", color, msg))?;
        }
    } else {
        let msg = if !stderr.is_empty() { stderr.to_owned() } else { "Git command failed".to_owned() };
        core.call_function::<()>("error", msg)?;
    }
    Ok(())
}

/// Calls `fn(item)` with the currently selected git item (from StatusView or active doc).
fn with_selected_file<F>(lua: &Lua, f: F) -> LuaResult<()>
where
    F: FnOnce(&Lua, LuaTable) -> LuaResult<()>,
{
    let core: LuaTable = require_table(lua, "core")?;
    let view: LuaValue = core.get("active_view")?;
    if let LuaValue::Table(v) = &view {
        let ctx: Option<String> = v.get("context")?;
        let has_get_selected: bool = !matches!(v.get::<LuaValue>("get_selected")?, LuaValue::Nil);
        if ctx.as_deref() == Some("session") && has_get_selected {
            let item: LuaValue = v.call_method("get_selected", ())?;
            if let LuaValue::Table(item_tbl) = item {
                return f(lua, item_tbl);
            }
        }
    }
    let path = match active_path(lua)? {
        Some(p) => p,
        None => {
            core.call_function::<()>("error", "No Git-tracked change selected")?;
            return Ok(());
        }
    };
    let git: LuaTable = require_table(lua, "core.git")?;
    let entry: LuaValue = git.call_function("get_file_status", path)?;
    match entry {
        LuaValue::Table(t) => f(lua, t),
        _ => core.call_function::<()>("error", "No Git-tracked change selected"),
    }
}

fn prompt_branch_checkout(lua: &Lua) -> LuaResult<()> {
    let path = active_path(lua)?;
    let git: LuaTable = require_table(lua, "core.git")?;

    let callback = lua.create_function(move |lua, (branches, err): (LuaValue, LuaValue)| {
        let core: LuaTable = require_table(lua, "core")?;
        let branches = match branches {
            LuaValue::Table(t) => t,
            _ => {
                let msg: String = match &err {
                    LuaValue::String(s) => s.to_str()?.to_owned(),
                    _ => "Unable to list branches".to_owned(),
                };
                return core.call_function::<()>("error", msg);
            }
        };
        let branches_key = lua.create_registry_value(branches)?;

        let native_picker = require_table(lua, "picker")?;

        let suggest = {
            let bk = lua.create_registry_value(lua.registry_value::<LuaTable>(&branches_key)?.clone())?;
            let np_key = lua.create_registry_value(native_picker.clone())?;
            lua.create_function(move |lua, text: String| {
                let branches: LuaTable = lua.registry_value(&bk)?;
                let np: LuaTable = lua.registry_value(&np_key)?;
                if text.is_empty() {
                    return Ok(LuaValue::Table(branches));
                }
                np.call_function("rank_strings", (LuaValue::Table(branches), text))
            })?
        };

        let submit = lua.create_function(move |lua, (text, item): (String, LuaValue)| {
            let branch = if let LuaValue::Table(ref t) = item {
                t.get::<String>("text").unwrap_or_else(|_| text.clone())
            } else {
                text.clone()
            };
            if branch.is_empty() { return Ok(()); }
            let git: LuaTable = require_table(lua, "core.git")?;
            let path = active_path(lua)?;
            let msg = format!("Checked out {}", branch);
            let callback = lua.create_function(move |lua, (ok, _stdout, stderr): (bool, String, String)| {
                show_git_result(lua, ok, &stderr, Some(&msg))
            })?;
            git.call_function::<()>("run", (path, lua.create_sequence_from(["checkout", branch.as_str()])?, callback))
        })?;

        let opts = lua.create_table()?;
        opts.set("suggest", suggest)?;
        opts.set("submit", submit)?;
        let command_view: LuaTable = core.get("command_view")?;
        command_view.call_method::<()>("enter", ("Checkout Branch", opts))
    })?;

    git.call_function::<()>("list_branches", (path, callback))
}

fn prompt_branch_create(lua: &Lua) -> LuaResult<()> {
    let core: LuaTable = require_table(lua, "core")?;
    let submit = lua.create_function(|lua, text: String| {
        if text.is_empty() { return Ok(()); }
        let git: LuaTable = require_table(lua, "core.git")?;
        let path = active_path(lua)?;
        let msg = format!("Created branch {}", text);
        let callback = lua.create_function(move |lua, (ok, _stdout, stderr): (bool, String, String)| {
            show_git_result(lua, ok, &stderr, Some(&msg))
        })?;
        git.call_function::<()>("run", (path, lua.create_sequence_from(["checkout", "-b", text.as_str()])?, callback))
    })?;
    let opts = lua.create_table()?;
    opts.set("submit", submit)?;
    let command_view: LuaTable = core.get("command_view")?;
    command_view.call_method::<()>("enter", ("Create Branch", opts))
}

fn prompt_commit(lua: &Lua) -> LuaResult<()> {
    let core: LuaTable = require_table(lua, "core")?;
    let submit = lua.create_function(|lua, text: String| {
        if text.is_empty() { return Ok(()); }
        let git: LuaTable = require_table(lua, "core.git")?;
        let path = active_path(lua)?;
        let callback = lua.create_function(|lua, (ok, _stdout, stderr): (bool, String, String)| {
            show_git_result(lua, ok, &stderr, Some("Committed changes"))
        })?;
        git.call_function::<()>("run", (path, lua.create_sequence_from(["commit", "-m", text.as_str()])?, callback))
    })?;
    let opts = lua.create_table()?;
    opts.set("submit", submit)?;
    let command_view: LuaTable = core.get("command_view")?;
    command_view.call_method::<()>("enter", ("Commit Message", opts))
}

fn prompt_stash(lua: &Lua) -> LuaResult<()> {
    let core: LuaTable = require_table(lua, "core")?;
    let submit = lua.create_function(|lua, text: String| {
        let git: LuaTable = require_table(lua, "core.git")?;
        let path = active_path(lua)?;
        let mut args = vec!["stash".to_owned(), "push".to_owned()];
        if !text.is_empty() {
            args.push("-m".to_owned());
            args.push(text);
        }
        let args_tbl = lua.create_sequence_from(args.iter().map(|s| s.as_str()))?;
        let callback = lua.create_function(|lua, (ok, _stdout, stderr): (bool, String, String)| {
            show_git_result(lua, ok, &stderr, Some("Stashed changes"))
        })?;
        git.call_function::<()>("run", (path, args_tbl, callback))
    })?;
    let opts = lua.create_table()?;
    opts.set("submit", submit)?;
    let command_view: LuaTable = core.get("command_view")?;
    command_view.call_method::<()>("enter", ("Stash Message (optional)", opts))
}

fn register_commands(lua: &Lua) -> LuaResult<()> {
    let command: LuaTable = require_table(lua, "core.command")?;
    let ui: LuaTable = require_table(lua, "core.git.ui")?;

    // Global git commands
    let cmds = lua.create_table()?;

    cmds.set("git:status", lua.create_function(|lua, ()| {
        let ui: LuaTable = require_table(lua, "core.git.ui")?;
        ui.call_function::<()>("open_status", LuaValue::Nil)
    })?)?;

    cmds.set("git:refresh", lua.create_function(|lua, ()| {
        let git: LuaTable = require_table(lua, "core.git")?;
        let path = active_path(lua)?;
        git.call_function::<()>("refresh", (path, true))
    })?)?;

    cmds.set("git:commit", lua.create_function(|lua, ()| prompt_commit(lua))?)?;

    cmds.set("git:pull", lua.create_function(|lua, ()| {
        let git: LuaTable = require_table(lua, "core.git")?;
        let path = active_path(lua)?;
        let callback = lua.create_function(|lua, (ok, _stdout, stderr): (bool, String, String)| {
            show_git_result(lua, ok, &stderr, Some("Pulled latest changes"))
        })?;
        git.call_function::<()>("run", (path, lua.create_sequence_from(["pull", "--ff-only"])?, callback))
    })?)?;

    cmds.set("git:push", lua.create_function(|lua, ()| {
        let git: LuaTable = require_table(lua, "core.git")?;
        let path = active_path(lua)?;
        let callback = lua.create_function(|lua, (ok, _stdout, stderr): (bool, String, String)| {
            show_git_result(lua, ok, &stderr, Some("Pushed changes"))
        })?;
        git.call_function::<()>("run", (path, lua.create_sequence_from(["push"])?, callback))
    })?)?;

    cmds.set("git:checkout", lua.create_function(|lua, ()| prompt_branch_checkout(lua))?)?;
    cmds.set("git:branch", lua.create_function(|lua, ()| prompt_branch_create(lua))?)?;
    cmds.set("git:stash", lua.create_function(|lua, ()| prompt_stash(lua))?)?;

    cmds.set("git:diff-repo", lua.create_function(|lua, ()| {
        let ui: LuaTable = require_table(lua, "core.git.ui")?;
        ui.call_function::<()>("open_repo_diff", (LuaValue::Nil, false))
    })?)?;

    cmds.set("git:diff-repo-staged", lua.create_function(|lua, ()| {
        let ui: LuaTable = require_table(lua, "core.git.ui")?;
        ui.call_function::<()>("open_repo_diff", (LuaValue::Nil, true))
    })?)?;

    cmds.set("git:diff-file", lua.create_function(|lua, ()| {
        with_selected_file(lua, |lua, item| {
            let path: String = item.get("path")?;
            let kind: String = item.get("kind").unwrap_or_default();
            let cached = kind == "staged";
            let ui: LuaTable = require_table(lua, "core.git.ui")?;
            ui.call_function::<()>("open_file_diff", (path, cached))
        })
    })?)?;

    cmds.set("git:stage-file", lua.create_function(|lua, ()| {
        with_selected_file(lua, |lua, item| {
            let path: String = item.get("path")?;
            let rel: String = item.get("rel").unwrap_or_default();
            let git: LuaTable = require_table(lua, "core.git")?;
            let msg = format!("Staged {}", rel);
            let callback = lua.create_function(move |lua, (ok, _stdout, stderr): (bool, String, String)| {
                show_git_result(lua, ok, &stderr, Some(&msg))
            })?;
            git.call_function::<()>("stage", (path, callback))
        })
    })?)?;

    cmds.set("git:unstage-file", lua.create_function(|lua, ()| {
        with_selected_file(lua, |lua, item| {
            let path: String = item.get("path")?;
            let rel: String = item.get("rel").unwrap_or_default();
            let git: LuaTable = require_table(lua, "core.git")?;
            let msg = format!("Unstaged {}", rel);
            let callback = lua.create_function(move |lua, (ok, _stdout, stderr): (bool, String, String)| {
                show_git_result(lua, ok, &stderr, Some(&msg))
            })?;
            git.call_function::<()>("unstage", (path, callback))
        })
    })?)?;

    command.call_function::<()>("add", (LuaValue::Nil, cmds))?;

    // StatusView-scoped commands
    let status_view: LuaTable = ui.get("StatusView")?;
    let sv_cmds = lua.create_table()?;

    sv_cmds.set("git:select-next", lua.create_function(|lua, ()| {
        let core: LuaTable = require_table(lua, "core")?;
        let v: LuaTable = core.get("active_view")?;
        let items: LuaTable = match v.call_method("get_items", ())? {
            LuaValue::Table(t) => t,
            _ => return Ok(()),
        };
        let idx: i64 = v.get("selected_idx").unwrap_or(1);
        let new_idx = idx.min(items.raw_len() as i64 - 1) + 1;
        v.set("selected_idx", new_idx.min(items.raw_len() as i64))?;
        v.call_method::<()>("scroll_to_selected", ())
    })?)?;

    sv_cmds.set("git:select-previous", lua.create_function(|lua, ()| {
        let core: LuaTable = require_table(lua, "core")?;
        let v: LuaTable = core.get("active_view")?;
        let idx: i64 = v.get("selected_idx").unwrap_or(1);
        v.set("selected_idx", (idx - 1).max(1))?;
        v.call_method::<()>("scroll_to_selected", ())
    })?)?;

    sv_cmds.set("git:open-selected", lua.create_function(|lua, ()| {
        let core: LuaTable = require_table(lua, "core")?;
        let v: LuaTable = core.get("active_view")?;
        v.call_method::<()>("open_selected", ())
    })?)?;

    command.call_function::<()>("add", (LuaValue::Table(status_view), sv_cmds))?;

    // Keymap bindings
    let keymap: LuaTable = require_table(lua, "core.keymap")?;
    let bindings = lua.create_table()?;
    bindings.set("ctrl+shift+g", "git:status")?;
    bindings.set("return", "git:open-selected")?;
    bindings.set("up", "git:select-previous")?;
    bindings.set("down", "git:select-next")?;
    keymap.call_function::<()>("add", bindings)?;

    Ok(())
}

fn patch_treeview(lua: &Lua) -> LuaResult<()> {
    let treeview: LuaTable = require_table(lua, "plugins.treeview")?;

    // Guard against double-patching
    let patched: bool = treeview.get("__git_highlighting_patched").unwrap_or(false);
    if patched { return Ok(()); }
    treeview.set("__git_highlighting_patched", true)?;

    let git_val: LuaValue = treeview.get("get_item_text")?;
    let old_fn: LuaFunction = match git_val {
        LuaValue::Function(f) => f,
        _ => return Err(LuaError::runtime(format!(
            "plugins.treeview.get_item_text is {:?}, expected function",
            git_val
        ))),
    };
    let old_key = std::sync::Arc::new(lua.create_registry_value(old_fn)?);

    treeview.set("get_item_text", lua.create_function(move |lua, (this, item, active, hovered): (LuaTable, LuaTable, bool, bool)| {
        let old: LuaFunction = lua.registry_value(&old_key)?;
        let results: LuaMultiValue = old.call((this, item.clone(), active, hovered))?;
        let mut vals = results.into_iter();
        let text = vals.next().unwrap_or(LuaValue::Nil);
        let font = vals.next().unwrap_or(LuaValue::Nil);
        let mut color = vals.next().unwrap_or(LuaValue::Nil);

        // Apply git highlighting if item is a non-active, non-hovered file
        if !active && !hovered {
            let ignored: bool = item.get("ignored").unwrap_or(false);
            let item_type: String = item.get("type").unwrap_or_default();
            let config: LuaTable = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let git_cfg: LuaValue = plugins.get("git")?;
            let highlighting_disabled = if let LuaValue::Table(gc) = &git_cfg {
                gc.get::<LuaValue>("treeview_highlighting")
                    .map(|v| matches!(v, LuaValue::Boolean(false)))
                    .unwrap_or(false)
            } else {
                false
            };

            if !ignored && item_type == "file" && !highlighting_disabled {
                let abs: Option<String> = item.get("abs_filename")?;
                if let Some(abs_path) = abs {
                    let git: LuaTable = require_table(lua, "core.git")?;
                    let entry: LuaValue = git.call_function("get_file_status", abs_path)?;
                    if let LuaValue::Table(e) = entry {
                        let kind: String = e.get("kind").unwrap_or_default();
                        let style: LuaTable = require_table(lua, "core.style")?;
                        color = match kind.as_str() {
                            "staged"    => style.get("accent")?,
                            "untracked" => style.get("good").unwrap_or(color),
                            "conflict"  => style.get("error").unwrap_or(color),
                            _           => style.get("text")?,
                        };
                    }
                }
            }
        }

        Ok(LuaMultiValue::from_vec(vec![text, font, color]))
    })?)?;

    Ok(())
}

fn init(lua: &Lua) -> LuaResult<LuaTable> {
    register_commands(lua)?;
    patch_treeview(lua)?;

    let git: LuaTable = require_table(lua, "core.git")?;
    let ui: LuaTable = require_table(lua, "core.git.ui")?;
    let result = lua.create_table()?;
    result.set("status", git)?;
    result.set("ui", ui)?;
    Ok(result)
}

/// Registers the `core.commands.git` preload (git commands, keymaps, TreeView patch).
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "core.commands.git",
        lua.create_function(|lua, ()| init(lua))?,
    )
}
