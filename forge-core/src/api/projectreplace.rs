use mlua::prelude::*;

use std::sync::Arc;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn lua_f64(v: Option<&LuaValue>) -> f64 {
    match v {
        Some(LuaValue::Number(n)) => *n,
        Some(LuaValue::Integer(n)) => *n as f64,
        _ => 0.0,
    }
}

fn font_get_height(font: &LuaValue) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_height", ()),
        LuaValue::UserData(ud) => ud.call_method("get_height", ()),
        _ => Ok(14.0),
    }
}

fn glob_to_lua_pattern(glob: &str) -> String {
    let mut out = String::from("^");
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            out.push_str(".*");
            i += 2;
        } else if chars[i] == '*' {
            out.push_str("[^/]*");
            i += 1;
        } else if chars[i] == '?' {
            out.push('.');
            i += 1;
        } else {
            let ch = chars[i];
            if "\\+^$().|[]{}".contains(ch) {
                out.push('\\');
            }
            out.push(ch);
            i += 1;
        }
    }
    out.push('$');
    out
}

fn path_matches_glob_rs(
    lua: &Lua,
    filename: &str,
    pattern: &str,
    projects: &LuaTable,
    common: &LuaTable,
) -> LuaResult<bool> {
    let normalized = filename.replace('\\', "/");
    for pair in projects.sequence_values::<LuaTable>() {
        let project = pair?;
        let project_path: String = project.get("path")?;
        let belongs: bool =
            common.call_function("path_belongs_to", (filename, project_path.as_str()))?;
        if belongs {
            let rel: String =
                common.call_function("relative_path", (project_path, filename))?;
            let rel_normalized = rel.replace('\\', "/");
            let re: LuaTable = lua.globals().get("regex")?;
            let compiled: LuaValue = re.call_function("compile", pattern)?;
            if !matches!(compiled, LuaValue::Nil) {
                let m: LuaValue = re.call_function("cmatch", (compiled, rel_normalized))?;
                return Ok(!matches!(m, LuaValue::Nil));
            }
            return Ok(false);
        }
    }
    let re: LuaTable = lua.globals().get("regex")?;
    let compiled: LuaValue = re.call_function("compile", pattern)?;
    if !matches!(compiled, LuaValue::Nil) {
        let m: LuaValue = re.call_function("cmatch", (compiled, normalized))?;
        return Ok(!matches!(m, LuaValue::Nil));
    }
    Ok(false)
}

fn collect_native_files(
    lua: &Lua,
    path: &LuaValue,
    path_glob: &LuaValue,
    config: &LuaTable,
) -> LuaResult<LuaTable> {
    let native_model = require_table(lua, "project_model")?;
    let core = require_table(lua, "core")?;
    let common = require_table(lua, "core.common")?;

    if let LuaValue::String(p) = path {
        let system: LuaTable = lua.globals().get("system")?;
        let info: Option<LuaTable> = system.call_function("get_file_info", p.to_str()?)?;
        if let Some(ref info) = info {
            let ftype: String = info.get("type")?;
            if ftype == "file" {
                let result = lua.create_table()?;
                result.push(p.to_str()?)?;
                return Ok(result);
            }
        }
    }

    let roots = lua.create_table()?;
    if let LuaValue::String(p) = path {
        let system: LuaTable = lua.globals().get("system")?;
        let info: Option<LuaTable> = system.call_function("get_file_info", p.to_str()?)?;
        if let Some(ref info) = info {
            let ftype: String = info.get("type")?;
            if ftype == "dir" {
                roots.push(p.to_str()?)?;
            }
        }
    }
    if roots.raw_len() == 0 {
        let projects: LuaTable = core.get("projects")?;
        for pair in projects.sequence_values::<LuaTable>() {
            let project = pair?;
            let ppath: String = project.get("path")?;
            roots.push(ppath)?;
        }
    }

    let opts = lua.create_table()?;
    let fsl: f64 = config.get("file_size_limit")?;
    opts.set("max_size_bytes", fsl * 1e6)?;
    let ps: LuaTable = config.get("project_scan")?;
    opts.set("max_files", ps.get::<LuaValue>("max_files")?)?;
    opts.set("exclude_dirs", ps.get::<LuaValue>("exclude_dirs")?)?;

    let files: LuaTable = native_model.call_function("get_all_files", (roots, opts))?;

    // Filter by glob
    if let LuaValue::String(g) = path_glob {
        let gs = g.to_str()?;
        if !gs.is_empty() {
            let pattern = glob_to_lua_pattern(&gs.replace('\\', "/"));
            let projects: LuaTable = core.get("projects")?;
            let filtered = lua.create_table()?;
            let mut count = 0i64;
            for pair in files.sequence_values::<String>() {
                let filename = pair?;
                if path_matches_glob_rs(lua, &filename, &pattern, &projects, &common)? {
                    count += 1;
                    filtered.set(count, filename)?;
                }
            }
            return Ok(filtered);
        }
    }

    // Filter by path prefix
    if let LuaValue::String(p) = path {
        let ps = p.to_str()?.to_owned();
        let system: LuaTable = lua.globals().get("system")?;
        let info: Option<LuaTable> = system.call_function("get_file_info", ps.as_str())?;
        let is_dir = info.map(|i| i.get::<String>("type").ok() == Some("dir".to_string())).unwrap_or(false);
        if !is_dir {
            let filtered = lua.create_table()?;
            let mut count = 0i64;
            for pair in files.sequence_values::<String>() {
                let filename = pair?;
                if filename.starts_with(&ps) {
                    count += 1;
                    filtered.set(count, filename)?;
                }
            }
            return Ok(filtered);
        }
    }

    Ok(files)
}

