use mlua::prelude::*;

use std::sync::Arc;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn build_results_view(lua: &Lua) -> LuaResult<(LuaTable, Arc<LuaRegistryKey>)> {
    let view_class = require_table(lua, "core.view")?;
    let results_view = view_class.call_method::<LuaTable>("extend", ())?;

    results_view.set(
        "__tostring",
        lua.create_function(|_, _: LuaTable| Ok("ResultsView"))?,
    )?;
    results_view.set("context", "session")?;

    let class_key = Arc::new(lua.create_registry_value(results_view.clone())?);

    // ResultsView:new(path, text, fn, path_glob, search_opts)
    {
        let ck = Arc::clone(&class_key);
        results_view.set(
            "new",
            lua.create_function(move |lua, (this, path, text, fn_find, path_glob, search_opts):
                (LuaTable, LuaValue, String, LuaFunction, LuaValue, LuaTable)| {
                let class: LuaTable = lua.registry_value(&ck)?;
                let super_tbl: LuaTable = class.get("super")?;
                let super_new: LuaFunction = super_tbl.get("new")?;
                super_new.call::<()>(this.clone())?;
                this.set("scrollable", true)?;
                this.set("brightness", 0.0)?;
                this.set("max_h_scroll", 0.0)?;
                this.set("display_results", lua.create_table()?)?;
                let begin_search: LuaFunction = this.get("begin_search")?;
                begin_search.call::<()>((
                    this.clone(),
                    path,
                    text,
                    fn_find,
                    path_glob,
                    search_opts,
                ))?;
                Ok(())
            })?,
        )?;
    }

    // get_name
    results_view.set(
        "get_name",
        lua.create_function(|_, this: LuaTable| {
            let pg: LuaValue = this.get("path_glob")?;
            if let LuaValue::String(ref s) = pg {
                let s = s.to_str()?;
                if !s.is_empty() {
                    return Ok(format!("Search Results [{s}]"));
                }
            }
            Ok("Search Results".to_string())
        })?,
    )?;

    // rebuild_display_results
    results_view.set(
        "rebuild_display_results",
        lua.create_function(|lua, this: LuaTable| {
            let display = lua.create_table()?;
            let results: LuaTable = this.get("results")?;
            let mut last_file = String::new();
            let mut idx = 0i64;
            for pair in results.sequence_values::<LuaTable>() {
                let item = pair?;
                let file: String = item.get("file")?;
                if file != last_file {
                    idx += 1;
                    let entry = lua.create_table()?;
                    entry.set("kind", "file")?;
                    entry.set("file", file.as_str())?;
                    display.set(idx, entry)?;
                    last_file.clone_from(&file);
                }
                idx += 1;
                let i: i64 = item.get::<i64>("_idx").unwrap_or(idx);
                let entry = lua.create_table()?;
                entry.set("kind", "match")?;
                entry.set("result_idx", i)?;
                display.set(idx, entry)?;
            }
            this.set("display_results", display)?;
            Ok(())
        })?,
    )?;

    // begin_search(self, path, text, fn, path_glob, search_opts)
    // Uses coroutine.yield via a thin Lua wrapper
    {
        results_view.set(
            "begin_search",
            lua.create_function(|lua, (this, path, text, fn_find, path_glob, search_opts):
                (LuaTable, LuaValue, String, LuaFunction, LuaValue, LuaTable)| {
                let search_args = lua.create_table()?;
                search_args.push(path.clone())?;
                search_args.push(text.as_str())?;
                search_args.push(fn_find)?;
                search_args.push(path_glob.clone())?;
                search_args.push(search_opts.clone())?;
                this.set("search_args", search_args)?;

                let results = lua.create_table()?;
                this.set("results", results)?;
                this.set("last_file_idx", 1)?;
                this.set("query", text.as_str())?;
                this.set("path_glob", path_glob)?;
                this.set("searching", true)?;
                this.set("selected_idx", 0)?;
                this.set("search_opts", search_opts.clone())?;

                // Collect files
                let native_search = require_table(lua, "project_search")?;
                let config = require_table(lua, "core.config")?;
                let core = require_table(lua, "core")?;

                let files = collect_native_files(lua, &path, &config)?;
                // Filter by path_glob
                let files = filter_by_path_glob(lua, files, &this)?;

                let file_count = files.raw_len() as i64;
                this.set("last_file_idx", file_count)?;

                let native_opts = lua.create_table()?;
                native_opts.set("files", files)?;
                native_opts.set("query", search_opts.get::<LuaValue>("query")?)?;
                native_opts.set("mode", search_opts.get::<LuaValue>("mode")?)?;
                native_opts.set("no_case", search_opts.get::<LuaValue>("no_case")?)?;

                let handle: LuaValue = native_search.call_function("search", native_opts)?;

                // Build tick function (called from Lua loop that yields)
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
                            this.set("searching", false)?;
                            return Ok(true); // done
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
                                entry.set("_idx", count)?;
                                results.set(count, entry)?;
                            }
                            let rebuild: LuaFunction = this.get("rebuild_display_results")?;
                            rebuild.call::<()>(this.clone())?;
                            core.set("redraw", true)?;
                        }
                        let done: bool = polled.get("done").unwrap_or(false);
                        if done {
                            this.set("searching", false)?;
                            this.set("brightness", 100.0)?;
                            let rebuild: LuaFunction = this.get("rebuild_display_results")?;
                            rebuild.call::<()>(this.clone())?;
                            core.set("redraw", true)?;
                            return Ok(true); // done
                        }
                    }
                    Ok(false)
                })?;

                // Lua wrapper with coroutine.yield
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
    }

    // refresh
    results_view.set(
        "refresh",
        lua.create_function(|lua, this: LuaTable| {
            let search_args: LuaTable = this.get("search_args")?;
            let begin_search: LuaFunction = this.get("begin_search")?;
            let table_unpack: LuaFunction = lua.globals().get::<LuaTable>("table")?.get("unpack")?;
            let args: LuaMultiValue = table_unpack.call(search_args)?;
            let mut call_args = LuaMultiValue::new();
            call_args.push_back(LuaValue::Table(this));
            call_args.extend(args);
            begin_search.call::<()>(call_args)
        })?,
    )?;

    // on_mouse_moved
    results_view.set(
        "on_mouse_moved",
        lua.create_function(|_lua, (this, mx, my, rest): (LuaTable, f64, f64, LuaMultiValue)| {
            let class: LuaTable = this.get("super")?;
            let super_omm: LuaFunction = class.get("on_mouse_moved")?;
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
                // i, item, x, y, w, h
                let item = vals.get(1);
                let rx = lua_f64(vals.get(2));
                let ry = lua_f64(vals.get(3));
                let rw = lua_f64(vals.get(4));
                let rh = lua_f64(vals.get(5));
                if mx >= rx && my >= ry && mx < rx + rw && my < ry + rh {
                    if let Some(LuaValue::Table(item)) = item {
                        let kind: String = item.get("kind")?;
                        if kind == "match" {
                            let result_idx: i64 = item.get("result_idx")?;
                            this.set("selected_idx", result_idx)?;
                        }
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
        results_view.set(
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
    results_view.set(
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
        results_view.set(
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
    results_view.set(
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
    results_view.set(
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
    results_view.set(
        "get_scrollable_size",
        lua.create_function(|_, this: LuaTable| {
            let yoffset: f64 = this.call_method("get_results_yoffset", ())?;
            let display: LuaTable = this.get("display_results")?;
            let count = display.raw_len() as f64;
            let lh: f64 = this.call_method("get_line_height", ())?;
            Ok(yoffset + count * lh)
        })?,
    )?;

    // get_h_scrollable_size
    results_view.set(
        "get_h_scrollable_size",
        lua.create_function(|_, this: LuaTable| {
            let v: f64 = this.get("max_h_scroll")?;
            Ok(v)
        })?,
    )?;

    // get_visible_results_range
    results_view.set(
        "get_visible_results_range",
        lua.create_function(|lua, this: LuaTable| {
            let lh: f64 = this.call_method("get_line_height", ())?;
            let oy: f64 = this.call_method("get_results_yoffset", ())?;
            let style = require_table(lua, "core.style")?;
            let font: LuaValue = style.get("font")?;
            let fh: f64 = font_get_height(&font)?;
            let scroll: LuaTable = this.get("scroll")?;
            let scroll_y: f64 = scroll.get("y")?;
            let raw_min = scroll_y + oy - fh;
            let min = 1i64.max((raw_min / lh).floor() as i64);
            let size: LuaTable = this.get("size")?;
            let size_y: f64 = size.get("y")?;
            let max = min + (size_y / lh).floor() as i64 + 1;
            Ok((min, max))
        })?,
    )?;

    // each_visible_result - stateful iterator, no coroutine.yield
    results_view.set(
        "each_visible_result",
        lua.create_function(|lua, this: LuaTable| {
            let lh: f64 = this.call_method("get_line_height", ())?;
            let (cx, cy): (f64, f64) = this.call_method("get_content_offset", ())?;
            let (min, max): (i64, i64) = this.call_method("get_visible_results_range", ())?;
            let oy: f64 = this.call_method("get_results_yoffset", ())?;
            let start_y = cy + oy + lh * (min - 1) as f64;
            let display: LuaTable = this.get("display_results")?;

            // Precompute entries
            let entries = lua.create_table()?;
            let mut count = 0i64;
            let mut y = start_y;
            for i in min..=max {
                let item: LuaValue = display.get(i)?;
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
    results_view.set(
        "scroll_to_make_selected_visible",
        lua.create_function(|_, this: LuaTable| {
            let selected_idx: i64 = this.get("selected_idx")?;
            if selected_idx <= 0 {
                return Ok(());
            }
            // Find display index for selected_idx
            let display: LuaTable = this.get("display_results")?;
            let mut display_idx: Option<i64> = None;
            for i in 1..=display.raw_len() as i64 {
                let item: LuaTable = display.get(i)?;
                let kind: String = item.get("kind")?;
                if kind == "match" {
                    let ri: i64 = item.get("result_idx")?;
                    if ri == selected_idx {
                        display_idx = Some(i);
                        break;
                    }
                }
            }
            let Some(idx) = display_idx else {
                return Ok(());
            };
            let h: f64 = this.call_method("get_line_height", ())?;
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
    results_view.set(
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

            renderer.call_function::<()>("draw_rect", (ox, oy, size_x, yoffset, bg.clone()))?;
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

            let padding: LuaTable = style.get("padding")?;
            let px: f64 = padding.get("x")?;
            let py: f64 = padding.get("y")?;
            let x = ox + px;
            let y = oy + py;

            let searching: bool = this.get("searching")?;
            let results: LuaTable = this.get("results")?;
            let result_count = results.raw_len();
            let query: String = this.get("query")?;
            let path_glob: LuaValue = this.get("path_glob")?;
            let last_file_idx: i64 = this.get("last_file_idx")?;

            let glob_suffix = match &path_glob {
                LuaValue::String(s) => {
                    let s = s.to_str()?;
                    if s.is_empty() { String::new() } else { format!(" in {s}") }
                }
                _ => String::new(),
            };

            let text = if searching {
                format!(
                    "Searching ({last_file_idx} files, {result_count} matches) for {query:?}{glob_suffix}..."
                )
            } else {
                format!("Found {result_count} matches for {query:?}{glob_suffix}")
            };

            let brightness: f64 = this.get("brightness")?;
            let color: LuaValue = common.call_function(
                "lerp",
                (
                    style.get::<LuaValue>("text")?,
                    style.get::<LuaValue>("accent")?,
                    brightness / 100.0,
                ),
            )?;
            renderer.call_function::<()>("draw_text", (style.get::<LuaValue>("font")?, text, x, y, color))?;

            // Horizontal line
            let dim: LuaValue = style.get("dim")?;
            let text_color: LuaValue = style.get("text")?;
            let dcolor: LuaValue =
                common.call_function("lerp", (dim, text_color, brightness / 100.0))?;
            let ds: f64 = style.get("divider_size")?;
            renderer.call_function::<()>(
                "draw_rect",
                (x, oy + yoffset - py, size_x - px * 2.0, ds, dcolor),
            )?;
            if searching {
                let text_c: LuaValue = style.get("text")?;
                renderer.call_function::<()>(
                    "draw_rect",
                    (x, oy + yoffset - py, size_x - px * 2.0, ds, text_c),
                )?;
            }

            // Results
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
                let item = match &vals[1] {
                    LuaValue::Table(t) => t,
                    _ => break,
                };
                let ix = lua_f64(vals.get(2)) + px;
                let iy = lua_f64(vals.get(3));
                let iw = lua_f64(vals.get(4));
                let ih = lua_f64(vals.get(5));

                let kind: String = item.get("kind")?;
                if kind == "file" {
                    let file: String = item.get("file")?;
                    let label: String =
                        root_project.call_method("normalize_path", file)?;
                    let end_x: f64 = common.call_function(
                        "draw_text",
                        (style_font.clone(), label, "left", ix, iy, iw, ih),
                    )?;
                    // Actually draw with accent color - common.draw_text returns x
                    // We need to use the 7-arg form
                    let _ = end_x;
                    // Re-draw properly: common.draw_text(font, color, text, align, x, y, w, h)
                    // The Lua signature is common.draw_text(font, color, text, align, x, y, w, h)
                    let file2: String = item.get("file")?;
                    let label2: String =
                        root_project.call_method("normalize_path", file2)?;
                    common.call_function::<f64>(
                        "draw_text",
                        (
                            style_font.clone(),
                            style_accent.clone(),
                            label2,
                            "left",
                            ix,
                            iy,
                            iw,
                            ih,
                        ),
                    )?;
                } else {
                    let result_idx: i64 = item.get("result_idx")?;
                    let results: LuaTable = this.get("results")?;
                    let match_item: LuaTable = results.get(result_idx)?;
                    let color = if result_idx == selected_idx {
                        renderer.call_function::<()>(
                            "draw_rect",
                            (ix - px, iy, iw, ih, line_hl.clone()),
                        )?;
                        style_accent.clone()
                    } else {
                        style_text.clone()
                    };
                    let line: i64 = match_item.get("line")?;
                    let col: i64 = match_item.get("col")?;
                    let prefix = format!("  line {line} (col {col}): ");
                    let end_x: f64 = common.call_function(
                        "draw_text",
                        (
                            style_font.clone(),
                            style_dim.clone(),
                            prefix,
                            "left",
                            ix,
                            iy,
                            iw,
                            ih,
                        ),
                    )?;
                    let match_text: String = match_item.get("text")?;
                    let end_x2: f64 = common.call_function(
                        "draw_text",
                        (
                            style_code_font.clone(),
                            color,
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
            }
            this.set("max_h_scroll", max_h_scroll)?;

            core.call_function::<()>("pop_clip_rect", ())?;
            this.call_method::<()>("draw_scrollbar", ())?;
            Ok(())
        })?,
    )?;

    Ok((results_view, class_key))
}

fn collect_native_files(lua: &Lua, path: &LuaValue, config: &LuaTable) -> LuaResult<LuaTable> {
    let native_model = require_table(lua, "project_model")?;
    let core = require_table(lua, "core")?;

    // Check if path is a single file
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

    // Collect roots
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
    Ok(files)
}

fn filter_by_path_glob(lua: &Lua, files: LuaTable, this: &LuaTable) -> LuaResult<LuaTable> {
    let path_glob: LuaValue = this.get("path_glob")?;
    let glob_str = match &path_glob {
        LuaValue::String(s) => {
            let s = s.to_str()?;
            if s.is_empty() { return Ok(files); }
            s.to_string()
        }
        _ => return Ok(files),
    };

    let pattern = glob_to_lua_pattern(&glob_str.replace('\\', "/"));
    let core = require_table(lua, "core")?;
    let common = require_table(lua, "core.common")?;
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
    Ok(filtered)
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
            // Escape regex metacharacters
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
        let belongs: bool = common.call_function("path_belongs_to", (filename, project_path.as_str()))?;
        if belongs {
            let rel: String = common.call_function("relative_path", (project_path, filename))?;
            let rel_normalized = rel.replace('\\', "/");
            let re = lua.globals().get::<LuaTable>("regex")?;
            let compiled: LuaValue = re.call_function("compile", pattern)?;
            if !matches!(compiled, LuaValue::Nil) {
                let m: LuaValue = re.call_function("cmatch", (compiled, rel_normalized))?;
                return Ok(!matches!(m, LuaValue::Nil));
            }
            return Ok(false);
        }
    }
    let re = lua.globals().get::<LuaTable>("regex")?;
    let compiled: LuaValue = re.call_function("compile", pattern)?;
    if !matches!(compiled, LuaValue::Nil) {
        let m: LuaValue = re.call_function("cmatch", (compiled, normalized))?;
        return Ok(!matches!(m, LuaValue::Nil));
    }
    Ok(false)
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

/// Registers `plugins.projectsearch`: project-wide text search with results view.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.projectsearch",
        lua.create_function(|lua, ()| {
            let (results_view, class_key) = build_results_view(lua)?;

            let module = lua.create_table()?;
            module.set("ResultsView", results_view.clone())?;

            // get_selected_text helper
            let get_selected_text = lua.create_function(|lua, ()| -> LuaResult<LuaValue> {
                let core = require_table(lua, "core")?;
                let view: LuaTable = core.get("active_view")?;
                let doc: LuaValue = view.get("doc")?;
                if let LuaValue::Table(ref d) = doc {
                    let sel: LuaMultiValue = d.call_method("get_selection", ())?;
                    let text: LuaValue = d.call_method("get_text", sel)?;
                    return Ok(text);
                }
                Ok(LuaValue::Nil)
            })?;
            let gst_key = Arc::new(lua.create_registry_value(get_selected_text)?);

            // search_plain
            let ck = Arc::clone(&class_key);
            let mk = lua.create_registry_value(module.clone())?;
            let search_plain = lua.create_function(move |lua, (text, path, insensitive):
                (String, LuaValue, Option<bool>)| {
                if text.is_empty() {
                    let core = require_table(lua, "core")?;
                    core.call_function::<()>("error", "Expected non-empty string")?;
                    return Ok(LuaValue::Nil);
                }
                let insensitive = insensitive.unwrap_or(false);
                let m: LuaTable = lua.registry_value(&mk)?;
                let path_glob: LuaValue = m.get("pending_path_glob")?;
                let rv_class: LuaTable = lua.registry_value(&ck)?;
                let search_opts = lua.create_table()?;
                search_opts.set("query", text.as_str())?;
                search_opts.set("mode", "plain")?;
                search_opts.set("no_case", insensitive)?;

                let fn_find = lua.create_function(move |_, _line: String| {
                    Ok(LuaValue::Nil) // unused, native search handles it
                })?;

                let rv: LuaTable = rv_class.call((path.clone(), text, fn_find, path_glob, search_opts))?;
                let core = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let node: LuaTable = root_view.call_method("get_active_node_default", ())?;
                node.call_method::<()>("add_view", rv.clone())?;
                Ok(LuaValue::Table(rv))
            })?;
            module.set("search_plain", search_plain)?;

            // search_regex
            let ck = Arc::clone(&class_key);
            let mk = lua.create_registry_value(module.clone())?;
            let search_regex = lua.create_function(move |lua, (text, path, insensitive):
                (String, LuaValue, Option<bool>)| {
                if text.is_empty() {
                    let core = require_table(lua, "core")?;
                    core.call_function::<()>("error", "Expected non-empty string")?;
                    return Ok(LuaValue::Nil);
                }
                let insensitive = insensitive.unwrap_or(false);
                let m: LuaTable = lua.registry_value(&mk)?;
                let path_glob: LuaValue = m.get("pending_path_glob")?;
                let rv_class: LuaTable = lua.registry_value(&ck)?;

                // Validate regex
                let regex_mod: LuaTable = lua.globals().get("regex")?;
                let flags = if insensitive { "i" } else { "" };
                let (re, errmsg): (LuaValue, LuaValue) =
                    regex_mod.call_function("compile", (text.as_str(), flags))?;
                if matches!(re, LuaValue::Nil) {
                    let core = require_table(lua, "core")?;
                    core.call_function::<()>("log", errmsg)?;
                    return Ok(LuaValue::Nil);
                }

                let search_opts = lua.create_table()?;
                search_opts.set("query", text.as_str())?;
                search_opts.set("mode", "regex")?;
                search_opts.set("no_case", insensitive)?;

                let fn_find = lua.create_function(move |_, _line: String| {
                    Ok(LuaValue::Nil)
                })?;

                let rv: LuaTable = rv_class.call((path.clone(), text, fn_find, path_glob, search_opts))?;
                let core = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let node: LuaTable = root_view.call_method("get_active_node_default", ())?;
                node.call_method::<()>("add_view", rv.clone())?;
                Ok(LuaValue::Table(rv))
            })?;
            module.set("search_regex", search_regex)?;

            // search_fuzzy
            let ck = Arc::clone(&class_key);
            let mk = lua.create_registry_value(module.clone())?;
            let search_fuzzy = lua.create_function(move |lua, (text, path, insensitive):
                (String, LuaValue, Option<bool>)| {
                if text.is_empty() {
                    let core = require_table(lua, "core")?;
                    core.call_function::<()>("error", "Expected non-empty string")?;
                    return Ok(LuaValue::Nil);
                }
                let insensitive = insensitive.unwrap_or(false);
                let m: LuaTable = lua.registry_value(&mk)?;
                let path_glob: LuaValue = m.get("pending_path_glob")?;
                let rv_class: LuaTable = lua.registry_value(&ck)?;

                let search_opts = lua.create_table()?;
                search_opts.set("query", text.as_str())?;
                search_opts.set("mode", "fuzzy")?;
                search_opts.set("no_case", insensitive)?;

                let fn_find = lua.create_function(move |_, _line: String| {
                    Ok(LuaValue::Nil)
                })?;

                let rv: LuaTable = rv_class.call((path.clone(), text, fn_find, path_glob, search_opts))?;
                let core = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let node: LuaTable = root_view.call_method("get_active_node_default", ())?;
                node.call_method::<()>("add_view", rv.clone())?;
                Ok(LuaValue::Table(rv))
            })?;
            module.set("search_fuzzy", search_fuzzy)?;

            // Commands
            let command = require_table(lua, "core.command")?;
            let gst_k = Arc::clone(&gst_key);

            // Global commands
            let cmds = lua.create_table()?;

            // project-search:find
            let mk2 = lua.create_registry_value(module.clone())?;
            let gst_k2 = Arc::clone(&gst_k);
            cmds.set(
                "project-search:find",
                lua.create_function(move |lua, path: LuaValue| {
                    let m: LuaTable = lua.registry_value(&mk2)?;
                    let core = require_table(lua, "core")?;
                    let command_view: LuaTable = core.get("command_view")?;
                    let gst: LuaFunction = lua.registry_value(&gst_k2)?;
                    let selected: LuaValue = gst.call(())?;
                    let path_str = match &path {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => "Project".to_string(),
                    };
                    let m_key = lua.create_registry_value(m)?;
                    let path_key = lua.create_registry_value(path)?;
                    let opts = lua.create_table()?;
                    if let LuaValue::String(ref s) = selected {
                        opts.set("text", s.to_str()?)?;
                    }
                    opts.set("select_text", true)?;
                    opts.set(
                        "submit",
                        lua.create_function(move |lua, text: String| {
                            let core2 = require_table(lua, "core")?;
                            let cv: LuaTable = core2.get("command_view")?;
                            let mk3 = lua.create_registry_value(lua.registry_value::<LuaTable>(&m_key)?)?;
                            let pk = lua.create_registry_value(lua.registry_value::<LuaValue>(&path_key)?)?;
                            let glob_opts = lua.create_table()?;
                            glob_opts.set(
                                "submit",
                                lua.create_function(move |lua, glob_text: String| {
                                    let m: LuaTable = lua.registry_value(&mk3)?;
                                    let path: LuaValue = lua.registry_value(&pk)?;
                                    let pg = if glob_text.is_empty() {
                                        LuaValue::Nil
                                    } else {
                                        LuaValue::String(lua.create_string(&glob_text)?)
                                    };
                                    m.set("pending_path_glob", pg)?;
                                    let sp: LuaFunction = m.get("search_plain")?;
                                    sp.call::<()>((text.as_str(), path, true))?;
                                    m.set("pending_path_glob", LuaValue::Nil)?;
                                    Ok(())
                                })?,
                            )?;
                            cv.call_method::<()>(
                                "enter",
                                ("Path Glob Filter (optional)", glob_opts),
                            )
                        })?,
                    )?;
                    command_view.call_method::<()>(
                        "enter",
                        (format!("Find Text In {path_str}"), opts),
                    )
                })?,
            )?;

            // project-search:find-regex
            let mk2 = lua.create_registry_value(module.clone())?;
            cmds.set(
                "project-search:find-regex",
                lua.create_function(move |lua, path: LuaValue| {
                    let m: LuaTable = lua.registry_value(&mk2)?;
                    let core = require_table(lua, "core")?;
                    let command_view: LuaTable = core.get("command_view")?;
                    let path_str = match &path {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => "Project".to_string(),
                    };
                    let m_key = lua.create_registry_value(m)?;
                    let path_key = lua.create_registry_value(path)?;
                    let opts = lua.create_table()?;
                    opts.set(
                        "submit",
                        lua.create_function(move |lua, text: String| {
                            let core2 = require_table(lua, "core")?;
                            let cv: LuaTable = core2.get("command_view")?;
                            let mk3 = lua.create_registry_value(lua.registry_value::<LuaTable>(&m_key)?)?;
                            let pk = lua.create_registry_value(lua.registry_value::<LuaValue>(&path_key)?)?;
                            let glob_opts = lua.create_table()?;
                            glob_opts.set(
                                "submit",
                                lua.create_function(move |lua, glob_text: String| {
                                    let m: LuaTable = lua.registry_value(&mk3)?;
                                    let path: LuaValue = lua.registry_value(&pk)?;
                                    let pg = if glob_text.is_empty() {
                                        LuaValue::Nil
                                    } else {
                                        LuaValue::String(lua.create_string(&glob_text)?)
                                    };
                                    m.set("pending_path_glob", pg)?;
                                    let sr: LuaFunction = m.get("search_regex")?;
                                    sr.call::<()>((text.as_str(), path, true))?;
                                    m.set("pending_path_glob", LuaValue::Nil)?;
                                    Ok(())
                                })?,
                            )?;
                            cv.call_method::<()>(
                                "enter",
                                ("Path Glob Filter (optional)", glob_opts),
                            )
                        })?,
                    )?;
                    command_view.call_method::<()>(
                        "enter",
                        (format!("Find Regex In {path_str}"), opts),
                    )
                })?,
            )?;

            // project-search:fuzzy-find
            let mk2 = lua.create_registry_value(module.clone())?;
            let gst_k3 = Arc::clone(&gst_key);
            cmds.set(
                "project-search:fuzzy-find",
                lua.create_function(move |lua, path: LuaValue| {
                    let m: LuaTable = lua.registry_value(&mk2)?;
                    let core = require_table(lua, "core")?;
                    let command_view: LuaTable = core.get("command_view")?;
                    let gst: LuaFunction = lua.registry_value(&gst_k3)?;
                    let selected: LuaValue = gst.call(())?;
                    let path_str = match &path {
                        LuaValue::String(s) => s.to_str()?.to_string(),
                        _ => "Project".to_string(),
                    };
                    let m_key = lua.create_registry_value(m)?;
                    let path_key = lua.create_registry_value(path)?;
                    let opts = lua.create_table()?;
                    if let LuaValue::String(ref s) = selected {
                        opts.set("text", s.to_str()?)?;
                    }
                    opts.set("select_text", true)?;
                    opts.set(
                        "submit",
                        lua.create_function(move |lua, text: String| {
                            let core2 = require_table(lua, "core")?;
                            let cv: LuaTable = core2.get("command_view")?;
                            let mk3 = lua.create_registry_value(lua.registry_value::<LuaTable>(&m_key)?)?;
                            let pk = lua.create_registry_value(lua.registry_value::<LuaValue>(&path_key)?)?;
                            let glob_opts = lua.create_table()?;
                            glob_opts.set(
                                "submit",
                                lua.create_function(move |lua, glob_text: String| {
                                    let m: LuaTable = lua.registry_value(&mk3)?;
                                    let path: LuaValue = lua.registry_value(&pk)?;
                                    let pg = if glob_text.is_empty() {
                                        LuaValue::Nil
                                    } else {
                                        LuaValue::String(lua.create_string(&glob_text)?)
                                    };
                                    m.set("pending_path_glob", pg)?;
                                    let sf: LuaFunction = m.get("search_fuzzy")?;
                                    sf.call::<()>((text.as_str(), path, true))?;
                                    m.set("pending_path_glob", LuaValue::Nil)?;
                                    Ok(())
                                })?,
                            )?;
                            cv.call_method::<()>(
                                "enter",
                                ("Path Glob Filter (optional)", glob_opts),
                            )
                        })?,
                    )?;
                    command_view.call_method::<()>(
                        "enter",
                        (format!("Fuzzy Find Text In {path_str}"), opts),
                    )
                })?,
            )?;

            command.call_function::<()>("add", (LuaValue::Nil, cmds))?;

            // ResultsView-specific commands
            let rv_cmds = lua.create_table()?;

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
                "project-search:refresh",
                lua.create_function(|lua, ()| {
                    let core = require_table(lua, "core")?;
                    let view: LuaTable = core.get("active_view")?;
                    view.call_method::<()>("refresh", ())
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

            command.call_function::<()>("add", (results_view, rv_cmds))?;

            // Keymaps
            let keymap = require_table(lua, "core.keymap")?;
            let bindings = lua.create_table()?;
            bindings.set("f5", "project-search:refresh")?;
            bindings.set("ctrl+shift+f", "project-search:find")?;
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

            Ok(LuaValue::Table(module))
        })?,
    )
}