fn build_replace_view(lua: &Lua) -> LuaResult<(LuaTable, Arc<LuaRegistryKey>)> {
    let view_class = require_table(lua, "core.view")?;
    let replace_view = view_class.call_method::<LuaTable>("extend", ())?;

    replace_view.set(
        "__tostring",
        lua.create_function(|_, _: LuaTable| Ok("ReplaceView"))?,
    )?;
    replace_view.set("context", "session")?;

    let class_key = Arc::new(lua.create_registry_value(replace_view.clone())?);

    // new
    {
        let ck = Arc::clone(&class_key);
        replace_view.set(
            "new",
            lua.create_function(
                move |lua,
                      (this, path, search, replace, fn_find, fn_apply, path_glob,
                       native_search_opts, native_replace_opts): (
                    LuaTable, LuaValue, String, String, LuaFunction, LuaFunction,
                    LuaValue, LuaTable, LuaTable,
                )| {
                    let class: LuaTable = lua.registry_value(&ck)?;
                    let super_tbl: LuaTable = class.get("super")?;
                    let super_new: LuaFunction = super_tbl.get("new")?;
                    super_new.call::<()>(this.clone())?;
                    this.set("scrollable", true)?;
                    this.set("max_h_scroll", 0.0)?;
                    this.set("path", path)?;
                    this.set("search", search)?;
                    this.set("replace", replace)?;
                    this.set("path_glob", path_glob)?;
                    this.set("fn_find", fn_find)?;
                    this.set("fn_apply", fn_apply)?;
                    this.set("results", lua.create_table()?)?;
                    this.set("phase", "scanning")?;
                    this.set("last_file_idx", 1)?;
                    this.set("selected_idx", 0)?;
                    this.set("brightness", 0.0)?;
                    this.set("replaced_count", 0)?;
                    this.set("replaced_files", 0)?;
                    this.set("operation", "replace")?;
                    this.set("native_search_opts", native_search_opts)?;
                    this.set("native_replace_opts", native_replace_opts)?;
                    let begin_scan: LuaFunction = this.get("begin_scan")?;
                    begin_scan.call::<()>(this.clone())?;
                    Ok(())
                },
            )?,
        )?;
    }

    replace_view.set(
        "get_name",
        lua.create_function(|_, _this: LuaTable| Ok("Replace Results"))?,
    )?;

    // begin_scan
    replace_view.set(
        "begin_scan",
        lua.create_function(|lua, this: LuaTable| {
            this.set("results", lua.create_table()?)?;
            this.set("phase", "scanning")?;
            this.set("last_file_idx", 1)?;

            let native_search = require_table(lua, "project_search")?;
            let core = require_table(lua, "core")?;
            let search_opts: LuaTable = this.get("native_search_opts")?;
            let handle: LuaValue = native_search.call_function("search", search_opts.clone())?;
            let files: LuaValue = search_opts.get("files")?;
            let file_count = match &files {
                LuaValue::Table(t) => t.raw_len() as i64,
                _ => 0,
            };
            this.set("last_file_idx", file_count)?;

            let this_key = lua.create_registry_value(this.clone())?;
            let handle_key = lua.create_registry_value(handle)?;
            let tick = lua.create_function(move |lua, ()| -> LuaResult<bool> {
                let this: LuaTable = lua.registry_value(&this_key)?;
                let handle: LuaValue = lua.registry_value(&handle_key)?;
                let native_search = require_table(lua, "project_search")?;
                let core = require_table(lua, "core")?;

                let polled: Option<LuaTable> =
                    native_search.call_function("poll", (handle.clone(), 128))?;
                if let Some(ref polled) = polled {
                    let error: LuaValue = polled.get("error")?;
                    if let LuaValue::String(ref e) = error {
                        core.call_function::<()>("error", e.to_str()?)?;
                        this.set("phase", "confirming")?;
                        return Ok(true);
                    }
                    let poll_results: LuaValue = polled.get("results")?;
                    if let LuaValue::Table(ref pr) = poll_results {
                        let results: LuaTable = this.get("results")?;
                        let mut count = results.raw_len() as i64;
                        for pair in pr.sequence_values::<LuaTable>() {
                            let item = pair?;
                            count += 1;
                            let entry = lua.create_table()?;
                            entry.set("file", item.get::<LuaValue>("file")?)?;
                            entry.set("text", item.get::<LuaValue>("text")?)?;
                            entry.set("line", item.get::<LuaValue>("line")?)?;
                            entry.set("col", item.get::<LuaValue>("col")?)?;
                            results.set(count, entry)?;
                        }
                        core.set("redraw", true)?;
                    }
                    let done: bool = polled.get("done").unwrap_or(false);
                    if done {
                        this.set("phase", "confirming")?;
                        this.set("brightness", 100.0)?;
                        core.set("redraw", true)?;
                        return Ok(true);
                    }
                }
                Ok(false)
            })?;

            let thread_fn: LuaFunction = lua.load(
                "local t = ...; return function() while true do if t() then return end; coroutine.yield(0.01) end end"
            ).call(tick)?;

            let results: LuaTable = this.get("results")?;
            core.call_function::<()>("add_thread", (thread_fn, results))?;
            let scroll: LuaTable = this.get("scroll")?;
            let to: LuaTable = scroll.get("to")?;
            to.set("y", 0.0)?;
            Ok(())
        })?,
    )?;

    // apply_replace
    replace_view.set(
        "apply_replace",
        lua.create_function(|lua, this: LuaTable| {
            this.set("phase", "replacing")?;
            this.set("replaced_count", 0)?;
            this.set("replaced_files", 0)?;
            let core = require_table(lua, "core")?;
            core.set("redraw", true)?;

            let native_search = require_table(lua, "project_search")?;
            let replace_opts: LuaTable = this.get("native_replace_opts")?;
            let handle: LuaValue = native_search.call_function("replace_async", replace_opts)?;

            let this_key = lua.create_registry_value(this)?;
            let handle_key = lua.create_registry_value(handle)?;
            let tick = lua.create_function(move |lua, ()| -> LuaResult<bool> {
                let this: LuaTable = lua.registry_value(&this_key)?;
                let handle: LuaValue = lua.registry_value(&handle_key)?;
                let native_search = require_table(lua, "project_search")?;
                let core = require_table(lua, "core")?;

                let polled: Option<LuaTable> =
                    native_search.call_function("replace_poll", handle.clone())?;
                if let Some(ref polled) = polled {
                    let error: LuaValue = polled.get("error")?;
                    if let LuaValue::String(ref e) = error {
                        core.call_function::<()>("error", e.to_str()?)?;
                        this.set("phase", "done")?;
                        this.set("brightness", 100.0)?;
                        core.set("redraw", true)?;
                        return Ok(true);
                    }
                    let done: bool = polled.get("done").unwrap_or(false);
                    if done {
                        let rc: i64 = polled.get("replaced_count").unwrap_or(0);
                        let rf: i64 = polled.get("replaced_files").unwrap_or(0);
                        this.set("replaced_count", rc)?;
                        this.set("replaced_files", rf)?;
                        this.set("phase", "done")?;
                        this.set("brightness", 100.0)?;
                        core.set("redraw", true)?;
                        return Ok(true);
                    }
                }
                Ok(false)
            })?;

            let thread_fn: LuaFunction = lua.load(
                "local t = ...; return function() while true do if t() then return end; coroutine.yield(0.01) end end"
            ).call(tick)?;

            core.call_function::<()>("add_thread", thread_fn)?;
            Ok(())
        })?,
    )?;

    // on_mouse_moved
    replace_view.set(
        "on_mouse_moved",
        lua.create_function(|_lua, (this, mx, my, rest): (LuaTable, f64, f64, LuaMultiValue)| {
            let super_cls: LuaTable = this.get("super")?;
            let super_omm: LuaFunction = super_cls.get("on_mouse_moved")?;
            let mut args = LuaMultiValue::new();
            args.push_back(LuaValue::Table(this.clone()));
            args.push_back(LuaValue::Number(mx));
            args.push_back(LuaValue::Number(my));
            args.extend(rest);
            super_omm.call::<()>(args)?;
            this.set("selected_idx", 0)?;
            let iter: LuaFunction = this.call_method("each_visible_result", ())?;
            loop {
                let r: LuaMultiValue = iter.call(())?;
                let vals: Vec<LuaValue> = r.into_vec();
                if vals.is_empty() || matches!(vals[0], LuaValue::Nil) {
                    break;
                }
                let rx = lua_f64(vals.get(2));
                let ry = lua_f64(vals.get(3));
                let rw = lua_f64(vals.get(4));
                let rh = lua_f64(vals.get(5));
                if mx >= rx && my >= ry && mx < rx + rw && my < ry + rh {
                    if let Some(LuaValue::Integer(i)) = vals.first() {
                        this.set("selected_idx", *i)?;
                    } else if let Some(LuaValue::Number(n)) = vals.first() {
                        this.set("selected_idx", *n as i64)?;
                    }
                    break;
                }
            }
            Ok(())
        })?,
    )?;

    // on_mouse_pressed
    {
        let ck = Arc::clone(&class_key);
        replace_view.set(
            "on_mouse_pressed",
            lua.create_function(move |lua, (this, rest): (LuaTable, LuaMultiValue)| {
                let class: LuaTable = lua.registry_value(&ck)?;
                let super_tbl: LuaTable = class.get("super")?;
                let super_omp: LuaFunction = super_tbl.get("on_mouse_pressed")?;
                let mut args = LuaMultiValue::new();
                args.push_back(LuaValue::Table(this.clone()));
                args.extend(rest);
                let caught: bool = super_omp.call(args).unwrap_or(false);
                if !caught {
                    let open: LuaFunction = this.get("open_selected_result")?;
                    return open.call(this);
                }
                Ok(LuaValue::Nil)
            })?,
        )?;
    }

    // open_selected_result
    replace_view.set(
        "open_selected_result",
        lua.create_function(|lua, this: LuaTable| {
            let selected_idx: i64 = this.get("selected_idx")?;
            let results: LuaTable = this.get("results")?;
            let res: LuaValue = results.get(selected_idx)?;
            if matches!(res, LuaValue::Nil) {
                return Ok(LuaValue::Nil);
            }
            let res = res.as_table().unwrap();
            let core = require_table(lua, "core")?;
            let file: String = res.get("file")?;
            let line: i64 = res.get("line")?;
            let col: i64 = res.get("col")?;
            let try_fn = lua.create_function(move |lua, ()| {
                let core = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let doc: LuaTable = core.call_function("open_doc", file.as_str())?;
                let dv: LuaTable = root_view.call_method("open_doc", doc)?;
                let root_node: LuaTable = root_view.get("root_node")?;
                root_node.call_method::<()>("update_layout", ())?;
                let dv_doc: LuaTable = dv.get("doc")?;
                dv_doc.call_method::<()>("set_selection", (line, col))?;
                dv.call_method::<()>("scroll_to_line", (line, false, true))?;
                Ok(())
            })?;
            core.call_function::<()>("try", try_fn)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )?;

    // update
    {
        let ck = Arc::clone(&class_key);
        replace_view.set(
            "update",
            lua.create_function(move |lua, this: LuaTable| {
                this.call_method::<()>("move_towards", ("brightness", 0.0, 0.1))?;
                let class: LuaTable = lua.registry_value(&ck)?;
                let super_tbl: LuaTable = class.get("super")?;
                let super_update: LuaFunction = super_tbl.get("update")?;
                super_update.call::<()>(this)
            })?,
        )?;
    }

    // get_results_yoffset
    replace_view.set(
        "get_results_yoffset",
        lua.create_function(|lua, _this: LuaTable| {
            let style = require_table(lua, "core.style")?;
            let font: LuaValue = style.get("font")?;
            let fh: f64 = font_get_height(&font)?;
            let padding: LuaTable = style.get("padding")?;
            let py: f64 = padding.get("y")?;
            Ok(fh + py * 3.0)
        })?,
    )?;

    // get_line_height
    replace_view.set(
        "get_line_height",
        lua.create_function(|lua, _this: LuaTable| {
            let style = require_table(lua, "core.style")?;
            let font: LuaValue = style.get("font")?;
            let fh: f64 = font_get_height(&font)?;
            let padding: LuaTable = style.get("padding")?;
            let py: f64 = padding.get("y")?;
            Ok(py + fh)
        })?,
    )?;

    // get_scrollable_size
    replace_view.set(
        "get_scrollable_size",
        lua.create_function(|_, this: LuaTable| {
            let yoffset: f64 = this.call_method("get_results_yoffset", ())?;
            let results: LuaTable = this.get("results")?;
            let count = results.raw_len() as f64;
            let lh: f64 = this.call_method("get_line_height", ())?;
            Ok(yoffset + count * lh)
        })?,
    )?;

    // get_h_scrollable_size
    replace_view.set(
        "get_h_scrollable_size",
        lua.create_function(|_, this: LuaTable| {
            let v: f64 = this.get("max_h_scroll")?;
            Ok(v)
        })?,
    )?;

    // get_visible_results_range
    replace_view.set(
        "get_visible_results_range",
        lua.create_function(|lua, this: LuaTable| {
            let lh: f64 = this.call_method("get_line_height", ())?;
            let oy: f64 = this.call_method("get_results_yoffset", ())?;
            let style = require_table(lua, "core.style")?;
            let font: LuaValue = style.get("font")?;
            let fh: f64 = font_get_height(&font)?;
            let scroll: LuaTable = this.get("scroll")?;
            let scroll_y: f64 = scroll.get("y")?;
            let min = 1i64.max(((scroll_y + oy - fh) / lh).floor() as i64);
            let size: LuaTable = this.get("size")?;
            let size_y: f64 = size.get("y")?;
            let max = min + (size_y / lh).floor() as i64 + 1;
            Ok((min, max))
        })?,
    )?;

    // each_visible_result - stateful iterator, no coroutine.yield
    replace_view.set(
        "each_visible_result",
        lua.create_function(|lua, this: LuaTable| {
            let lh: f64 = this.call_method("get_line_height", ())?;
            let (cx, cy): (f64, f64) = this.call_method("get_content_offset", ())?;
            let (min, max): (i64, i64) = this.call_method("get_visible_results_range", ())?;
            let oy: f64 = this.call_method("get_results_yoffset", ())?;
            let start_y = cy + oy + lh * (min - 1) as f64;
            let results: LuaTable = this.get("results")?;

            let entries = lua.create_table()?;
            let mut count = 0i64;
            let mut y = start_y;
            for i in min..=max {
                let item: LuaValue = results.get(i)?;
                if matches!(item, LuaValue::Nil) {
                    break;
                }
                let (_, _, bw): (f64, f64, f64) = this.call_method("get_content_bounds", ())?;
                count += 1;
                let entry = lua.create_table()?;
                entry.set(1, i)?;
                entry.set(2, item)?;
                entry.set(3, cx)?;
                entry.set(4, y)?;
                entry.set(5, bw)?;
                entry.set(6, lh)?;
                entries.set(count, entry)?;
                y += lh;
            }

            let state = lua.create_table()?;
            state.set("idx", 0i64)?;
            state.set("len", count)?;
            let entries_key = lua.create_registry_value(entries)?;

            let iterator = lua.create_function(move |lua, ()| -> LuaResult<LuaMultiValue> {
                let idx: i64 = state.get("idx")?;
                let len: i64 = state.get("len")?;
                let next = idx + 1;
                if next > len {
                    return Ok(LuaMultiValue::new());
                }
                state.set("idx", next)?;
                let entries: LuaTable = lua.registry_value(&entries_key)?;
                let entry: LuaTable = entries.get(next)?;
                Ok(LuaMultiValue::from_vec(vec![
                    entry.get(1)?,
                    entry.get(2)?,
                    entry.get(3)?,
                    entry.get(4)?,
                    entry.get(5)?,
                    entry.get(6)?,
                ]))
            })?;
            Ok(iterator)
        })?,
    )?;

    // scroll_to_make_selected_visible
    replace_view.set(
        "scroll_to_make_selected_visible",
        lua.create_function(|_, this: LuaTable| {
            let h: f64 = this.call_method("get_line_height", ())?;
            let idx: i64 = this.get("selected_idx")?;
            let y = h * (idx - 1) as f64;
            let scroll: LuaTable = this.get("scroll")?;
            let to: LuaTable = scroll.get("to")?;
            let cur_y: f64 = to.get("y")?;
            let size: LuaTable = this.get("size")?;
            let size_y: f64 = size.get("y")?;
            let yoffset: f64 = this.call_method("get_results_yoffset", ())?;
            to.set("y", cur_y.min(y))?;
            let new_y: f64 = to.get("y")?;
            to.set("y", new_y.max(y + h - size_y + yoffset))?;
            Ok(())
        })?,
    )?;

    // draw
    replace_view.set(
        "draw",
        lua.create_function(|lua, this: LuaTable| {
            let style = require_table(lua, "core.style")?;
            let common = require_table(lua, "core.common")?;
            let core = require_table(lua, "core")?;
            let renderer: LuaTable = lua.globals().get("renderer")?;

            let bg: LuaValue = style.get("background")?;
            this.call_method::<()>("draw_background", bg.clone())?;

            let position: LuaTable = this.get("position")?;
            let ox: f64 = position.get("x")?;
            let oy: f64 = position.get("y")?;
            let size: LuaTable = this.get("size")?;
            let size_x: f64 = size.get("x")?;
            let yoffset: f64 = this.call_method("get_results_yoffset", ())?;

            renderer.call_function::<()>("draw_rect", (ox, oy, size_x, yoffset, bg))?;
            let scroll: LuaTable = this.get("scroll")?;
            let scroll_y: f64 = scroll.get("y")?;
            if scroll_y != 0.0 {
                let ds: LuaValue = style.get("divider_size")?;
                let divider: LuaValue = style.get("divider")?;
                renderer.call_function::<()>(
                    "draw_rect",
                    (ox, oy + yoffset, size_x, ds, divider),
                )?;
            }

            let brightness: f64 = this.get("brightness")?;
            let color: LuaValue = common.call_function(
                "lerp",
                (
                    style.get::<LuaValue>("text")?,
                    style.get::<LuaValue>("accent")?,
                    brightness / 100.0,
                ),
            )?;
            let padding: LuaTable = style.get("padding")?;
            let px: f64 = padding.get("x")?;
            let py: f64 = padding.get("y")?;
            let x = ox + px;
            let y = oy + py;

            let phase: String = this.get("phase")?;
            let operation: String = this.get("operation")?;
            let search: String = this.get("search")?;
            let replace: String = this.get("replace")?;
            let results: LuaTable = this.get("results")?;
            let result_count = results.raw_len();
            let last_file_idx: i64 = this.get("last_file_idx")?;
            let replaced_count: i64 = this.get("replaced_count")?;
            let replaced_files: i64 = this.get("replaced_files")?;
            let path_glob: LuaValue = this.get("path_glob")?;
            let glob_suffix = match &path_glob {
                LuaValue::String(s) => {
                    let s = s.to_str()?;
                    if s.is_empty() { String::new() } else { format!(" in {s}") }
                }
                _ => String::new(),
            };

            let is_swap = operation == "swap";
            let msg = match phase.as_str() {
                "scanning" => {
                    if is_swap {
                        format!("Scanning ({last_file_idx} files, {result_count} matches) to swap {search:?} and {replace:?}{glob_suffix}...")
                    } else {
                        format!("Searching ({last_file_idx} files, {result_count} matches) for {search:?}{glob_suffix}...")
                    }
                }
                "confirming" => {
                    if is_swap {
                        format!("Found {result_count} matches to swap {search:?} and {replace:?}{glob_suffix} -- press F5 to apply")
                    } else {
                        format!("Found {result_count} matches for {search:?}{glob_suffix} -- press F5 to replace all with {replace:?}")
                    }
                }
                "replacing" => {
                    if is_swap {
                        format!("Swapping... ({replaced_files} files written)")
                    } else {
                        format!("Replacing... ({replaced_files} files written)")
                    }
                }
                _ => {
                    if is_swap {
                        format!("Done -- swapped {replaced_count} occurrences in {replaced_files} files ({search:?} <-> {replace:?})")
                    } else {
                        format!("Done -- replaced {replaced_count} occurrences in {replaced_files} files ({search:?} -> {replace:?})")
                    }
                }
            };
            let font: LuaValue = style.get("font")?;
            renderer.call_function::<()>("draw_text", (font, msg, x, y, color))?;

            let dcolor: LuaValue = common.call_function(
                "lerp",
                (
                    style.get::<LuaValue>("dim")?,
                    style.get::<LuaValue>("text")?,
                    brightness / 100.0,
                ),
            )?;
            let ds: f64 = style.get("divider_size")?;
            renderer.call_function::<()>(
                "draw_rect",
                (x, oy + yoffset - py, size_x - px * 2.0, ds, dcolor),
            )?;

            let (_, _, bw): (f64, f64, f64) = this.call_method("get_content_bounds", ())?;
            let size_y: f64 = size.get("y")?;
            core.call_function::<()>(
                "push_clip_rect",
                (ox, oy + yoffset + ds, bw, size_y - yoffset),
            )?;

            let selected_idx: i64 = this.get("selected_idx")?;
            let style_accent: LuaValue = style.get("accent")?;
            let style_dim: LuaValue = style.get("dim")?;
            let style_text: LuaValue = style.get("text")?;
            let style_font: LuaValue = style.get("font")?;
            let style_code_font: LuaValue = style.get("code_font")?;
            let line_hl: LuaValue = style.get("line_highlight")?;
            let root_project: LuaTable = core.call_function("root_project", ())?;
            let mut max_h_scroll: f64 = this.get("max_h_scroll")?;

            let iter: LuaFunction = this.call_method("each_visible_result", ())?;
            loop {
                let r: LuaMultiValue = iter.call(())?;
                let vals: Vec<LuaValue> = r.into_vec();
                if vals.is_empty() || matches!(vals[0], LuaValue::Nil) {
                    break;
                }
                let i = match &vals[0] {
                    LuaValue::Integer(n) => *n,
                    LuaValue::Number(n) => *n as i64,
                    _ => break,
                };
                let item = match &vals[1] {
                    LuaValue::Table(t) => t,
                    _ => break,
                };
                let ix = lua_f64(vals.get(2));
                let iy = lua_f64(vals.get(3));
                let iw = lua_f64(vals.get(4));
                let ih = lua_f64(vals.get(5));

                let tc = if i == selected_idx {
                    renderer.call_function::<()>(
                        "draw_rect",
                        (ix, iy, iw, ih, line_hl.clone()),
                    )?;
                    style_accent.clone()
                } else {
                    style_text.clone()
                };
                let draw_x = ix + px;
                let file: String = item.get("file")?;
                let line: i64 = item.get("line")?;
                let col: i64 = item.get("col")?;
                let label: String = root_project.call_method("normalize_path", file)?;
                let prefix = format!("{label} at line {line} (col {col}): ");
                let end_x: f64 = common.call_function(
                    "draw_text",
                    (
                        style_font.clone(),
                        style_dim.clone(),
                        prefix,
                        "left",
                        draw_x,
                        iy,
                        iw,
                        ih,
                    ),
                )?;
                let match_text: String = item.get("text")?;
                let end_x2: f64 = common.call_function(
                    "draw_text",
                    (
                        style_code_font.clone(),
                        tc,
                        match_text,
                        "left",
                        end_x,
                        iy,
                        iw,
                        ih,
                    ),
                )?;
                max_h_scroll = max_h_scroll.max(end_x2);
            }
            this.set("max_h_scroll", max_h_scroll)?;

            core.call_function::<()>("pop_clip_rect", ())?;
            this.call_method::<()>("draw_scrollbar", ())?;
            Ok(())
        })?,
    )?;

    Ok((replace_view, class_key))
}

fn get_selected_text(lua: &Lua) -> LuaResult<LuaValue> {
    let core = require_table(lua, "core")?;
    let view: LuaTable = core.get("active_view")?;
    let doc: LuaValue = view.get("doc")?;
    if let LuaValue::Table(ref d) = doc {
        let sel: LuaMultiValue = d.call_method("get_selection", ())?;
        let text: LuaValue = d.call_method("get_text", sel)?;
        return Ok(text);
    }
    Ok(LuaValue::Nil)
}

/// Registers `plugins.projectreplace`: project-wide find and replace with preview.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.projectreplace",
        lua.create_function(|lua, ()| {
            // Config defaults
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let common = require_table(lua, "core.common")?;
            let defaults = lua.create_table()?;
            defaults.set("backup_originals", true)?;
            let merged: LuaTable = common.call_function(
                "merge",
                (defaults, plugins.get::<LuaValue>("projectreplace")?),
            )?;
            plugins.set("projectreplace", merged)?;

            let (replace_view, class_key) = build_replace_view(lua)?;

            // Helper: open_replace_view
            let ck = Arc::clone(&class_key);
            let open_replace_view = lua.create_function(
                move |lua,
                      (path, search, replace, fn_find, fn_apply, path_glob, operation,
                       native_search_opts, native_replace_opts): (
                    LuaValue, String, String, LuaFunction, LuaFunction, LuaValue,
                    Option<String>, LuaTable, LuaTable,
                )| {
                    if search.is_empty() {
                        let core = require_table(lua, "core")?;
                        core.call_function::<()>("error", "Expected non-empty search string")?;
                        return Ok(LuaValue::Nil);
                    }
                    let rv_class: LuaTable = lua.registry_value(&ck)?;
                    let rv: LuaTable = rv_class.call((
                        path,
                        search,
                        replace,
                        fn_find,
                        fn_apply,
                        path_glob,
                        native_search_opts,
                        native_replace_opts,
                    ))?;
                    rv.set("operation", operation.unwrap_or_else(|| "replace".to_string()))?;
                    let core = require_table(lua, "core")?;
                    let root_view: LuaTable = core.get("root_view")?;
                    let node: LuaTable = root_view.call_method("get_active_node_default", ())?;
                    node.call_method::<()>("add_view", rv.clone())?;
                    Ok(LuaValue::Table(rv))
                },
            )?;
            let orv_key = Arc::new(lua.create_registry_value(open_replace_view)?);

            // Helper: prompt_path_glob
            let prompt_path_glob = lua.create_function(|lua, submit: LuaFunction| {
                let core = require_table(lua, "core")?;
                let cv: LuaTable = core.get("command_view")?;
                let opts = lua.create_table()?;
                opts.set(
                    "submit",
                    lua.create_function(move |lua, text: String| {
                        let val = if text.is_empty() {
                            LuaValue::Nil
                        } else {
                            LuaValue::String(lua.create_string(&text)?)
                        };
                        submit.call::<()>(val)
                    })?,
                )?;
                cv.call_method::<()>("enter", ("Path Glob Filter (optional)", opts))
            })?;
            let ppg_key = Arc::new(lua.create_registry_value(prompt_path_glob)?);

            // Helper: prompt_yes_no
            let prompt_yes_no = lua.create_function(
                |lua, (label, default, submit): (String, bool, LuaFunction)| {
                    let core = require_table(lua, "core")?;
                    let cv: LuaTable = core.get("command_view")?;
                    let opts = lua.create_table()?;
                    opts.set("text", "")?;
                    opts.set(
                        "validate",
                        lua.create_function(move |_, text: String| {
                            let t = text.trim().to_lowercase();
                            if t.is_empty() {
                                return Ok(true);
                            }
                            Ok(matches!(
                                t.as_str(),
                                "y" | "yes" | "true" | "1" | "n" | "no" | "false" | "0"
                            ))
                        })?,
                    )?;
                    opts.set(
                        "submit",
                        lua.create_function(move |_, text: String| {
                            let t = text.trim().to_lowercase();
                            let val = if t.is_empty() {
                                default
                            } else {
                                matches!(t.as_str(), "y" | "yes" | "true" | "1")
                            };
                            submit.call::<()>(val)
                        })?,
                    )?;
                    cv.call_method::<()>("enter", (label, opts))
                },
            )?;
            let pyn_key = Arc::new(lua.create_registry_value(prompt_yes_no)?);

            // Commands
            let command = require_table(lua, "core.command")?;

            // Global commands
            let cmds = lua.create_table()?;

            // project-search:replace
            let orv_k = Arc::clone(&orv_key);
            let ppg_k = Arc::clone(&ppg_key);
            cmds.set(
                "project-search:replace",
                lua.create_function(move |lua, path: LuaValue| {
                    let core = require_table(lua, "core")?;
                    let cv: LuaTable = core.get("command_view")?;
                    let selected = get_selected_text(lua)?;
                    let path_str = match &path {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => "Project".to_string(),
                    };
                    let orv_k2 = Arc::clone(&orv_k);
                    let ppg_k2 = Arc::clone(&ppg_k);
                    let path_key = lua.create_registry_value(path)?;
                    let opts = lua.create_table()?;
                    if let LuaValue::String(ref s) = selected {
                        opts.set("text", s.to_str()?)?;
                    }
                    opts.set("select_text", true)?;
                    opts.set(
                        "submit",
                        lua.create_function(move |lua, search: String| {
                            let ppg: LuaFunction = lua.registry_value(&ppg_k2)?;
                            let orv_k3 = Arc::clone(&orv_k2);
                            let pk = lua.create_registry_value(lua.registry_value::<LuaValue>(&path_key)?)?;

                            let glob_submit = lua.create_function(move |lua, path_glob: LuaValue| {
                                let core2 = require_table(lua, "core")?;
                                let cv2: LuaTable = core2.get("command_view")?;
                                let orv_k4 = Arc::clone(&orv_k3);
                                let pk2 = lua.create_registry_value(lua.registry_value::<LuaValue>(&pk)?)?;
                                let pg_key = lua.create_registry_value(path_glob)?;
                                let search_str = search.clone();
                                let replace_opts = lua.create_table()?;
                                replace_opts.set(
                                    "submit",
                                    lua.create_function(move |lua, replace_text: String| {
                                        let orv: LuaFunction = lua.registry_value(&orv_k4)?;
                                        let path: LuaValue = lua.registry_value(&pk2)?;
                                        let pg: LuaValue = lua.registry_value(&pg_key)?;
                                        let config = require_table(lua, "core.config")?;
                                        let plugins: LuaTable = config.get("plugins")?;
                                        let pr_cfg: LuaTable = plugins.get("projectreplace")?;
                                        let backup: bool = pr_cfg.get("backup_originals").unwrap_or(true);
                                        let q = search_str.clone();

                                        let files = collect_native_files(lua, &path, &pg, &config)?;
                                        let search_opts = lua.create_table()?;
                                        search_opts.set("files", files.clone())?;
                                        search_opts.set("query", q.as_str())?;
                                        search_opts.set("mode", "plain")?;
                                        search_opts.set("no_case", false)?;

                                        let replace_native = lua.create_table()?;
                                        replace_native.set("files", files)?;
                                        replace_native.set("mode", "plain")?;
                                        replace_native.set("query", q.as_str())?;
                                        replace_native.set("replace", replace_text.as_str())?;
                                        replace_native.set("no_case", false)?;
                                        replace_native.set("backup_originals", backup)?;

                                        let fn_find = lua.create_function(|_, _: String| Ok(LuaValue::Nil))?;
                                        let fn_apply = lua.create_function(|_, _: String| Ok(LuaValue::Nil))?;
                                        orv.call::<()>((
                                            path, q, replace_text, fn_find, fn_apply,
                                            pg, Some("replace"), search_opts, replace_native,
                                        ))
                                    })?,
                                )?;
                                cv2.call_method::<()>("enter", ("Replace With", replace_opts))
                            })?;
                            ppg.call::<()>(glob_submit)
                        })?,
                    )?;
                    cv.call_method::<()>(
                        "enter",
                        (format!("Replace Text In {path_str}"), opts),
                    )
                })?,
            )?;

            // project-search:replace-regex
            let orv_k = Arc::clone(&orv_key);
            let ppg_k = Arc::clone(&ppg_key);
            cmds.set(
                "project-search:replace-regex",
                lua.create_function(move |lua, path: LuaValue| {
                    let core = require_table(lua, "core")?;
                    let cv: LuaTable = core.get("command_view")?;
                    let path_str = match &path {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => "Project".to_string(),
                    };
                    let orv_k2 = Arc::clone(&orv_k);
                    let ppg_k2 = Arc::clone(&ppg_k);
                    let path_key = lua.create_registry_value(path)?;
                    let opts = lua.create_table()?;
                    opts.set(
                        "submit",
                        lua.create_function(move |lua, search: String| {
                            // Validate regex
                            let regex_mod: LuaTable = lua.globals().get("regex")?;
                            let (re, errmsg): (LuaValue, LuaValue) =
                                regex_mod.call_function("compile", search.as_str())?;
                            if matches!(re, LuaValue::Nil) {
                                let core2 = require_table(lua, "core")?;
                                core2.call_function::<()>("log", errmsg)?;
                                return Ok(());
                            }
                            let ppg: LuaFunction = lua.registry_value(&ppg_k2)?;
                            let orv_k3 = Arc::clone(&orv_k2);
                            let pk = lua.create_registry_value(lua.registry_value::<LuaValue>(&path_key)?)?;

                            let glob_submit = lua.create_function(move |lua, path_glob: LuaValue| {
                                let core2 = require_table(lua, "core")?;
                                let cv2: LuaTable = core2.get("command_view")?;
                                let orv_k4 = Arc::clone(&orv_k3);
                                let pk2 = lua.create_registry_value(lua.registry_value::<LuaValue>(&pk)?)?;
                                let pg_key = lua.create_registry_value(path_glob)?;
                                let search_str = search.clone();
                                let replace_opts = lua.create_table()?;
                                replace_opts.set(
                                    "submit",
                                    lua.create_function(move |lua, replace_text: String| {
                                        let orv: LuaFunction = lua.registry_value(&orv_k4)?;
                                        let path: LuaValue = lua.registry_value(&pk2)?;
                                        let pg: LuaValue = lua.registry_value(&pg_key)?;
                                        let config = require_table(lua, "core.config")?;
                                        let plugins: LuaTable = config.get("plugins")?;
                                        let pr_cfg: LuaTable = plugins.get("projectreplace")?;
                                        let backup: bool = pr_cfg.get("backup_originals").unwrap_or(true);
                                        let q = search_str.clone();

                                        let files = collect_native_files(lua, &path, &pg, &config)?;
                                        let search_opts = lua.create_table()?;
                                        search_opts.set("files", files.clone())?;
                                        search_opts.set("query", q.as_str())?;
                                        search_opts.set("mode", "regex")?;
                                        search_opts.set("no_case", false)?;

                                        let replace_native = lua.create_table()?;
                                        replace_native.set("files", files)?;
                                        replace_native.set("mode", "regex")?;
                                        replace_native.set("query", q.as_str())?;
                                        replace_native.set("replace", replace_text.as_str())?;
                                        replace_native.set("no_case", false)?;
                                        replace_native.set("backup_originals", backup)?;

                                        let fn_find = lua.create_function(|_, _: String| Ok(LuaValue::Nil))?;
                                        let fn_apply = lua.create_function(|_, _: String| Ok(LuaValue::Nil))?;
                                        orv.call::<()>((
                                            path, q, replace_text, fn_find, fn_apply,
                                            pg, Some("replace"), search_opts, replace_native,
                                        ))
                                    })?,
                                )?;
                                cv2.call_method::<()>("enter", ("Replace With", replace_opts))
                            })?;
                            ppg.call::<()>(glob_submit)
                        })?,
                    )?;
                    cv.call_method::<()>(
                        "enter",
                        (format!("Replace Regex In {path_str}"), opts),
                    )
                })?,
            )?;

            // project-search:swap
            let orv_k = Arc::clone(&orv_key);
            let ppg_k = Arc::clone(&ppg_key);
            let pyn_k = Arc::clone(&pyn_key);
            cmds.set(
                "project-search:swap",
                lua.create_function(move |lua, path: LuaValue| {
                    let core = require_table(lua, "core")?;
                    let cv: LuaTable = core.get("command_view")?;
                    let selected = get_selected_text(lua)?;
                    let path_str = match &path {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => "Project".to_string(),
                    };
                    let orv_k2 = Arc::clone(&orv_k);
                    let ppg_k2 = Arc::clone(&ppg_k);
                    let pyn_k2 = Arc::clone(&pyn_k);
                    let path_key = lua.create_registry_value(path)?;
                    let opts = lua.create_table()?;
                    if let LuaValue::String(ref s) = selected {
                        opts.set("text", s.to_str()?)?;
                    }
                    opts.set("select_text", true)?;
                    opts.set(
                        "submit",
                        lua.create_function(move |lua, text_a: String| {
                            let core2 = require_table(lua, "core")?;
                            let cv2: LuaTable = core2.get("command_view")?;
                            let orv_k3 = Arc::clone(&orv_k2);
                            let ppg_k3 = Arc::clone(&ppg_k2);
                            let pyn_k3 = Arc::clone(&pyn_k2);
                            let pk = lua.create_registry_value(lua.registry_value::<LuaValue>(&path_key)?)?;
                            let b_opts = lua.create_table()?;
                            b_opts.set(
                                "submit",
                                lua.create_function(move |lua, text_b: String| {
                                    if text_a.is_empty() || text_b.is_empty() {
                                        let core3 = require_table(lua, "core")?;
                                        core3.call_function::<()>("error", "Swap text cannot be empty")?;
                                        return Ok(());
                                    }
                                    // Chain of yes/no prompts for swap options
                                    let pyn: LuaFunction = lua.registry_value(&pyn_k3)?;
                                    let ppg: LuaFunction = lua.registry_value(&ppg_k3)?;
                                    let orv_k4 = Arc::clone(&orv_k3);
                                    let pk2 = lua.create_registry_value(lua.registry_value::<LuaValue>(&pk)?)?;
                                    let ta = text_a.clone();
                                    let tb = text_b.clone();
                                    let pyn_key2 = lua.create_registry_value(pyn.clone())?;
                                    let ppg_key2 = lua.create_registry_value(ppg)?;

                                    let a_regex_submit = lua.create_function(move |lua, a_regex: bool| {
                                        let pyn2: LuaFunction = lua.registry_value(&pyn_key2)?;
                                        let orv_k5 = Arc::clone(&orv_k4);
                                        let pk3 = lua.create_registry_value(lua.registry_value::<LuaValue>(&pk2)?)?;
                                        let ppg2: LuaFunction = lua.registry_value(&ppg_key2)?;
                                        let ta2 = ta.clone();
                                        let tb2 = tb.clone();
                                        let pyn_key3 = lua.create_registry_value(pyn2.clone())?;
                                        let ppg_key3 = lua.create_registry_value(ppg2)?;

                                        let a_case_submit = lua.create_function(move |lua, a_case: bool| {
                                            let pyn3: LuaFunction = lua.registry_value(&pyn_key3)?;
                                            let orv_k6 = Arc::clone(&orv_k5);
                                            let pk4 = lua.create_registry_value(lua.registry_value::<LuaValue>(&pk3)?)?;
                                            let ppg3: LuaFunction = lua.registry_value(&ppg_key3)?;
                                            let ta3 = ta2.clone();
                                            let tb3 = tb2.clone();
                                            let pyn_key4 = lua.create_registry_value(pyn3.clone())?;
                                            let ppg_key4 = lua.create_registry_value(ppg3)?;

                                            let b_regex_submit = lua.create_function(move |lua, b_regex: bool| {
                                                let pyn4: LuaFunction = lua.registry_value(&pyn_key4)?;
                                                let orv_k7 = Arc::clone(&orv_k6);
                                                let pk5 = lua.create_registry_value(lua.registry_value::<LuaValue>(&pk4)?)?;
                                                let ppg4: LuaFunction = lua.registry_value(&ppg_key4)?;
                                                let ta4 = ta3.clone();
                                                let tb4 = tb3.clone();

                                                let ppg_key5 = lua.create_registry_value(ppg4)?;
                                                let b_case_submit = lua.create_function(move |lua, b_case: bool| {
                                                    let orv_k8 = Arc::clone(&orv_k7);
                                                    let pk6 = lua.create_registry_value(lua.registry_value::<LuaValue>(&pk5)?)?;
                                                    let ppg5: LuaFunction = lua.registry_value(&ppg_key5)?;
                                                    let ta5 = ta4.clone();
                                                    let tb5 = tb4.clone();

                                                    let glob_submit = lua.create_function(move |lua, path_glob: LuaValue| {
                                                        let orv: LuaFunction = lua.registry_value(&orv_k8)?;
                                                        let path: LuaValue = lua.registry_value(&pk6)?;
                                                        let config = require_table(lua, "core.config")?;
                                                        let plugins: LuaTable = config.get("plugins")?;
                                                        let pr_cfg: LuaTable = plugins.get("projectreplace")?;
                                                        let backup: bool = pr_cfg.get("backup_originals").unwrap_or(true);
                                                        let qa = ta5.clone();
                                                        let qb = tb5.clone();

                                                        let files = collect_native_files(lua, &path, &path_glob, &config)?;

                                                        let search_opts = lua.create_table()?;
                                                        search_opts.set("files", files.clone())?;
                                                        search_opts.set("query", qa.as_str())?;
                                                        search_opts.set("mode", "plain")?;
                                                        search_opts.set("no_case", !a_case)?;

                                                        let replace_native = lua.create_table()?;
                                                        replace_native.set("files", files)?;
                                                        replace_native.set("mode", "swap")?;
                                                        replace_native.set("query", qa.as_str())?;
                                                        replace_native.set("replace", qb.as_str())?;
                                                        replace_native.set("no_case", !a_case)?;
                                                        replace_native.set("backup_originals", backup)?;
                                                        replace_native.set("query_b", qb.as_str())?;
                                                        replace_native.set("query_b_regex", b_regex)?;
                                                        replace_native.set("query_b_case", b_case)?;
                                                        replace_native.set("query_a_regex", a_regex)?;

                                                        let fn_find = lua.create_function(|_, _: String| Ok(LuaValue::Nil))?;
                                                        let fn_apply = lua.create_function(|_, _: String| Ok(LuaValue::Nil))?;
                                                        orv.call::<()>((
                                                            path, qa, qb, fn_find, fn_apply,
                                                            path_glob, Some("swap"), search_opts, replace_native,
                                                        ))
                                                    })?;
                                                    ppg5.call::<()>(glob_submit)
                                                })?;
                                                pyn4.call::<()>(("Match Case for B? [Y/n]", true, b_case_submit))
                                            })?;
                                            pyn3.call::<()>(("Regex for B? [y/N]", false, b_regex_submit))
                                        })?;
                                        pyn2.call::<()>(("Match Case for A? [Y/n]", true, a_case_submit))
                                    })?;
                                    pyn.call::<()>(("Regex for A? [y/N]", false, a_regex_submit))
                                })?,
                            )?;
                            cv2.call_method::<()>("enter", ("Swap Text B", b_opts))
                        })?,
                    )?;
                    cv.call_method::<()>(
                        "enter",
                        (format!("Swap Text A In {path_str}"), opts),
                    )
                })?,
            )?;

            command.call_function::<()>("add", (LuaValue::Nil, cmds))?;

            // ReplaceView-specific commands
            let rv_cmds = lua.create_table()?;

            rv_cmds.set(
                "project-search:confirm-replace",
                lua.create_function(|lua, ()| {
                    let core = require_table(lua, "core")?;
                    let view: LuaTable = core.get("active_view")?;
                    let phase: String = view.get("phase")?;
                    if phase == "confirming" {
                        view.call_method::<()>("apply_replace", ())?;
                    }
                    Ok(())
                })?,
            )?;

            rv_cmds.set(
                "project-search:select-previous",
                lua.create_function(|lua, ()| {
                    let core = require_table(lua, "core")?;
                    let view: LuaTable = core.get("active_view")?;
                    let idx: i64 = view.get("selected_idx")?;
                    view.set("selected_idx", 1i64.max(idx - 1))?;
                    view.call_method::<()>("scroll_to_make_selected_visible", ())
                })?,
            )?;

            rv_cmds.set(
                "project-search:select-next",
                lua.create_function(|lua, ()| {
                    let core = require_table(lua, "core")?;
                    let view: LuaTable = core.get("active_view")?;
                    let idx: i64 = view.get("selected_idx")?;
                    let results: LuaTable = view.get("results")?;
                    let len = results.raw_len() as i64;
                    view.set("selected_idx", len.min(idx + 1))?;
                    view.call_method::<()>("scroll_to_make_selected_visible", ())
                })?,
            )?;

            rv_cmds.set(
                "project-search:open-selected",
                lua.create_function(|lua, ()| {
                    let core = require_table(lua, "core")?;
                    let view: LuaTable = core.get("active_view")?;
                    view.call_method::<()>("open_selected_result", ())
                })?,
            )?;

            rv_cmds.set(
                "project-search:move-to-previous-page",
                lua.create_function(|lua, ()| {
                    let core = require_table(lua, "core")?;
                    let view: LuaTable = core.get("active_view")?;
                    let scroll: LuaTable = view.get("scroll")?;
                    let to: LuaTable = scroll.get("to")?;
                    let y: f64 = to.get("y")?;
                    let size: LuaTable = view.get("size")?;
                    let sy: f64 = size.get("y")?;
                    to.set("y", y - sy)
                })?,
            )?;

            rv_cmds.set(
                "project-search:move-to-next-page",
                lua.create_function(|lua, ()| {
                    let core = require_table(lua, "core")?;
                    let view: LuaTable = core.get("active_view")?;
                    let scroll: LuaTable = view.get("scroll")?;
                    let to: LuaTable = scroll.get("to")?;
                    let y: f64 = to.get("y")?;
                    let size: LuaTable = view.get("size")?;
                    let sy: f64 = size.get("y")?;
                    to.set("y", y + sy)
                })?,
            )?;

            rv_cmds.set(
                "project-search:move-to-start-of-doc",
                lua.create_function(|lua, ()| {
                    let core = require_table(lua, "core")?;
                    let view: LuaTable = core.get("active_view")?;
                    let scroll: LuaTable = view.get("scroll")?;
                    let to: LuaTable = scroll.get("to")?;
                    to.set("y", 0.0)
                })?,
            )?;

            rv_cmds.set(
                "project-search:move-to-end-of-doc",
                lua.create_function(|lua, ()| {
                    let core = require_table(lua, "core")?;
                    let view: LuaTable = core.get("active_view")?;
                    let scrollable: f64 = view.call_method("get_scrollable_size", ())?;
                    let scroll: LuaTable = view.get("scroll")?;
                    let to: LuaTable = scroll.get("to")?;
                    to.set("y", scrollable)
                })?,
            )?;

            command.call_function::<()>("add", (replace_view, rv_cmds))?;

            // Keymaps
            let keymap = require_table(lua, "core.keymap")?;
            let bindings = lua.create_table()?;
            bindings.set("ctrl+shift+h", "project-search:replace")?;
            bindings.set("f5", "project-search:confirm-replace")?;
            bindings.set("up", "project-search:select-previous")?;
            bindings.set("down", "project-search:select-next")?;
            bindings.set("return", "project-search:open-selected")?;
            bindings.set("pageup", "project-search:move-to-previous-page")?;
            bindings.set("pagedown", "project-search:move-to-next-page")?;
            bindings.set("ctrl+home", "project-search:move-to-start-of-doc")?;
            bindings.set("ctrl+end", "project-search:move-to-end-of-doc")?;
            bindings.set("home", "project-search:move-to-start-of-doc")?;
            bindings.set("end", "project-search:move-to-end-of-doc")?;
            keymap.call_function::<()>("add", bindings)?;

            Ok(LuaValue::Boolean(true))
        })?,
    )
}
