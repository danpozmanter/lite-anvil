use std::sync::Arc;

use mlua::prelude::*;

// ── Rust native helpers ──────────────────────────────────────────────────────

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Initialise all TreeView state fields.
///
/// Reads initial plugin config and restores any session-persisted size, so
/// `TreeView:new()` only needs to call `super.new` then this function.
fn init(lua: &Lua, self_table: LuaTable) -> LuaResult<()> {
    let config: LuaTable = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let tv_cfg: LuaTable = plugins.get("treeview")?;
    // Restore session-persisted treeview width if available.
    let core = require_table(lua, "core")?;
    if let LuaValue::Table(session) = core.get::<LuaValue>("session")? {
        let saved: LuaValue = session.get("treeview_size")?;
        match saved {
            LuaValue::Number(_) | LuaValue::Integer(_) => tv_cfg.set("size", saved)?,
            _ => {}
        }
    }
    self_table.set("scrollable", true)?;
    self_table.set("visible", tv_cfg.get::<LuaValue>("visible")?)?;
    self_table.set("init_size", true)?;
    self_table.set("target_size", tv_cfg.get::<LuaValue>("size")?)?;
    self_table.set("show_hidden", tv_cfg.get::<LuaValue>("show_hidden")?)?;
    self_table.set("show_ignored", tv_cfg.get::<LuaValue>("show_ignored")?)?;
    let tooltip = lua.create_table()?;
    tooltip.set("x", LuaValue::Nil)?;
    tooltip.set("y", LuaValue::Nil)?;
    tooltip.set("begin", 0.0f64)?;
    tooltip.set("alpha", 0.0f64)?;
    self_table.set("tooltip", tooltip)?;
    self_table.set("last_scroll_y", 0.0f64)?;
    self_table.set("selected_path", LuaValue::Nil)?;
    self_table.set("hovered_path", LuaValue::Nil)?;
    self_table.set("selected_item", LuaValue::Nil)?;
    self_table.set("hovered_item", LuaValue::Nil)?;
    self_table.set("item_icon_width", 0.0f64)?;
    self_table.set("item_chevron_width", 0.0f64)?;
    self_table.set("item_text_spacing", 0.0f64)?;
    self_table.set("items_dirty", true)?;
    self_table.set("last_project_count", 0i64)?;
    self_table.set("last_tree_generation", 0i64)?;
    self_table.set("visible_count", 0i64)?;
    self_table.set("count_lines", 0i64)?;
    self_table.set("model_roots", lua.create_table()?)?;
    self_table.set("model_opts", LuaValue::Nil)?;
    self_table.set("project_roots", lua.create_table()?)?;
    let range_cache = lua.create_table()?;
    range_cache.set("start_row", 0i64)?;
    range_cache.set("end_row", 0i64)?;
    range_cache.set("items", lua.create_table()?)?;
    self_table.set("range_cache", range_cache)?;
    self_table.set("text_width_cache", lua.create_table()?)?;
    self_table.set("label_cache", lua.create_table()?)?;
    self_table.set("label_cache_count", 0i64)?;
    self_table.set("item_font_height", 0.0f64)?;
    self_table.set("icon_font_height", 0.0f64)?;
    Ok(())
}

/// Build roots/opts/project_roots from live Lua globals, call tree_model.sync_roots,
/// and update all view state fields.
fn sync_model(lua: &Lua, self_table: LuaTable) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let projects: LuaTable = core.get("projects")?;
    let pathsep: String = lua
        .globals()
        .get("PATHSEP")
        .unwrap_or_else(|_| "/".to_string());
    let roots = lua.create_table()?;
    let project_roots = lua.create_table()?;
    let mut project_count = 0i64;
    for entry in projects.sequence_values::<LuaTable>() {
        let project = entry?;
        let path: String = project.get("path")?;
        project_count += 1;
        roots.raw_set(project_count, path.clone())?;
        project_roots.set(path.clone(), project.clone())?;
        if pathsep == "\\" {
            project_roots.set(path.replace('\\', "/"), project)?;
        }
    }
    self_table.set("project_roots", project_roots)?;
    let config: LuaTable = require_table(lua, "core.config")?;
    let show_hidden: bool = self_table.get("show_hidden").unwrap_or(false);
    let show_ignored: bool = self_table.get("show_ignored").unwrap_or(true);
    let plugins: LuaTable = config.get("plugins")?;
    let tv_cfg: LuaTable = plugins.get("treeview")?;
    let max_entries: LuaValue = tv_cfg.get("max_dir_entries")?;
    let file_size_limit: f64 = config.get("file_size_limit").unwrap_or(50.0);
    let ignore_files: LuaValue = config.get("ignore_files")?;
    let (gitignore_enabled, gitignore_additional) = match config.get::<LuaTable>("gitignore") {
        Ok(t) => {
            let enabled: LuaValue = t.get("enabled").unwrap_or(LuaValue::Nil);
            let additional: LuaValue = t.get("additional_patterns").unwrap_or(LuaValue::Nil);
            (!matches!(enabled, LuaValue::Boolean(false)), additional)
        }
        Err(_) => (true, LuaValue::Nil),
    };
    let opts = lua.create_table()?;
    opts.set("show_hidden", show_hidden)?;
    opts.set("show_ignored", show_ignored)?;
    opts.set("max_entries", max_entries)?;
    opts.set("file_size_limit_bytes", file_size_limit * 1_000_000.0)?;
    opts.set("ignore_files", ignore_files)?;
    opts.set("gitignore_enabled", gitignore_enabled)?;
    match gitignore_additional {
        LuaValue::Nil => opts.set("gitignore_additional_patterns", lua.create_table()?)?,
        other => opts.set("gitignore_additional_patterns", other)?,
    }
    let tree_model = require_table(lua, "tree_model")?;
    let sync_roots: LuaFunction = tree_model.get("sync_roots")?;
    sync_roots.call::<()>((roots.clone(), opts))?;
    let generation_fn: LuaFunction = tree_model.get("generation")?;
    let tree_gen: i64 = generation_fn.call(())?;
    let visible_count_fn: LuaFunction = tree_model.get("visible_count")?;
    let vis: i64 = visible_count_fn.call(roots.clone())?;
    self_table.set("last_tree_generation", tree_gen)?;
    self_table.set("visible_count", vis)?;
    self_table.set("count_lines", vis)?;
    self_table.set("items_dirty", false)?;
    self_table.set("last_project_count", project_count)?;
    self_table.set("model_roots", roots)?;
    let range_cache = lua.create_table()?;
    range_cache.set("start_row", 0i64)?;
    range_cache.set("end_row", 0i64)?;
    range_cache.set("items", lua.create_table()?)?;
    self_table.set("range_cache", range_cache)?;
    Ok(())
}

/// Attach project info to an item table from project_roots.
fn attach_project(item: &LuaTable, project_roots: &LuaTable) -> LuaResult<()> {
    let project_root: Option<String> = item.get("project_root")?;
    if let Some(root) = project_root {
        let project: LuaValue = project_roots.get(root.as_str())?;
        item.set("project", project)?;
        let name: Option<String> = item.get("name")?;
        if let Some(n) = name {
            item.set("filename", n)?;
        }
    }
    Ok(())
}

/// Fetch a single row; reads model_roots and project_roots from self.
fn get_item_by_row(lua: &Lua, (self_table, row): (LuaTable, i64)) -> LuaResult<LuaValue> {
    let visible_count: i64 = self_table.get("visible_count")?;
    if row < 1 || row > visible_count {
        return Ok(LuaValue::Nil);
    }
    let range_cache: LuaTable = self_table.get("range_cache")?;
    let cache_start: i64 = range_cache.get("start_row")?;
    let cache_end: i64 = range_cache.get("end_row")?;
    if row >= cache_start && row <= cache_end && cache_start > 0 {
        let items: LuaTable = range_cache.get("items")?;
        let idx = row - cache_start + 1;
        return items.raw_get(idx);
    }
    let model_roots: LuaTable = self_table.get("model_roots")?;
    let project_roots: LuaTable = self_table.get("project_roots")?;
    let tree_model = require_table(lua, "tree_model")?;
    let item_at: LuaFunction = tree_model.get("item_at")?;
    let item: LuaValue = item_at.call((model_roots, row))?;
    if let LuaValue::Table(ref t) = item {
        attach_project(t, &project_roots)?;
    }
    Ok(item)
}

/// Fetch a range of rows; reads model_roots and project_roots from self.
fn get_items_in_range(
    lua: &Lua,
    (self_table, start_row, end_row): (LuaTable, i64, i64),
) -> LuaResult<LuaTable> {
    if start_row < 1 || end_row < start_row {
        return lua.create_table();
    }
    let visible_count: i64 = self_table.get("visible_count")?;
    let start_row = start_row.max(1).min(visible_count);
    let end_row = end_row.max(1).min(visible_count);
    let range_cache: LuaTable = self_table.get("range_cache")?;
    let cache_start: i64 = range_cache.get("start_row")?;
    let cache_end: i64 = range_cache.get("end_row")?;
    if start_row == cache_start && end_row == cache_end {
        return range_cache.get("items");
    }
    let model_roots: LuaTable = self_table.get("model_roots")?;
    let project_roots: LuaTable = self_table.get("project_roots")?;
    let tree_model = require_table(lua, "tree_model")?;
    let items_in_range: LuaFunction = tree_model.get("items_in_range")?;
    let items: LuaTable = items_in_range.call((model_roots, start_row, end_row))?;
    for item in items.clone().sequence_values::<LuaTable>() {
        let item = item?;
        attach_project(&item, &project_roots)?;
    }
    range_cache.set("start_row", start_row)?;
    range_cache.set("end_row", end_row)?;
    range_cache.set("items", items.clone())?;
    Ok(items)
}

/// Apply font-derived scale metrics and reset label caches.
///
/// All metric values are computed by the Lua caller using font userdata calls
/// that Rust cannot make directly.
fn apply_scale_metrics(
    lua: &Lua,
    (self_table, icon_width, chevron_width, text_spacing, font_height, icon_font_height): (
        LuaTable,
        f64,
        f64,
        f64,
        f64,
        f64,
    ),
) -> LuaResult<()> {
    self_table.set("item_icon_width", icon_width)?;
    self_table.set("item_chevron_width", chevron_width)?;
    self_table.set("item_text_spacing", text_spacing)?;
    self_table.set("item_font_height", font_height)?;
    self_table.set("icon_font_height", icon_font_height)?;
    self_table.set("text_width_cache", lua.create_table()?)?;
    self_table.set("label_cache", lua.create_table()?)?;
    self_table.set("label_cache_count", 0i64)?;
    Ok(())
}

/// Update hover/tooltip state from mouse-move coordinates.
///
/// `item` is nil when the cursor is outside any row; `in_text_box` indicates
/// whether the cursor overlaps the item label (computed via font calls).
/// `cur_time` is `system.get_time()` passed so Rust avoids that call.
fn update_hover(
    _lua: &Lua,
    (self_table, item, in_text_box, px, py, same_hover, cur_time): (
        LuaTable,
        LuaValue,
        bool,
        f64,
        f64,
        bool,
        f64,
    ),
) -> LuaResult<()> {
    let tooltip: LuaTable = self_table.get("tooltip")?;
    match &item {
        LuaValue::Table(t) => {
            let abs_filename: LuaValue = t.get("abs_filename")?;
            self_table.set("hovered_item", item.clone())?;
            self_table.set("hovered_path", abs_filename)?;
            if in_text_box {
                tooltip.set("x", px)?;
                tooltip.set("y", py)?;
                if !same_hover {
                    tooltip.set("begin", cur_time)?;
                }
            } else {
                tooltip.set("x", LuaValue::Nil)?;
                tooltip.set("y", LuaValue::Nil)?;
            }
        }
        _ => {
            self_table.set("hovered_item", LuaValue::Nil)?;
            self_table.set("hovered_path", LuaValue::Nil)?;
            tooltip.set("x", LuaValue::Nil)?;
            tooltip.set("y", LuaValue::Nil)?;
        }
    }
    Ok(())
}

/// Update selected_item/selected_path and compute scroll target.
///
/// `item_height` is computed by the caller via `self:get_item_height()` so
/// Rust never needs to touch the font userdata.
fn set_selection(
    _lua: &Lua,
    (self_table, selection, selection_y_val, center, instant, item_height): (
        LuaTable,
        LuaValue,
        LuaValue,
        Option<bool>,
        Option<bool>,
        f64,
    ),
) -> LuaResult<()> {
    let selection_y: Option<f64> = match selection_y_val {
        LuaValue::Number(n) => Some(n),
        LuaValue::Integer(n) => Some(n as f64),
        _ => None,
    };
    match &selection {
        LuaValue::Table(t) => {
            let abs_filename: Option<String> = t.get("abs_filename")?;
            self_table.set("selected_item", selection.clone())?;
            self_table.set("selected_path", abs_filename.clone())?;
            if let Some(sel_y) = selection_y {
                let size: LuaTable = self_table.get("size")?;
                let size_y: f64 = size.get("y")?;
                let lh = item_height.max(1.0);
                if sel_y <= 0.0 || sel_y >= size_y {
                    let mut scroll_y = sel_y;
                    let is_center = center.unwrap_or(false);
                    if !is_center && sel_y >= size_y - lh {
                        scroll_y = sel_y - size_y + lh;
                    }
                    if is_center {
                        scroll_y = sel_y - (size_y - lh) / 2.0;
                    }
                    let scroll: LuaTable = self_table.get("scroll")?;
                    let scroll_to: LuaTable = scroll.get("to")?;
                    let count_lines: i64 = self_table.get("count_lines").unwrap_or(0);
                    let max_scroll = (count_lines as f64 + 1.0) * lh - size_y;
                    let scroll_val = scroll_y.max(0.0).min(max_scroll.max(0.0));
                    scroll_to.set("y", scroll_val)?;
                    if instant.unwrap_or(false) {
                        scroll.set("y", scroll_val)?;
                    }
                }
            }
        }
        LuaValue::Nil => {
            self_table.set("selected_item", LuaValue::Nil)?;
            self_table.set("selected_path", LuaValue::Nil)?;
        }
        _ => {}
    }
    Ok(())
}

fn make_native_module(lua: &Lua) -> LuaResult<LuaTable> {
    let m = lua.create_table()?;
    m.set(
        "init",
        lua.create_function(|lua, self_table: LuaTable| init(lua, self_table))?,
    )?;
    m.set(
        "sync_model",
        lua.create_function(|lua, self_table: LuaTable| sync_model(lua, self_table))?,
    )?;
    m.set("get_item_by_row", lua.create_function(get_item_by_row)?)?;
    m.set(
        "get_items_in_range",
        lua.create_function(get_items_in_range)?,
    )?;
    m.set(
        "apply_scale_metrics",
        lua.create_function(apply_scale_metrics)?,
    )?;
    m.set("update_hover", lua.create_function(update_hover)?)?;
    m.set("set_selection", lua.create_function(set_selection)?)?;
    Ok(m)
}

// ── Constants ────────────────────────────────────────────────────────────────

const TOOLTIP_BORDER: f64 = 1.0;
const TOOLTIP_DELAY: f64 = 0.5;
const TOOLTIP_ALPHA: f64 = 255.0;
const TOOLTIP_ALPHA_RATE: f64 = 1.0;
const SEPARATOR_INSET: f64 = 10.0;
const LABEL_CACHE_MAX: i64 = 3000;

/// Registers `plugins.treeview` as a pure-Rust preload.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    let native_key = lua.create_registry_value(make_native_module(lua)?)?;
    preload.set(
        "treeview_native",
        lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&native_key))?,
    )?;
    preload.set(
        "plugins.treeview",
        lua.create_function(|lua, ()| build_treeview_plugin(lua))?,
    )?;
    Ok(())
}

/// Build the entire treeview plugin in Rust, returning the view instance.
fn build_treeview_plugin(lua: &Lua) -> LuaResult<LuaValue> {
    let core = require_table(lua, "core")?;
    let common = require_table(lua, "core.common")?;
    let command = require_table(lua, "core.command")?;
    let config: LuaTable = require_table(lua, "core.config")?;
    let keymap = require_table(lua, "core.keymap")?;
    let view_class: LuaTable = require_table(lua, "core.view")?;

    // config.plugins.treeview defaults
    let plugins: LuaTable = config.get("plugins")?;
    let scale: f64 = lua.globals().get("SCALE")?;

    // common.merge existing config with defaults
    let defaults = lua.create_table()?;
    defaults.set("size", 200.0 * scale)?;
    defaults.set("highlight_focused_file", true)?;
    defaults.set("expand_dirs_to_focused_file", false)?;
    defaults.set("scroll_to_focused_file", false)?;
    defaults.set("animate_scroll_to_focused_file", true)?;
    defaults.set("show_hidden", false)?;
    defaults.set("show_ignored", true)?;
    defaults.set("visible", true)?;
    defaults.set("max_dir_entries", 5000)?;

    let existing: LuaValue = plugins.get("treeview")?;
    let merge: LuaFunction = common.get("merge")?;
    let tv_cfg: LuaTable = merge.call((defaults, existing))?;
    plugins.set("treeview", tv_cfg.clone())?;

    // TreeView = View:extend()
    let tree_view = view_class.call_method::<LuaTable>("extend", ())?;
    tree_view.set(
        "__tostring",
        lua.create_function(|_lua, _self: LuaTable| Ok("TreeView"))?,
    )?;

    // Compute icon_vertical_nudge at load time
    let round: LuaFunction = common.get("round")?;
    let icon_vertical_nudge: f64 = round.call(1.0 * scale)?;

    let class_key = Arc::new(lua.create_registry_value(tree_view.clone())?);

    // TreeView:new()
    tree_view.set("new", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_new: LuaFunction = super_tbl.get("new")?;
            super_new.call::<()>(this.clone())?;
            let native: LuaTable = require_table(lua, "treeview_native")?;
            let init_fn: LuaFunction = native.get("init")?;
            init_fn.call::<()>(this)?;
            Ok(())
        })?
    })?;

    // TreeView:set_target_size(axis, value)
    tree_view.set(
        "set_target_size",
        lua.create_function(|lua, (this, axis, value): (LuaTable, String, f64)| {
            if axis == "x" {
                let scale: f64 = lua.globals().get("SCALE")?;
                let common: LuaTable = require_table(lua, "core.common")?;
                let round: LuaFunction = common.get("round")?;
                let rounded: f64 = round.call(value)?;
                let target = (140.0 * scale).max(rounded);
                this.set("target_size", target)?;
                let config: LuaTable = require_table(lua, "core.config")?;
                let plugins: LuaTable = config.get("plugins")?;
                let tv_cfg: LuaTable = plugins.get("treeview")?;
                tv_cfg.set("size", target)?;
                let core: LuaTable = require_table(lua, "core")?;
                if let LuaValue::Table(session) = core.get::<LuaValue>("session")? {
                    session.set("treeview_size", target)?;
                }
                Ok(LuaValue::Boolean(true))
            } else {
                Ok(LuaValue::Nil)
            }
        })?,
    )?;

    // TreeView:get_name()
    tree_view.set(
        "get_name",
        lua.create_function(|_lua, _this: LuaTable| Ok(LuaValue::Nil))?,
    )?;

    // TreeView:get_item_height()
    tree_view.set(
        "get_item_height",
        lua.create_function(|lua, _this: LuaTable| {
            let style: LuaTable = require_table(lua, "core.style")?;
            let font: LuaValue = style.get("font")?;
            let font_h: f64 = match &font {
                LuaValue::Table(t) => t.call_method("get_height", ())?,
                LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                _ => 14.0,
            };
            let padding: LuaTable = style.get("padding")?;
            let pad_y: f64 = padding.get("y")?;
            Ok(font_h + pad_y)
        })?,
    )?;

    // TreeView:each_item() - stateful iterator (no coroutine.yield)
    tree_view.set(
        "each_item",
        lua.create_function(|lua, this: LuaTable| {
            let (ox, oy): (f64, f64) = this.call_method("get_content_offset", ())?;
            let h: f64 = this.call_method("get_item_height", ())?;
            this.call_method::<()>("sync_model", ())?;
            let visible_count: i64 = this.get("visible_count")?;
            this.set("count_lines", visible_count)?;
            let style: LuaTable = require_table(lua, "core.style")?;
            let padding: LuaTable = style.get("padding")?;
            let pad_y: f64 = padding.get("y")?;
            let size_x: f64 = {
                let size: LuaTable = this.get("size")?;
                size.get("x")?
            };

            // Pre-collect all items
            let results = lua.create_table()?;
            for i in 1..=visible_count {
                let item: LuaValue = this.call_method("get_item_by_row", i)?;
                let entry = lua.create_table()?;
                entry.raw_set(1, item)?;
                entry.raw_set(2, ox)?;
                entry.raw_set(3, oy + pad_y + h * (i - 1) as f64)?;
                entry.raw_set(4, size_x)?;
                entry.raw_set(5, h)?;
                results.raw_set(i, entry)?;
            }

            let state = lua.create_table()?;
            state.set("idx", 0i64)?;
            state.set("len", visible_count)?;
            let results_key = lua.create_registry_value(results)?;

            let iterator =
                lua.create_function(move |lua, ()| -> LuaResult<LuaMultiValue> {
                    let idx: i64 = state.get("idx")?;
                    let len: i64 = state.get("len")?;
                    let next_idx = idx + 1;
                    if next_idx > len {
                        return Ok(LuaMultiValue::new());
                    }
                    state.set("idx", next_idx)?;
                    let results: LuaTable = lua.registry_value(&results_key)?;
                    let entry: LuaTable = results.raw_get(next_idx)?;
                    let item: LuaValue = entry.raw_get(1)?;
                    let ex: LuaValue = entry.raw_get(2)?;
                    let ey: LuaValue = entry.raw_get(3)?;
                    let ew: LuaValue = entry.raw_get(4)?;
                    let eh: LuaValue = entry.raw_get(5)?;
                    Ok(LuaMultiValue::from_vec(vec![item, ex, ey, ew, eh]))
                })?;
            Ok(iterator)
        })?,
    )?;

    // TreeView:sync_model()
    tree_view.set(
        "sync_model",
        lua.create_function(|lua, this: LuaTable| {
            let items_dirty: bool = this.get("items_dirty").unwrap_or(false);
            let core: LuaTable = require_table(lua, "core")?;
            let projects: LuaTable = core.get("projects")?;
            let project_count = projects.raw_len() as i64;
            let last_count: i64 = this.get("last_project_count").unwrap_or(0);
            if !items_dirty && project_count == last_count {
                return Ok(());
            }
            let native: LuaTable = require_table(lua, "treeview_native")?;
            let sync_fn: LuaFunction = native.get("sync_model")?;
            sync_fn.call(this)
        })?,
    )?;

    // TreeView:get_item_by_row(row)
    tree_view.set(
        "get_item_by_row",
        lua.create_function(|lua, (this, row): (LuaTable, i64)| {
            let native: LuaTable = require_table(lua, "treeview_native")?;
            let f: LuaFunction = native.get("get_item_by_row")?;
            f.call::<LuaValue>((this, row))
        })?,
    )?;

    // TreeView:get_items_in_range(start_row, end_row)
    tree_view.set(
        "get_items_in_range",
        lua.create_function(|lua, (this, start_row, end_row): (LuaTable, i64, i64)| {
            let native: LuaTable = require_table(lua, "treeview_native")?;
            let f: LuaFunction = native.get("get_items_in_range")?;
            f.call::<LuaTable>((this, start_row, end_row))
        })?,
    )?;

    // TreeView:resolve_path(path)
    tree_view.set(
        "resolve_path",
        lua.create_function(|lua, (this, path): (LuaTable, Option<String>)| -> LuaResult<LuaMultiValue> {
            let path = match path {
                Some(p) => p,
                None => return Ok(LuaMultiValue::new()),
            };
            this.call_method::<()>("sync_model", ())?;
            let model_roots: LuaTable = this.get("model_roots")?;
            let tree_model: LuaTable = require_table(lua, "tree_model")?;
            let get_row: LuaFunction = tree_model.get("get_row")?;
            let idx: LuaValue = get_row.call((model_roots, path))?;
            let idx_opt: Option<i64> = match idx {
                LuaValue::Integer(n) => Some(n),
                LuaValue::Number(n) => Some(n as i64),
                _ => None,
            };
            if let Some(idx) = idx_opt {
                let (ox, oy): (f64, f64) = this.call_method("get_content_offset", ())?;
                let h: f64 = this.call_method("get_item_height", ())?;
                let style: LuaTable = require_table(lua, "core.style")?;
                let padding: LuaTable = style.get("padding")?;
                let pad_y: f64 = padding.get("y")?;
                let y = oy + pad_y + h * (idx - 1) as f64;
                let size: LuaTable = this.get("size")?;
                let size_x: f64 = size.get("x")?;
                let item: LuaValue = this.call_method("get_item_by_row", idx)?;
                Ok(LuaMultiValue::from_vec(vec![
                    item,
                    LuaValue::Number(ox),
                    LuaValue::Number(y),
                    LuaValue::Number(size_x),
                    LuaValue::Number(h),
                ]))
            } else {
                Ok(LuaMultiValue::new())
            }
        })?,
    )?;

    // TreeView:get_selected_item()
    tree_view.set(
        "get_selected_item",
        lua.create_function(|_lua, this: LuaTable| -> LuaResult<LuaValue> {
            let selected_path: LuaValue = this.get("selected_path")?;
            if let LuaValue::String(ref path) = selected_path {
                let path_str = path.to_str()?.to_string();
                let result: LuaMultiValue = this.call_method("resolve_path", path_str)?;
                let item = result.into_iter().next().unwrap_or(LuaValue::Nil);
                if matches!(item, LuaValue::Table(_)) {
                    this.set("selected_item", item.clone())?;
                    return Ok(item);
                }
                this.set("selected_item", LuaValue::Nil)?;
                this.set("selected_path", LuaValue::Nil)?;
            }
            this.get("selected_item")
        })?,
    )?;

    // TreeView:get_hovered_item()
    tree_view.set(
        "get_hovered_item",
        lua.create_function(|_lua, this: LuaTable| -> LuaResult<LuaValue> {
            let hovered_path: LuaValue = this.get("hovered_path")?;
            if let LuaValue::String(ref path) = hovered_path {
                let path_str = path.to_str()?.to_string();
                let result: LuaMultiValue = this.call_method("resolve_path", path_str)?;
                let item = result.into_iter().next().unwrap_or(LuaValue::Nil);
                if matches!(item, LuaValue::Table(_)) {
                    this.set("hovered_item", item.clone())?;
                    return Ok(item);
                }
                this.set("hovered_item", LuaValue::Nil)?;
                this.set("hovered_path", LuaValue::Nil)?;
            }
            this.get("hovered_item")
        })?,
    )?;

    // TreeView:set_selection(selection, selection_y, center, instant)
    tree_view.set(
        "set_selection",
        lua.create_function(|lua, (this, selection, selection_y, center, instant): (LuaTable, LuaValue, LuaValue, Option<bool>, Option<bool>)| {
            let h: f64 = if matches!(selection, LuaValue::Table(_)) && !matches!(selection_y, LuaValue::Nil | LuaValue::Boolean(false)) {
                this.call_method("get_item_height", ())?
            } else {
                0.0
            };
            let native: LuaTable = require_table(lua, "treeview_native")?;
            let set_sel: LuaFunction = native.get("set_selection")?;
            set_sel.call::<()>((this, selection, selection_y, center, instant, h))
        })?,
    )?;

    // TreeView:set_selection_to_path(path, expand, scroll_to, instant)
    tree_view.set(
        "set_selection_to_path",
        lua.create_function(|lua, (this, path, expand, scroll_to, instant): (LuaTable, String, Option<bool>, Option<bool>, Option<bool>)| -> LuaResult<LuaValue> {
            if expand.unwrap_or(false) {
                let tree_model: LuaTable = require_table(lua, "tree_model")?;
                let expand_to: LuaFunction = tree_model.get("expand_to")?;
                expand_to.call::<()>(path.clone())?;
                this.set("items_dirty", true)?;
            }
            this.call_method::<()>("sync_model", ())?;
            let result: LuaMultiValue = this.call_method("resolve_path", path)?;
            let mut vals = result.into_iter();
            let to_select = vals.next().unwrap_or(LuaValue::Nil);
            let _ = vals.next(); // ox
            let to_select_y = vals.next().unwrap_or(LuaValue::Nil);
            if matches!(to_select, LuaValue::Table(_)) {
                let sel_y = if scroll_to.unwrap_or(false) {
                    to_select_y
                } else {
                    LuaValue::Boolean(false)
                };
                this.call_method::<()>("set_selection", (to_select.clone(), sel_y, true, instant))?;
            }
            Ok(to_select)
        })?,
    )?;

    // TreeView:get_text_bounding_box(item, x, y, w, h)
    tree_view.set(
        "get_text_bounding_box",
        lua.create_function(
            |lua, (this, item, x, y, _w, h): (LuaTable, LuaTable, f64, f64, f64, f64)| {
                let style: LuaTable = require_table(lua, "core.style")?;
                let padding: LuaTable = style.get("padding")?;
                let pad_x: f64 = padding.get("x")?;
                let depth: f64 = item.get("depth")?;
                let chevron_w: f64 = this.get("item_chevron_width")?;
                let icon_w: f64 = this.get("item_icon_width")?;
                let text_spacing: f64 = this.get("item_text_spacing")?;
                let xoffset = depth * pad_x + pad_x + chevron_w + icon_w + text_spacing;
                let new_x = x + xoffset;
                let abs_filename: String = item.get("abs_filename")?;
                let name: String = item.get("name")?;
                let size: LuaTable = this.get("size")?;
                let size_x: f64 = size.get("x")?;
                let cache: LuaTable =
                    this.call_method("get_label_cache", (abs_filename, name, size_x))?;
                let width_cache: LuaTable = cache.get("width_cache")?;
                let text_width: f64 = width_cache.get("width")?;
                let w = text_width + 2.0 * pad_x;
                Ok((new_x, y, w, h))
            },
        )?,
    )?;

    // TreeView:get_label_cache(path, text, avail_width)
    tree_view.set(
        "get_label_cache",
        lua.create_function(
            |lua, (this, path, text, avail_width): (LuaTable, String, String, Option<f64>)| {
                let width_key = (avail_width.unwrap_or(0.0).max(0.0)).floor() as i64;
                let label_cache: LuaTable = this.get("label_cache")?;
                let cached: LuaValue = label_cache.get(path.as_str())?;
                if let LuaValue::Table(ref c) = cached {
                    let cached_text: Option<String> = c.get("text")?;
                    let cached_wk: Option<i64> = c.get("width_key")?;
                    if cached_text.as_deref() == Some(text.as_str())
                        && cached_wk == Some(width_key)
                    {
                        return Ok(c.clone());
                    }
                }

                let style: LuaTable = require_table(lua, "core.style")?;
                let font: LuaValue = style.get("font")?;
                let full_width: f64 = match &font {
                    LuaValue::Table(t) => t.call_method("get_width", text.clone())?,
                    LuaValue::UserData(ud) => ud.call_method("get_width", text.clone())?,
                    _ => 0.0,
                };
                let display_text = if full_width > width_key as f64 && width_key > 0 {
                    let dots = "\u{2026}";
                    let padding: LuaTable = style.get("padding")?;
                    let pad_x: f64 = padding.get("x")?;
                    let max_w = (width_key as f64 - pad_x).max(0.0);
                    let text_bytes = text.as_bytes();
                    let mut low = 0usize;
                    let mut high = text_bytes.len();
                    while low < high {
                        let mid = (low + high).div_ceil(2);
                        let candidate = format!("{}{}", &text[..mid], dots);
                        let cw: f64 = match &font {
                            LuaValue::Table(t) => t.call_method("get_width", candidate)?,
                            LuaValue::UserData(ud) => ud.call_method("get_width", candidate)?,
                            _ => 0.0,
                        };
                        if cw <= max_w {
                            low = mid;
                        } else {
                            high = mid - 1;
                        }
                    }
                    if low > 0 {
                        format!("{}{}", &text[..low], dots)
                    } else {
                        dots.to_string()
                    }
                } else {
                    text.clone()
                };

                let new_cache = lua.create_table()?;
                new_cache.set("text", text.as_str())?;
                new_cache.set("width_key", width_key)?;
                new_cache.set("display_text", display_text.as_str())?;
                let width_cache_entry = lua.create_table()?;
                width_cache_entry.set("name", text.as_str())?;
                width_cache_entry.set("width", full_width)?;
                new_cache.set("width_cache", width_cache_entry.clone())?;

                let cache_count: i64 = this.get("label_cache_count").unwrap_or(0);
                if cache_count >= LABEL_CACHE_MAX {
                    this.set("label_cache", lua.create_table()?)?;
                    this.set("text_width_cache", lua.create_table()?)?;
                    this.set("label_cache_count", 0i64)?;
                }
                let label_cache: LuaTable = this.get("label_cache")?;
                let text_width_cache: LuaTable = this.get("text_width_cache")?;
                label_cache.set(path.as_str(), new_cache.clone())?;
                text_width_cache.set(path.as_str(), width_cache_entry)?;
                let new_count: i64 = this.get::<i64>("label_cache_count").unwrap_or(0) + 1;
                this.set("label_cache_count", new_count)?;
                Ok(new_cache)
            },
        )?,
    )?;

    // TreeView:on_mouse_moved(px, py, ...)
    tree_view.set("on_mouse_moved", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, (this, px, py, args): (LuaTable, f64, f64, LuaMultiValue)| {
            let visible: bool = this.get("visible").unwrap_or(false);
            if !visible { return Ok(()); }
            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_fn: LuaFunction = super_tbl.get("on_mouse_moved")?;
            let mut call_args = vec![LuaValue::Table(this.clone()), LuaValue::Number(px), LuaValue::Number(py)];
            call_args.extend(args);
            let result: LuaValue = super_fn.call(LuaMultiValue::from_vec(call_args))?;
            if !matches!(result, LuaValue::Nil | LuaValue::Boolean(false)) {
                this.set("hovered_item", LuaValue::Nil)?;
                this.set("hovered_path", LuaValue::Nil)?;
                return Ok(());
            }
            this.call_method::<()>("sync_model", ())?;
            let (ox, oy): (f64, f64) = this.call_method("get_content_offset", ())?;
            let h: f64 = this.call_method("get_item_height", ())?;
            let style: LuaTable = require_table(lua, "core.style")?;
            let padding: LuaTable = style.get("padding")?;
            let pad: f64 = padding.get("y")?;
            let row = ((py - oy - pad) / h).floor() as i64;
            let item: LuaValue = if row >= 0 {
                this.call_method("get_item_by_row", row + 1)?
            } else {
                LuaValue::Nil
            };
            let (item, in_text_box, same_hover) = if let LuaValue::Table(ref t) = item {
                let size: LuaTable = this.get("size")?;
                let size_x: f64 = size.get("x")?;
                if px > ox && px <= ox + size_x {
                    let abs_filename: String = t.get("abs_filename")?;
                    let hovered_path: LuaValue = this.get("hovered_path")?;
                    let same = match hovered_path {
                        LuaValue::String(ref s) => s.to_str().map(|s| s == abs_filename).unwrap_or(false),
                        _ => false,
                    };
                    let (ix, iy, iw, ih): (f64, f64, f64, f64) = this.call_method(
                        "get_text_bounding_box",
                        (t.clone(), ox, oy + pad + row as f64 * h, size_x, h),
                    )?;
                    let in_box = px > ix && py > iy && px <= ix + iw && py <= iy + ih;
                    (item, in_box, same)
                } else {
                    (LuaValue::Nil, false, false)
                }
            } else {
                (LuaValue::Nil, false, false)
            };
            let native: LuaTable = require_table(lua, "treeview_native")?;
            let update_hover: LuaFunction = native.get("update_hover")?;
            let system: LuaTable = lua.globals().get("system")?;
            let get_time: LuaFunction = system.get("get_time")?;
            let cur_time: f64 = get_time.call(())?;
            update_hover.call((this, item, in_text_box, px, py, same_hover, cur_time))
        })?
    })?;

    // TreeView:on_mouse_left()
    tree_view.set("on_mouse_left", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_fn: LuaFunction = super_tbl.get("on_mouse_left")?;
            super_fn.call::<()>(this.clone())?;
            this.set("hovered_item", LuaValue::Nil)?;
            this.set("hovered_path", LuaValue::Nil)?;
            Ok(())
        })?
    })?;

    // TreeView:update()
    tree_view.set("update", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let visible: bool = this.get("visible").unwrap_or(false);
            let target_size: f64 = this.get("target_size").unwrap_or(0.0);
            let dest = if visible { target_size } else { 0.0 };
            let init_size: bool = this.get("init_size").unwrap_or(false);
            if init_size {
                let size: LuaTable = this.get("size")?;
                size.set("x", dest)?;
                this.set("init_size", false)?;
            } else {
                let size: LuaTable = this.get("size")?;
                this.call_method::<()>("move_towards", (size, "x", dest, 0.35, "treeview"))?;
            }

            let size: LuaTable = this.get("size")?;
            let size_x: f64 = size.get("x")?;
            let size_y: f64 = size.get("y")?;
            if size_x == 0.0 || size_y == 0.0 || !visible {
                return Ok(());
            }

            let system: LuaTable = lua.globals().get("system")?;
            let get_time: LuaFunction = system.get("get_time")?;
            let cur_time: f64 = get_time.call(())?;
            let tooltip: LuaTable = this.get("tooltip")?;
            let tooltip_begin: f64 = tooltip.get("begin")?;
            let duration = cur_time - tooltip_begin;
            let hovered_path: LuaValue = this.get("hovered_path")?;
            let tooltip_x: LuaValue = tooltip.get("x")?;
            if !matches!(hovered_path, LuaValue::Nil) && !matches!(tooltip_x, LuaValue::Nil) && duration > TOOLTIP_DELAY {
                this.call_method::<()>("move_towards", (tooltip.clone(), "alpha", TOOLTIP_ALPHA, TOOLTIP_ALPHA_RATE, "treeview"))?;
            } else {
                tooltip.set("alpha", 0.0)?;
            }

            let scroll: LuaTable = this.get("scroll")?;
            let scroll_y: f64 = scroll.get("y")?;
            let last_scroll_y: f64 = this.get("last_scroll_y").unwrap_or(0.0);
            let dy = (last_scroll_y - scroll_y).abs();
            if dy > 0.0 {
                let core: LuaTable = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let mouse: LuaTable = root_view.get("mouse")?;
                let mx: f64 = mouse.get("x")?;
                let my: f64 = mouse.get("y")?;
                this.call_method::<()>("on_mouse_moved", (mx, my, 0, 0))?;
                this.set("last_scroll_y", scroll_y)?;
            }

            let config: LuaTable = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let cfg: LuaTable = plugins.get("treeview")?;
            let tree_model: LuaTable = require_table(lua, "tree_model")?;
            let generation_fn: LuaFunction = tree_model.get("generation")?;
            let cur_gen: i64 = generation_fn.call(())?;
            let last_gen: i64 = this.get("last_tree_generation").unwrap_or(0);
            if cur_gen != last_gen {
                this.set("items_dirty", true)?;
            }

            let highlight: bool = cfg.get("highlight_focused_file").unwrap_or(true);
            if highlight {
                let core: LuaTable = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let current_node: LuaValue = root_view.call_method("get_active_node", ())?;
                let active_view: LuaTable = core.get("active_view")?;
                if let LuaValue::Table(ref node) = current_node {
                    let locked: bool = node.get("locked").unwrap_or(false);
                    if !locked {
                        let is_self: bool = active_view == this;
                        let last_active: LuaValue = this.get("last_active_view")?;
                        let same_as_last = match (&last_active, &LuaValue::Table(active_view.clone())) {
                            (LuaValue::Table(a), LuaValue::Table(b)) => a == b,
                            _ => false,
                        };
                        if !is_self && !same_as_last {
                            this.set("last_active_view", active_view.clone())?;
                            let doc_view: LuaTable = require_table(lua, "core.docview")?;
                            let is_docview: bool = doc_view.call_method("is_extended_by", active_view.clone())?;
                            if is_docview {
                                let doc: LuaValue = active_view.get("doc")?;
                                let abs_filename: String = if let LuaValue::Table(ref d) = doc {
                                    d.get("abs_filename").unwrap_or_default()
                                } else {
                                    String::new()
                                };
                                let expand: bool = cfg.get("expand_dirs_to_focused_file").unwrap_or(false);
                                let scroll_to: bool = cfg.get("scroll_to_focused_file").unwrap_or(false);
                                let animate: bool = cfg.get("animate_scroll_to_focused_file").unwrap_or(true);
                                this.call_method::<LuaValue>("set_selection_to_path", (abs_filename, expand, scroll_to, !animate))?;
                            } else {
                                this.call_method::<()>("set_selection", (LuaValue::Nil,))?;
                            }
                        }
                    }
                }
            }

            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_update: LuaFunction = super_tbl.get("update")?;
            super_update.call(this)
        })?
    })?;

    // TreeView:on_scale_change()
    tree_view.set(
        "on_scale_change",
        lua.create_function(|lua, this: LuaTable| {
            let style: LuaTable = require_table(lua, "core.style")?;
            let icon_font: LuaValue = style.get("icon_font")?;
            let font: LuaValue = style.get("font")?;
            let padding: LuaTable = style.get("padding")?;
            let pad_x: f64 = padding.get("x")?;

            let get_width = |f: &LuaValue, s: &str| -> LuaResult<f64> {
                match f {
                    LuaValue::Table(t) => t.call_method("get_width", s),
                    LuaValue::UserData(ud) => ud.call_method("get_width", s),
                    _ => Ok(14.0),
                }
            };
            let get_height = |f: &LuaValue| -> LuaResult<f64> {
                match f {
                    LuaValue::Table(t) => t.call_method("get_height", ()),
                    LuaValue::UserData(ud) => ud.call_method("get_height", ()),
                    _ => Ok(14.0),
                }
            };

            let w_d_upper: f64 = get_width(&icon_font, "D")?;
            let w_d_lower: f64 = get_width(&icon_font, "d")?;
            let w_f: f64 = get_width(&icon_font, "f")?;
            let icon_w = w_d_upper.max(w_d_lower).max(w_f);

            let w_plus: f64 = get_width(&icon_font, "+")?;
            let w_minus: f64 = get_width(&icon_font, "-")?;
            let chev_w = w_plus.max(w_minus).max(pad_x);

            let text_spacing = pad_x.max((icon_w * 0.4).ceil());
            let font_h: f64 = get_height(&font)?;
            let icon_font_h: f64 = get_height(&icon_font)?;

            let native: LuaTable = require_table(lua, "treeview_native")?;
            let apply_fn: LuaFunction = native.get("apply_scale_metrics")?;
            apply_fn.call::<()>((this, icon_w, chev_w, text_spacing, font_h, icon_font_h))
        })?,
    )?;

    // TreeView:get_scrollable_size()
    tree_view.set(
        "get_scrollable_size",
        lua.create_function(|_lua, this: LuaTable| {
            let count_lines: LuaValue = this.get("count_lines")?;
            if matches!(count_lines, LuaValue::Nil) {
                return Ok(f64::MAX);
            }
            let count: f64 = match count_lines {
                LuaValue::Integer(n) => n as f64,
                LuaValue::Number(n) => n,
                _ => return Ok(f64::MAX),
            };
            let h: f64 = this.call_method("get_item_height", ())?;
            Ok(h * (count + 1.0))
        })?,
    )?;

    // TreeView:draw_tooltip()
    tree_view.set(
        "draw_tooltip",
        lua.create_function(|lua, this: LuaTable| {
            let hovered: LuaValue = this.call_method("get_hovered_item", ())?;
            let hovered = match hovered {
                LuaValue::Table(t) => t,
                _ => return Ok(()),
            };
            let common: LuaTable = require_table(lua, "core.common")?;
            let style: LuaTable = require_table(lua, "core.style")?;
            let renderer: LuaTable = lua.globals().get("renderer")?;
            let core: LuaTable = require_table(lua, "core")?;

            let abs_filename: String = hovered.get("abs_filename")?;
            let text: String = common.call_function("home_encode", abs_filename.clone())?;
            let font: LuaValue = style.get("font")?;
            let w: f64 = match &font {
                LuaValue::Table(t) => t.call_method("get_width", text.clone())?,
                LuaValue::UserData(ud) => ud.call_method("get_width", text.clone())?,
                _ => 0.0,
            };
            let h: f64 = match &font {
                LuaValue::Table(t) => t.call_method("get_height", text.clone())?,
                LuaValue::UserData(ud) => ud.call_method("get_height", text.clone())?,
                _ => 14.0,
            };

            let resolve_result: LuaMultiValue = this.call_method("resolve_path", abs_filename)?;
            let mut rvals = resolve_result.into_iter();
            let _ = rvals.next(); // item
            let _ = rvals.next(); // ox
            let row_y_val = rvals.next().unwrap_or(LuaValue::Nil);

            let tooltip: LuaTable = this.get("tooltip")?;
            let tooltip_font_offset: f64 = match &font {
                LuaValue::Table(t) => t.call_method("get_height", ())?,
                LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                _ => 14.0,
            };
            let tooltip_x: f64 = tooltip.get("x")?;
            let tooltip_y: f64 = tooltip.get("y")?;
            let padding: LuaTable = style.get("padding")?;
            let pad_x: f64 = padding.get("x")?;
            let pad_y: f64 = padding.get("y")?;

            let row_y: f64 = match row_y_val {
                LuaValue::Number(n) => n,
                LuaValue::Integer(n) => n as f64,
                _ => tooltip_y,
            };
            let row_h: f64 = this.call_method("get_item_height", ())?;

            let mut x = tooltip_x + tooltip_font_offset;
            let mut y = tooltip_y + tooltip_font_offset;
            let w = w + pad_x;
            let h = h + pad_y;

            let root_view: LuaTable = core.get("root_view")?;
            let root_node: LuaTable = root_view.get("root_node")?;
            let root_size: LuaTable = root_node.get("size")?;
            let root_w: f64 = root_size.get("x")?;
            let root_h: f64 = root_size.get("y")?;

            if x + w > root_w - pad_x {
                x = tooltip_x - w - tooltip_font_offset;
            }
            if x < pad_x {
                x = pad_x;
            }
            if y >= row_y && y <= row_y + row_h {
                y = row_y - h - tooltip_font_offset;
            }
            if y < pad_x {
                y = (root_h - h - pad_y).min(row_y + row_h + tooltip_font_offset);
            }

            let tooltip_alpha: f64 = tooltip.get("alpha")?;
            let style_text: LuaTable = style.get("text")?;
            let style_bg2: LuaTable = style.get("background2")?;

            // replace_alpha helper
            let replace_alpha = |color: &LuaTable, alpha: f64| -> LuaResult<LuaTable> {
                let r: LuaValue = color.raw_get(1)?;
                let g: LuaValue = color.raw_get(2)?;
                let b: LuaValue = color.raw_get(3)?;
                let result = lua.create_table()?;
                result.raw_set(1, r)?;
                result.raw_set(2, g)?;
                result.raw_set(3, b)?;
                result.raw_set(4, alpha)?;
                Ok(result)
            };

            let border_color = replace_alpha(&style_text, tooltip_alpha)?;
            let bg_color = replace_alpha(&style_bg2, tooltip_alpha)?;
            let text_color = replace_alpha(&style_text, tooltip_alpha)?;

            let bx = x - TOOLTIP_BORDER;
            let by = y - TOOLTIP_BORDER;
            let bw = w + 2.0 * TOOLTIP_BORDER;
            let bh = h + 2.0 * TOOLTIP_BORDER;

            let draw_rect: LuaFunction = renderer.get("draw_rect")?;
            draw_rect.call::<()>((bx, by, bw, bh, border_color))?;
            draw_rect.call::<()>((x, y, w, h, bg_color))?;
            common.call_function::<LuaValue>("draw_text", (font, text_color, text, "center", x, y, w, h))?;
            Ok(())
        })?,
    )?;

    // TreeView:get_item_icon(item, active, hovered)
    tree_view.set(
        "get_item_icon",
        lua.create_function(
            |lua, (_this, item, active, hovered): (LuaTable, LuaTable, bool, bool)| {
                let item_type: String = item.get("type")?;
                let character = if item_type == "dir" {
                    let expanded: bool = item.get("expanded").unwrap_or(false);
                    if expanded { "D" } else { "d" }
                } else {
                    "f"
                };
                let style: LuaTable = require_table(lua, "core.style")?;
                let font: LuaValue = style.get("icon_font")?;
                let color: LuaValue = if active || hovered {
                    style.get("accent")?
                } else {
                    let ignored: bool = item.get("ignored").unwrap_or(false);
                    if ignored {
                        style.get("dim")?
                    } else {
                        style.get("text")?
                    }
                };
                Ok((character, font, color))
            },
        )?,
    )?;

    // TreeView:get_item_text(item, active, hovered)
    tree_view.set(
        "get_item_text",
        lua.create_function(
            |lua, (this, item, active, hovered): (LuaTable, LuaTable, bool, bool)| {
                let size: LuaTable = this.get("size")?;
                let size_x: f64 = size.get("x")?;
                let style: LuaTable = require_table(lua, "core.style")?;
                let padding: LuaTable = style.get("padding")?;
                let pad_x: f64 = padding.get("x")?;
                let depth: f64 = item.get("depth")?;
                let chevron_w: f64 = this.get("item_chevron_width")?;
                let icon_w: f64 = this.get("item_icon_width")?;
                let text_spacing: f64 = this.get("item_text_spacing")?;
                let available_width =
                    size_x - (depth * pad_x + pad_x * 2.0 + chevron_w + icon_w + text_spacing);
                let abs_filename: String = item.get("abs_filename")?;
                let name: String = item.get("name")?;
                let cache: LuaTable =
                    this.call_method("get_label_cache", (abs_filename, name, available_width))?;
                let text: String = cache.get("display_text")?;
                let font: LuaValue = style.get("font")?;
                let color: LuaValue = if active || hovered {
                    style.get("accent")?
                } else {
                    let ignored: bool = item.get("ignored").unwrap_or(false);
                    if ignored {
                        style.get("dim")?
                    } else {
                        style.get("text")?
                    }
                };
                Ok((text, font, color))
            },
        )?,
    )?;

    // TreeView:draw_item_text(item, active, hovered, x, y, w, h)
    tree_view.set(
        "draw_item_text",
        lua.create_function(
            |lua,
             (this, item, active, hovered, x, y, _w, h): (
                LuaTable,
                LuaTable,
                bool,
                bool,
                f64,
                f64,
                f64,
                f64,
            )| {
                let (text, font, color): (String, LuaValue, LuaValue) =
                    this.call_method("get_item_text", (item, active, hovered))?;
                let common: LuaTable = require_table(lua, "core.common")?;
                common.call_function::<LuaValue>(
                    "draw_text",
                    (font, color, text, LuaValue::Nil, x, y, 0, h),
                )?;
                Ok(())
            },
        )?,
    )?;

    // TreeView:draw_item_icon(item, active, hovered, x, y, w, h)
    let icon_nudge = icon_vertical_nudge;
    tree_view.set(
        "draw_item_icon",
        lua.create_function(
            move |lua,
                  (this, item, active, hovered, x, y, _w, h): (
                LuaTable,
                LuaTable,
                bool,
                bool,
                f64,
                f64,
                f64,
                f64,
            )| {
                let (icon_char, icon_font, icon_color): (String, LuaValue, LuaValue) =
                    this.call_method("get_item_icon", (item, active, hovered))?;
                let style: LuaTable = require_table(lua, "core.style")?;
                let font: LuaValue = style.get("font")?;
                let font_h: f64 = match &font {
                    LuaValue::Table(t) => t.call_method("get_height", ())?,
                    LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                    _ => 14.0,
                };
                let common: LuaTable = require_table(lua, "core.common")?;
                let round: LuaFunction = common.get("round")?;
                let text_top: f64 = y + round.call::<f64>((h - font_h) / 2.0)?;
                let icon_font_h: f64 = this.get("icon_font_height")?;
                let iy: f64 = text_top + round.call::<f64>((font_h - icon_font_h) / 2.0)? - icon_nudge;
                let renderer: LuaTable = lua.globals().get("renderer")?;
                let draw_text: LuaFunction = renderer.get("draw_text")?;
                draw_text.call::<LuaValue>((icon_font, icon_char, x, iy, icon_color))?;
                let icon_w: f64 = this.get("item_icon_width")?;
                let text_spacing: f64 = this.get("item_text_spacing")?;
                Ok(icon_w + text_spacing)
            },
        )?,
    )?;

    // TreeView:draw_item_body(item, active, hovered, x, y, w, h)
    tree_view.set(
        "draw_item_body",
        lua.create_function(
            |_lua,
             (this, item, active, hovered, x, y, w, h): (
                LuaTable,
                LuaTable,
                bool,
                bool,
                f64,
                f64,
                f64,
                f64,
            )| {
                let offset: f64 =
                    this.call_method("draw_item_icon", (item.clone(), active, hovered, x, y, w, h))?;
                let new_x = x + offset;
                this.call_method::<()>(
                    "draw_item_text",
                    (item, active, hovered, new_x, y, w, h),
                )?;
                Ok(())
            },
        )?,
    )?;

    // TreeView:draw_item_chevron(item, active, hovered, x, y, w, h)
    let chev_nudge = icon_vertical_nudge;
    tree_view.set(
        "draw_item_chevron",
        lua.create_function(
            move |lua,
                  (this, item, _active, hovered, x, y, _w, h): (
                LuaTable,
                LuaTable,
                bool,
                bool,
                f64,
                f64,
                f64,
                f64,
            )| {
                let item_type: String = item.get("type")?;
                if item_type == "dir" {
                    let expanded: bool = item.get("expanded").unwrap_or(false);
                    let chevron_icon = if expanded { "-" } else { "+" };
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let chevron_color: LuaValue = if hovered {
                        style.get("accent")?
                    } else {
                        style.get("text")?
                    };
                    let font: LuaValue = style.get("font")?;
                    let font_h: f64 = match &font {
                        LuaValue::Table(t) => t.call_method("get_height", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                        _ => 14.0,
                    };
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let round: LuaFunction = common.get("round")?;
                    let text_top: f64 = y + round.call::<f64>((h - font_h) / 2.0)?;
                    let icon_font_h: f64 = this.get("icon_font_height")?;
                    let iy: f64 = text_top + round.call::<f64>((font_h - icon_font_h) / 2.0)?
                        - chev_nudge;
                    let icon_font: LuaValue = style.get("icon_font")?;
                    let renderer: LuaTable = lua.globals().get("renderer")?;
                    let draw_text: LuaFunction = renderer.get("draw_text")?;
                    draw_text
                        .call::<LuaValue>((icon_font, chevron_icon, x, iy, chevron_color))?;
                }
                let chevron_w: f64 = this.get("item_chevron_width")?;
                Ok(chevron_w)
            },
        )?,
    )?;

    // TreeView:draw_item_background(item, active, hovered, x, y, w, h)
    tree_view.set(
        "draw_item_background",
        lua.create_function(
            |lua,
             (_this, _item, active, hovered, x, y, w, h): (
                LuaTable,
                LuaValue,
                bool,
                bool,
                f64,
                f64,
                f64,
                f64,
            )| {
                let style: LuaTable = require_table(lua, "core.style")?;
                let renderer: LuaTable = lua.globals().get("renderer")?;
                let draw_rect: LuaFunction = renderer.get("draw_rect")?;
                if active {
                    let line_hl: LuaTable = style.get("line_highlight")?;
                    let active_color = lua.create_table()?;
                    active_color.raw_set(1, line_hl.raw_get::<LuaValue>(1)?)?;
                    active_color.raw_set(2, line_hl.raw_get::<LuaValue>(2)?)?;
                    active_color.raw_set(3, line_hl.raw_get::<LuaValue>(3)?)?;
                    let a: f64 = line_hl.raw_get::<f64>(4).unwrap_or(0.0);
                    active_color.raw_set(4, a.max(210.0))?;
                    draw_rect.call::<()>((x, y, w, h, active_color))?;
                }
                if hovered && !active {
                    let line_hl: LuaTable = style.get("line_highlight")?;
                    let hover_color = lua.create_table()?;
                    hover_color.raw_set(1, line_hl.raw_get::<LuaValue>(1)?)?;
                    hover_color.raw_set(2, line_hl.raw_get::<LuaValue>(2)?)?;
                    hover_color.raw_set(3, line_hl.raw_get::<LuaValue>(3)?)?;
                    hover_color.raw_set(4, 110.0)?;
                    draw_rect.call::<()>((x, y, w, h, hover_color))?;
                }
                Ok(())
            },
        )?,
    )?;

    // TreeView:draw_item(item, active, hovered, x, y, w, h)
    tree_view.set(
        "draw_item",
        lua.create_function(
            |lua,
             (this, item, active, hovered, x, y, w, h): (
                LuaTable,
                LuaTable,
                bool,
                bool,
                f64,
                f64,
                f64,
                f64,
            )| {
                this.call_method::<()>(
                    "draw_item_background",
                    (item.clone(), active, hovered, x, y, w, h),
                )?;
                let style: LuaTable = require_table(lua, "core.style")?;
                let padding: LuaTable = style.get("padding")?;
                let pad_x: f64 = padding.get("x")?;
                let depth: f64 = item.get("depth")?;
                let mut draw_x = x + depth * pad_x + pad_x;
                let chevron_w: f64 = this.call_method(
                    "draw_item_chevron",
                    (item.clone(), active, hovered, draw_x, y, w, h),
                )?;
                draw_x += chevron_w;
                this.call_method::<()>(
                    "draw_item_body",
                    (item, active, hovered, draw_x, y, w, h),
                )?;
                Ok(())
            },
        )?,
    )?;

    // TreeView:draw()
    tree_view.set("draw", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let visible: bool = this.get("visible").unwrap_or(false);
            if !visible { return Ok(()); }
            let style: LuaTable = require_table(lua, "core.style")?;
            let bg2: LuaValue = style.get("background2")?;
            this.call_method::<()>("draw_background", bg2)?;

            let renderer: LuaTable = lua.globals().get("renderer")?;
            let draw_rect: LuaFunction = renderer.get("draw_rect")?;
            let position: LuaTable = this.get("position")?;
            let pos_x: f64 = position.get("x")?;
            let pos_y: f64 = position.get("y")?;
            let size: LuaTable = this.get("size")?;
            let size_x: f64 = size.get("x")?;
            let size_y: f64 = size.get("y")?;
            let divider_size: f64 = style.get("divider_size")?;
            let divider_color: LuaValue = style.get("divider")?;
            draw_rect.call::<()>((
                pos_x + SEPARATOR_INSET,
                pos_y,
                (size_x - SEPARATOR_INSET * 2.0).max(0.0),
                divider_size,
                divider_color,
            ))?;

            let core: LuaTable = require_table(lua, "core")?;
            let projects: LuaTable = core.get("projects")?;
            let project_count = projects.raw_len() as i64;
            let last_count: i64 = this.get("last_project_count").unwrap_or(0);
            if project_count != last_count {
                this.set("items_dirty", true)?;
            }
            this.call_method::<()>("sync_model", ())?;

            let (ox, oy): (f64, f64) = this.call_method("get_content_offset", ())?;
            let h: f64 = this.call_method("get_item_height", ())?;
            let padding: LuaTable = style.get("padding")?;
            let pad: f64 = padding.get("y")?;
            let first_row = 1i64.max(((pos_y - oy - pad) / h).floor() as i64 + 1);
            let visible_count: i64 = this.get("visible_count")?;
            let last_row = visible_count.min(((pos_y + size_y - oy - pad) / h).floor() as i64 + 1);
            let items: LuaTable = this.call_method("get_items_in_range", (first_row, last_row))?;

            let selected_path: LuaValue = this.get("selected_path")?;
            let hovered_path: LuaValue = this.get("hovered_path")?;

            for pair in items.pairs::<i64, LuaTable>() {
                let (offset, item) = pair?;
                let row = first_row + offset - 1;
                let item_y = oy + pad + (row - 1) as f64 * h;
                let abs_filename: String = item.get("abs_filename")?;
                let is_active = match &selected_path {
                    LuaValue::String(s) => s.to_str().map(|s| s == abs_filename).unwrap_or(false),
                    _ => false,
                };
                let is_hovered = match &hovered_path {
                    LuaValue::String(s) => s.to_str().map(|s| s == abs_filename).unwrap_or(false),
                    _ => false,
                };
                this.call_method::<()>(
                    "draw_item",
                    (item, is_active, is_hovered, ox, item_y, size_x, h),
                )?;
            }

            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_draw_scrollbar: LuaFunction = super_tbl.get("draw_scrollbar")?;
            super_draw_scrollbar.call::<()>(this.clone())?;

            let hp: LuaValue = this.get("hovered_path")?;
            let tooltip: LuaTable = this.get("tooltip")?;
            let tooltip_x: LuaValue = tooltip.get("x")?;
            let tooltip_alpha: f64 = tooltip.get("alpha").unwrap_or(0.0);
            if !matches!(hp, LuaValue::Nil) && !matches!(tooltip_x, LuaValue::Nil) && tooltip_alpha > 0.0 {
                let root_view: LuaTable = core.get("root_view")?;
                let draw_tooltip_fn: LuaFunction = this.get("draw_tooltip")?;
                root_view.call_method::<()>("defer_draw", (draw_tooltip_fn, this))?;
            }
            Ok(())
        })?
    })?;

    // TreeView:get_parent(item)
    tree_view.set(
        "get_parent",
        lua.create_function(|lua, (this, item): (LuaTable, Option<LuaTable>)| -> LuaResult<LuaMultiValue> {
            let item = match item {
                Some(i) => LuaValue::Table(i),
                None => this.call_method("get_selected_item", ())?,
            };
            let item = match item {
                LuaValue::Table(t) => t,
                _ => return Ok(LuaMultiValue::new()),
            };
            let abs_filename: String = item.get("abs_filename")?;
            let common: LuaTable = require_table(lua, "core.common")?;
            let parent_path: LuaValue = common.call_function("dirname", abs_filename)?;
            if matches!(parent_path, LuaValue::Nil) {
                return Ok(LuaMultiValue::new());
            }
            let result: LuaMultiValue = this.call_method("resolve_path", parent_path)?;
            let mut vals: Vec<LuaValue> = result.into_iter().collect();
            if vals.is_empty() || matches!(vals[0], LuaValue::Nil) {
                return Ok(LuaMultiValue::new());
            }
            // Return (item, y) — skip ox, keep y
            let item_val = vals.remove(0);
            let y_val = if vals.len() >= 2 { vals.remove(1) } else { LuaValue::Nil };
            Ok(LuaMultiValue::from_vec(vec![item_val, y_val]))
        })?,
    )?;

    // TreeView:get_item(item, direction)
    tree_view.set(
        "get_item",
        lua.create_function(|lua, (this, item, direction): (LuaTable, LuaValue, i64)| -> LuaResult<LuaMultiValue> {
            this.call_method::<()>("sync_model", ())?;
            let idx: i64 = if let LuaValue::Table(ref t) = item {
                let abs_filename: String = t.get("abs_filename")?;
                let model_roots: LuaTable = this.get("model_roots")?;
                let tree_model: LuaTable = require_table(lua, "tree_model")?;
                let get_row: LuaFunction = tree_model.get("get_row")?;
                let row: LuaValue = get_row.call((model_roots, abs_filename))?;
                match row {
                    LuaValue::Integer(n) => n + direction,
                    LuaValue::Number(n) => n as i64 + direction,
                    _ => if direction >= 0 { 1 } else { this.get::<i64>("visible_count")? },
                }
            } else if direction >= 0 {
                1
            } else {
                this.get::<i64>("visible_count")?
            };
            let visible_count: i64 = this.get("visible_count")?;
            let idx = idx.max(1).min(visible_count);
            let target: LuaValue = this.call_method("get_item_by_row", idx)?;
            if let LuaValue::Table(ref t) = target {
                let abs: String = t.get("abs_filename")?;
                let result: LuaMultiValue = this.call_method("resolve_path", abs)?;
                Ok(result)
            } else {
                Ok(LuaMultiValue::new())
            }
        })?,
    )?;

    // TreeView:get_next(item)
    tree_view.set(
        "get_next",
        lua.create_function(|_lua, (this, item): (LuaTable, LuaValue)| -> LuaResult<LuaMultiValue> {
            this.call_method("get_item", (item, 1))
        })?,
    )?;

    // TreeView:get_previous(item)
    tree_view.set(
        "get_previous",
        lua.create_function(|_lua, (this, item): (LuaTable, LuaValue)| -> LuaResult<LuaMultiValue> {
            this.call_method("get_item", (item, -1))
        })?,
    )?;

    // TreeView:toggle_expand(toggle, item)
    tree_view.set(
        "toggle_expand",
        lua.create_function(|lua, (this, toggle, item): (LuaTable, LuaValue, Option<LuaTable>)| {
            let item = match item {
                Some(i) => LuaValue::Table(i),
                None => this.call_method("get_selected_item", ())?,
            };
            let item = match item {
                LuaValue::Table(t) => t,
                _ => return Ok(()),
            };
            let item_type: String = item.get("type")?;
            if item_type == "dir" {
                let abs_filename: String = item.get("abs_filename")?;
                let tree_model: LuaTable = require_table(lua, "tree_model")?;
                let toggle_fn: LuaFunction = tree_model.get("toggle_expand")?;
                let toggle_arg: LuaValue = match toggle {
                    LuaValue::Boolean(b) => LuaValue::Boolean(b),
                    _ => LuaValue::Nil,
                };
                toggle_fn.call::<()>((abs_filename, toggle_arg))?;
                this.set("items_dirty", true)?;
            }
            Ok(())
        })?,
    )?;

    // TreeView:open_doc(filename)
    tree_view.set(
        "open_doc",
        lua.create_function(|lua, (_this, filename): (LuaTable, String)| {
            let core: LuaTable = require_table(lua, "core")?;
            let root_view: LuaTable = core.get("root_view")?;
            let doc: LuaValue = core.call_function("open_doc", filename)?;
            root_view.call_method::<()>("open_doc", doc)?;
            Ok(())
        })?,
    )?;

    // TreeView:on_context_menu()
    tree_view.set(
        "on_context_menu",
        lua.create_function(|lua, this: LuaTable| {
            let context_menu: LuaTable = require_table(lua, "core.contextmenu")?;
            let divider: LuaValue = context_menu.get("DIVIDER")?;
            let items = lua.create_table()?;

            let entries = [
                ("Open in System", "treeview:open-in-system"),
            ];
            let mut idx = 1;
            for (text, cmd) in &entries {
                let entry = lua.create_table()?;
                entry.set("text", *text)?;
                entry.set("command", *cmd)?;
                items.raw_set(idx, entry)?;
                idx += 1;
            }
            items.raw_set(idx, divider)?;
            idx += 1;
            let entries2 = [
                ("Rename", "treeview:rename"),
                ("Delete", "treeview:delete"),
                ("New File", "treeview:new-file"),
                ("New Folder", "treeview:new-folder"),
                ("Remove directory", "treeview:remove-project-directory"),
                ("Find in Directory", "treeview:search-in-directory"),
            ];
            for (text, cmd) in &entries2 {
                let entry = lua.create_table()?;
                entry.set("text", *text)?;
                entry.set("command", *cmd)?;
                items.raw_set(idx, entry)?;
                idx += 1;
            }
            let result = lua.create_table()?;
            result.set("items", items)?;
            Ok((result, this))
        })?,
    )?;

    // ── Instantiate the view ─────────────────────────────────────────────────

    let view: LuaTable = tree_view.call_function("__call", tree_view.clone())?;
    view.call_method::<()>("on_scale_change", ())?;
    let root_view: LuaTable = core.get("root_view")?;
    let node: LuaTable = root_view.call_method("get_active_node", ())?;
    let split_opts = lua.create_table()?;
    split_opts.set("x", true)?;
    let view_node: LuaTable =
        node.call_method("split", ("left", view.clone(), split_opts, true))?;
    view.set("node", view_node.clone())?;

    // Toolbar integration
    let toolbar_plugin_config: LuaValue = plugins.get("toolbarview")?;
    let mut toolbar_view_val: LuaValue = LuaValue::Nil;
    if !matches!(toolbar_plugin_config, LuaValue::Boolean(false)) {
        let pcall: LuaFunction = lua.globals().get("pcall")?;
        let require_fn: LuaFunction = lua.globals().get("require")?;
        let result: LuaMultiValue =
            pcall.call((require_fn, "plugins.toolbarview"))?;
        let mut vals = result.into_iter();
        let ok: bool = match vals.next() {
            Some(LuaValue::Boolean(b)) => b,
            _ => false,
        };
        if ok {
            if let Some(LuaValue::Table(toolbar_class)) = vals.next() {
                let tb: LuaTable = toolbar_class.call_function("__call", toolbar_class.clone())?;
                let split_opts2 = lua.create_table()?;
                split_opts2.set("y", true)?;
                view_node.call_method::<LuaTable>("split", ("down", tb.clone(), split_opts2))?;
                let min_w: f64 = tb.call_method("get_min_width", ())?;
                let tv_size: f64 = tv_cfg.get("size")?;
                view.call_method::<LuaValue>("set_target_size", ("x", tv_size.max(min_w)))?;

                // toolbar:toggle command
                let tb_key = Arc::new(lua.create_registry_value(tb.clone())?);
                let toggle_cmd = lua.create_table()?;
                toggle_cmd.set(
                    "toolbar:toggle",
                    lua.create_function(move |lua, ()| {
                        let tb: LuaTable = lua.registry_value(&tb_key)?;
                        tb.call_method::<()>("toggle_visible", ())?;
                        Ok(())
                    })?,
                )?;
                command.call_function::<()>("add", (LuaValue::Nil, toggle_cmd))?;

                toolbar_view_val = LuaValue::Table(tb);
            }
        }
    }

    // Monkey-patch core.remove_project
    let view_key = Arc::new(lua.create_registry_value(view.clone())?);
    {
        let old_remove: LuaFunction = core.get("remove_project")?;
        let vk = Arc::clone(&view_key);
        let new_remove = lua.create_function(move |lua, (project, force): (LuaValue, LuaValue)| {
            let old: LuaFunction = lua.globals().get::<LuaTable>("core")?.get("_old_remove_project")?;
            let result: LuaValue = old.call((project, force))?;
            let v: LuaTable = lua.registry_value(&vk)?;
            v.set("items_dirty", true)?;
            let native: LuaTable = require_table(lua, "treeview_native")?;
            let sync_fn: LuaFunction = native.get("sync_model")?;
            sync_fn.call::<()>(v)?;
            Ok(result)
        })?;
        core.set("_old_remove_project", old_remove)?;
        core.set("remove_project", new_remove)?;
    }

    // Monkey-patch core.on_quit_project (if it exists)
    {
        let quit_val: LuaValue = core.get("on_quit_project")?;
        if let LuaValue::Function(_) = quit_val {
            let vk = Arc::clone(&view_key);
            let new_quit = lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                v.set("items_dirty", true)?;
                let tree_model: LuaTable = require_table(lua, "tree_model")?;
                let clear_all: LuaFunction = tree_model.get("clear_all")?;
                clear_all.call::<()>(())?;
                let old: LuaValue =
                    lua.globals().get::<LuaTable>("core")?.get("_old_on_quit_project")?;
                if let LuaValue::Function(f) = old {
                    f.call::<()>(())?;
                }
                Ok(())
            })?;
            core.set("_old_on_quit_project", quit_val)?;
            core.set("on_quit_project", new_quit)?;
        }
    }

    // ── Helper closures for commands ─────────────────────────────────────────

    let is_project_folder = lua.create_function(|_lua, item: LuaTable| -> LuaResult<bool> {
        let project: LuaValue = item.get("project")?;
        if matches!(project, LuaValue::Nil) {
            return Ok(false);
        }
        let abs_filename: String = item.get("abs_filename")?;
        if let LuaValue::Table(p) = project {
            let path: String = p.get("path")?;
            Ok(abs_filename == path)
        } else {
            Ok(false)
        }
    })?;
    let is_project_folder_key = Arc::new(lua.create_registry_value(is_project_folder)?);

    let is_primary_project_folder =
        lua.create_function(|lua, path: String| -> LuaResult<bool> {
            let core: LuaTable = require_table(lua, "core")?;
            let root_project: LuaFunction = core.get("root_project")?;
            let project: LuaTable = root_project.call(())?;
            let project_path: String = project.get("path")?;
            Ok(project_path == path)
        })?;
    let is_primary_key = Arc::new(lua.create_registry_value(is_primary_project_folder)?);

    // treeitem() helper
    let treeitem = {
        let vk = Arc::clone(&view_key);
        lua.create_function(move |lua, ()| -> LuaResult<LuaValue> {
            let v: LuaTable = lua.registry_value(&vk)?;
            let hovered: LuaValue = v.call_method("get_hovered_item", ())?;
            if matches!(hovered, LuaValue::Table(_)) {
                return Ok(hovered);
            }
            v.call_method("get_selected_item", ())
        })?
    };
    let treeitem_key = Arc::new(lua.create_registry_value(treeitem)?);

    // invalidate_project_tree helper
    let invalidate_fn = {
        let vk = Arc::clone(&view_key);
        lua.create_function(move |lua, project: LuaValue| {
            if let LuaValue::Table(p) = &project {
                let path: Option<String> = p.get("path")?;
                if let Some(path) = path {
                    let tree_model: LuaTable = require_table(lua, "tree_model")?;
                    let invalidate: LuaFunction = tree_model.get("invalidate")?;
                    invalidate.call::<()>(path)?;
                }
            }
            let v: LuaTable = lua.registry_value(&vk)?;
            v.set("items_dirty", true)?;
            Ok(())
        })?
    };
    let invalidate_key = Arc::new(lua.create_registry_value(invalidate_fn)?);

    // ── treeview:search-in-directory command ──────────────────────────────────
    let projectsearch_cfg: LuaValue = plugins.get("projectsearch")?;
    if !matches!(projectsearch_cfg, LuaValue::Boolean(false)) {
        let vk = Arc::clone(&view_key);
        let pred = lua.create_function(move |lua, active_view: LuaValue| {
            let v: LuaTable = lua.registry_value(&vk)?;
            let hovered: LuaValue = v.call_method("get_hovered_item", ())?;
            if let LuaValue::Table(ref h) = hovered {
                let item_type: String = h.get("type")?;
                if item_type != "dir" {
                    return Ok((false, LuaValue::Nil));
                }
                let av = match active_view {
                    LuaValue::Table(t) => t,
                    _ => {
                        let core: LuaTable = require_table(lua, "core")?;
                        core.get("active_view")?
                    }
                };
                if av == v {
                    return Ok((true, LuaValue::Nil));
                }
            }
            Ok((false, LuaValue::Nil))
        })?;

        let vk2 = Arc::clone(&view_key);
        let cmds = lua.create_table()?;
        cmds.set(
            "treeview:search-in-directory",
            lua.create_function(move |lua, _item: LuaValue| {
                let v: LuaTable = lua.registry_value(&vk2)?;
                let hovered: LuaValue = v.call_method("get_hovered_item", ())?;
                if let LuaValue::Table(h) = hovered {
                    let abs_filename: String = h.get("abs_filename")?;
                    let cmd: LuaTable = require_table(lua, "core.command")?;
                    cmd.call_function::<()>("perform", ("project-search:find", abs_filename))?;
                }
                Ok(())
            })?,
        )?;
        command.call_function::<()>("add", (pred, cmds))?;
    }

    // ── treeview:delete and treeview:rename ──────────────────────────────────
    {
        let vk = Arc::clone(&view_key);
        let ipfk = Arc::clone(&is_project_folder_key);
        let tik = Arc::clone(&treeitem_key);
        let pred = lua.create_function(move |lua, active_view: LuaValue| {
            let ti_fn: LuaFunction = lua.registry_value(&tik)?;
            let item: LuaValue = ti_fn.call(())?;
            if let LuaValue::Table(ref t) = item {
                let ipf: LuaFunction = lua.registry_value(&ipfk)?;
                let is_pf: bool = ipf.call(t.clone())?;
                if is_pf {
                    return Ok((false, LuaValue::Nil));
                }
                let v: LuaTable = lua.registry_value(&vk)?;
                let av = match active_view {
                    LuaValue::Table(t) => t,
                    _ => {
                        let core: LuaTable = require_table(lua, "core")?;
                        core.get("active_view")?
                    }
                };
                if av == v {
                    return Ok((true, item));
                }
            }
            Ok((false, LuaValue::Nil))
        })?;

        let inv_k = Arc::clone(&invalidate_key);
        let cmds = lua.create_table()?;
        cmds.set(
            "treeview:delete",
            lua.create_function(move |lua, item: LuaTable| {
                let filename: String = item.get("abs_filename")?;
                let relfilename: String = item.get("filename")?;
                let core: LuaTable = require_table(lua, "core")?;
                let common: LuaTable = require_table(lua, "core.common")?;
                let root_project: LuaFunction = core.get("root_project")?;
                let rp: LuaTable = root_project.call(())?;
                let project: LuaValue = item.get("project")?;

                let display_name = if let LuaValue::Table(ref p) = project {
                    let pp: String = p.get("path")?;
                    let rpp: String = rp.get("path")?;
                    if pp != rpp {
                        let pathsep: String = lua.globals().get("PATHSEP")?;
                        let basename: String = common.call_function("basename", filename.clone())?;
                        format!("{}{}{}", basename, pathsep, relfilename)
                    } else {
                        relfilename
                    }
                } else {
                    relfilename
                };

                let system: LuaTable = lua.globals().get("system")?;
                let file_info: LuaTable = system.call_function("get_file_info", filename.clone())?;
                let file_type: String = file_info.get("type")?;
                let type_label = if file_type == "dir" { "Directory" } else { "File" };

                let nag_view: LuaTable = core.get("nag_view")?;
                let opt = lua.create_table()?;
                let yes = lua.create_table()?;
                yes.set("text", "Yes")?;
                yes.set("default_yes", true)?;
                opt.raw_set(1, yes)?;
                let no = lua.create_table()?;
                no.set("text", "No")?;
                no.set("default_no", true)?;
                opt.raw_set(2, no)?;

                let title = format!("Delete {}", type_label);
                let msg = format!(
                    "Are you sure you want to delete the {}?\n{}: {}",
                    type_label.to_lowercase(),
                    type_label,
                    display_name,
                );

                let inv_fn_k = Arc::clone(&inv_k);
                let callback = lua.create_function(move |lua, choice: LuaTable| {
                    let text: String = choice.get("text")?;
                    if text == "Yes" {
                        let system: LuaTable = lua.globals().get("system")?;
                        let fi: LuaTable = system.call_function("get_file_info", filename.clone())?;
                        let ft: String = fi.get("type")?;
                        let core: LuaTable = require_table(lua, "core")?;
                        if ft == "dir" {
                            let common: LuaTable = require_table(lua, "core.common")?;
                            let result: LuaMultiValue = common.call_function("rm", (filename.clone(), true))?;
                            let mut vals = result.into_iter();
                            let deleted = !matches!(vals.next(), Some(LuaValue::Boolean(false)) | Some(LuaValue::Nil));
                            if !deleted {
                                let err = match vals.next() {
                                    Some(LuaValue::String(s)) => s.to_string_lossy().to_string(),
                                    _ => "unknown error".to_string(),
                                };
                                let path = match vals.next() {
                                    Some(LuaValue::String(s)) => s.to_string_lossy().to_string(),
                                    _ => filename.clone(),
                                };
                                let err_fn: LuaFunction = core.get("error")?;
                                err_fn.call::<()>(format!("Error: {} - \"{}\" ", err, path))?;
                                return Ok(());
                            }
                        } else {
                            let os: LuaTable = lua.globals().get("os")?;
                            let result: LuaMultiValue = os.call_function("remove", filename.clone())?;
                            let mut vals = result.into_iter();
                            let removed = !matches!(vals.next(), Some(LuaValue::Boolean(false)) | Some(LuaValue::Nil));
                            if !removed {
                                let err = match vals.next() {
                                    Some(LuaValue::String(s)) => s.to_string_lossy().to_string(),
                                    _ => "unknown error".to_string(),
                                };
                                let err_fn: LuaFunction = core.get("error")?;
                                err_fn.call::<()>(format!("Error: {} - \"{}\"", err, filename))?;
                                return Ok(());
                            }
                        }
                        let log_fn: LuaFunction = core.get("log")?;
                        log_fn.call::<()>(format!("Deleted \"{}\"", filename))?;
                        let inv_fn: LuaFunction = lua.registry_value(&inv_fn_k)?;
                        let item_project: LuaValue = lua.globals().get::<LuaTable>("core")?.get("_treeview_last_delete_project")?;
                        inv_fn.call::<()>(item_project)?;
                    }
                    Ok(())
                })?;

                // Stash the project for use in the callback
                core.set("_treeview_last_delete_project", project)?;

                nag_view.call_method::<()>("show", (title, msg, opt, callback))?;
                Ok(())
            })?,
        )?;

        let inv_k2 = Arc::clone(&invalidate_key);
        cmds.set(
            "treeview:rename",
            lua.create_function(move |lua, item: LuaTable| {
                let project: LuaValue = item.get("project")?;
                let old_abs: String = item.get("abs_filename")?;
                let old_filename: String = if let LuaValue::Table(ref p) = project {
                    p.call_method("normalize_path", old_abs.clone())?
                } else {
                    old_abs.clone()
                };
                let core: LuaTable = require_table(lua, "core")?;
                let command_view: LuaTable = core.get("command_view")?;

                let inv_k = Arc::clone(&inv_k2);
                let opts = lua.create_table()?;
                opts.set("text", old_filename.clone())?;
                let submit = lua.create_function(move |lua, filename: String| {
                    let core: LuaTable = require_table(lua, "core")?;
                    let item_project: LuaValue = core.get("_treeview_rename_project")?;
                    let abs_filename: String = if let LuaValue::Table(ref p) = item_project {
                        p.call_method("absolute_path", filename.clone())?
                    } else {
                        filename.clone()
                    };
                    let os: LuaTable = lua.globals().get("os")?;
                    let result: LuaMultiValue = os.call_function("rename", (old_abs.clone(), abs_filename.clone()))?;
                    let mut vals = result.into_iter();
                    let ok = !matches!(vals.next(), Some(LuaValue::Nil) | Some(LuaValue::Boolean(false)));
                    if ok {
                        let docs: LuaTable = core.get("docs")?;
                        for entry in docs.sequence_values::<LuaTable>() {
                            let doc = entry?;
                            let doc_abs: LuaValue = doc.get("abs_filename")?;
                            if let LuaValue::String(ref s) = doc_abs {
                                let s = s.to_str()?.to_string();
                                if s == old_abs {
                                    doc.call_method::<()>("set_filename", (filename.clone(), abs_filename.clone()))?;
                                    doc.call_method::<()>("reset_syntax", ())?;
                                    break;
                                }
                            }
                        }
                        let log_fn: LuaFunction = core.get("log")?;
                        log_fn.call::<()>(format!("Renamed \"{}\" to \"{}\"", old_filename, filename))?;
                    } else {
                        let err = match vals.next() {
                            Some(LuaValue::String(s)) => s.to_string_lossy().to_string(),
                            _ => "unknown error".to_string(),
                        };
                        let err_fn: LuaFunction = core.get("error")?;
                        err_fn.call::<()>(format!(
                            "Error while renaming \"{}\" to \"{}\": {}",
                            old_abs, abs_filename, err
                        ))?;
                    }
                    let inv_fn: LuaFunction = lua.registry_value(&inv_k)?;
                    inv_fn.call::<()>(core.get::<LuaValue>("_treeview_rename_project")?)
                })?;
                opts.set("submit", submit)?;

                let project_path = if let LuaValue::Table(ref p) = project {
                    let pp: Option<String> = p.get("path")?;
                    pp
                } else {
                    None
                };
                let pp_for_suggest = project_path.clone();
                let suggest = lua.create_function(move |lua, text: String| -> LuaResult<LuaValue> {
                    let common: LuaTable = require_table(lua, "core.common")?;
                    common.call_function("path_suggest", (text, pp_for_suggest.clone()))
                })?;
                opts.set("suggest", suggest)?;

                core.set("_treeview_rename_project", project)?;
                command_view.call_method::<()>("enter", ("Rename", opts))?;
                Ok(())
            })?,
        )?;

        command.call_function::<()>("add", (pred, cmds))?;
    }

    // ── Global commands (nil predicate) ──────────────────────────────────────
    {
        let vk = Arc::clone(&view_key);
        let cmds = lua.create_table()?;
        cmds.set("treeview:toggle", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let visible: bool = v.get("visible").unwrap_or(false);
                v.set("visible", !visible)
            })?
        })?;
        cmds.set("treeview:toggle-hidden", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let show: bool = v.get("show_hidden").unwrap_or(false);
                v.set("show_hidden", !show)?;
                v.set("items_dirty", true)
            })?
        })?;
        cmds.set("treeview:toggle-ignored", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let show: bool = v.get("show_ignored").unwrap_or(true);
                v.set("show_ignored", !show)?;
                v.set("items_dirty", true)
            })?
        })?;
        cmds.set("treeview:toggle-focus", {
            let vk = Arc::clone(&vk);
            let ck = Arc::clone(&class_key);
            lua.create_function(move |lua, ()| {
                let core: LuaTable = require_table(lua, "core")?;
                let v: LuaTable = lua.registry_value(&vk)?;
                let active_view: LuaTable = core.get("active_view")?;
                let tv_class: LuaTable = lua.registry_value(&ck)?;
                let is_tv: bool = active_view.call_method("is", tv_class)?;
                if !is_tv {
                    let command_view_class: LuaTable = require_table(lua, "core.commandview")?;
                    let is_cv: bool = active_view.call_method("is", command_view_class)?;
                    let prev = if is_cv {
                        core.get::<LuaValue>("last_active_view")?
                    } else {
                        LuaValue::Table(active_view)
                    };
                    let prev = match prev {
                        LuaValue::Nil => {
                            let root_view: LuaTable = core.get("root_view")?;
                            let primary: LuaTable = root_view.call_method("get_primary_node", ())?;
                            LuaValue::Table(primary.get::<LuaTable>("active_view")?)
                        }
                        other => other,
                    };
                    core.set("_treeview_previous_view", prev)?;
                    let set_active: LuaFunction = core.get("set_active_view")?;
                    set_active.call::<()>(v.clone())?;
                    let selected: LuaValue = v.call_method("get_selected_item", ())?;
                    if matches!(selected, LuaValue::Nil) {
                        let iter: LuaFunction = v.call_method("each_item", ())?;
                        let result: LuaMultiValue = iter.call(())?;
                        let mut vals = result.into_iter();
                        let first_item = vals.next().unwrap_or(LuaValue::Nil);
                        if matches!(first_item, LuaValue::Table(_)) {
                            let _ = vals.next(); // ox
                            let y = vals.next().unwrap_or(LuaValue::Nil);
                            v.call_method::<()>("set_selection", (first_item, y))?;
                        }
                    }
                } else {
                    let prev: LuaValue = core.get("_treeview_previous_view")?;
                    let target = match prev {
                        LuaValue::Table(t) => t,
                        _ => {
                            let root_view: LuaTable = core.get("root_view")?;
                            let primary: LuaTable = root_view.call_method("get_primary_node", ())?;
                            primary.get::<LuaTable>("active_view")?
                        }
                    };
                    let set_active: LuaFunction = core.get("set_active_view")?;
                    set_active.call::<()>(target)?;
                }
                Ok(())
            })?
        })?;
        command.call_function::<()>("add", (LuaValue::Nil, cmds))?;
    }

    // ── TreeView-specific commands ───────────────────────────────────────────
    {
        let vk = Arc::clone(&view_key);
        let cmds = lua.create_table()?;

        cmds.set("treeview:next", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let selected: LuaValue = v.call_method("get_selected_item", ())?;
                let result: LuaMultiValue = v.call_method("get_next", selected)?;
                let mut vals = result.into_iter();
                let item = vals.next().unwrap_or(LuaValue::Nil);
                let _ = vals.next(); // ox
                let y = vals.next().unwrap_or(LuaValue::Nil);
                v.call_method::<()>("set_selection", (item, y))
            })?
        })?;

        cmds.set("treeview:previous", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let selected: LuaValue = v.call_method("get_selected_item", ())?;
                let result: LuaMultiValue = v.call_method("get_previous", selected)?;
                let mut vals = result.into_iter();
                let item = vals.next().unwrap_or(LuaValue::Nil);
                let _ = vals.next();
                let y = vals.next().unwrap_or(LuaValue::Nil);
                v.call_method::<()>("set_selection", (item, y))
            })?
        })?;

        cmds.set("treeview:open", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let item: LuaValue = v.call_method("get_selected_item", ())?;
                let item = match item {
                    LuaValue::Table(t) => t,
                    _ => return Ok(()),
                };
                let item_type: String = item.get("type")?;
                if item_type == "dir" {
                    v.call_method::<()>("toggle_expand", ())?;
                } else {
                    let core: LuaTable = require_table(lua, "core")?;
                    let try_fn: LuaFunction = core.get("try")?;
                    let last_active: LuaValue = core.get("last_active_view")?;
                    let active_view: LuaTable = core.get("active_view")?;
                    if matches!(last_active, LuaValue::Table(_)) && active_view == v {
                        let set_active: LuaFunction = core.get("set_active_view")?;
                        set_active.call::<()>(last_active)?;
                    }
                    let project: LuaValue = item.get("project")?;
                    let normalized: String = if let LuaValue::Table(ref p) = project {
                        p.call_method("normalize_path", item.get::<String>("abs_filename")?)?
                    } else {
                        item.get("abs_filename")?
                    };
                    let open_fn = lua.create_function(move |lua, ()| {
                        let v: LuaTable = lua.globals().get::<LuaTable>("core")?.get("_treeview_view_ref")?;
                        let normalized: String = lua.globals().get::<LuaTable>("core")?.get("_treeview_open_path")?;
                        v.call_method::<()>("open_doc", normalized)?;
                        Ok(())
                    })?;
                    core.set("_treeview_view_ref", v)?;
                    core.set("_treeview_open_path", normalized)?;
                    try_fn.call::<()>(open_fn)?;
                }
                Ok(())
            })?
        })?;

        cmds.set("treeview:deselect", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                v.call_method::<()>("set_selection", (LuaValue::Nil,))
            })?
        })?;

        cmds.set("treeview:select", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let hovered: LuaValue = v.call_method("get_hovered_item", ())?;
                v.call_method::<()>("set_selection", (hovered,))
            })?
        })?;

        cmds.set("treeview:select-and-open", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let hovered: LuaValue = v.call_method("get_hovered_item", ())?;
                if matches!(hovered, LuaValue::Table(_)) {
                    v.call_method::<()>("set_selection", (hovered,))?;
                    let command: LuaTable = require_table(lua, "core.command")?;
                    command.call_function::<()>("perform", "treeview:open")?;
                }
                Ok(())
            })?
        })?;

        cmds.set("treeview:collapse", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let item: LuaValue = v.call_method("get_selected_item", ())?;
                if let LuaValue::Table(ref t) = item {
                    let item_type: String = t.get("type")?;
                    let expanded: bool = t.get("expanded").unwrap_or(false);
                    if item_type == "dir" && expanded {
                        v.call_method::<()>("toggle_expand", (false,))?;
                    } else {
                        let result: LuaMultiValue = v.call_method("get_parent", (t.clone(),))?;
                        let mut vals = result.into_iter();
                        let parent = vals.next().unwrap_or(LuaValue::Nil);
                        let y = vals.next().unwrap_or(LuaValue::Nil);
                        if matches!(parent, LuaValue::Table(_)) {
                            v.call_method::<()>("set_selection", (parent, y))?;
                        }
                    }
                }
                Ok(())
            })?
        })?;

        cmds.set("treeview:expand", {
            let vk = Arc::clone(&vk);
            lua.create_function(move |lua, ()| {
                let v: LuaTable = lua.registry_value(&vk)?;
                let item: LuaValue = v.call_method("get_selected_item", ())?;
                let item = match item {
                    LuaValue::Table(t) => t,
                    _ => return Ok(()),
                };
                let item_type: String = item.get("type")?;
                if item_type != "dir" {
                    return Ok(());
                }
                let expanded: bool = item.get("expanded").unwrap_or(false);
                if expanded {
                    let result: LuaMultiValue = v.call_method("get_next", item.clone())?;
                    let mut vals = result.into_iter();
                    let next_item = vals.next().unwrap_or(LuaValue::Nil);
                    if let LuaValue::Table(ref ni) = next_item {
                        let next_depth: f64 = ni.get("depth")?;
                        let item_depth: f64 = item.get("depth")?;
                        if next_depth > item_depth {
                            let _ = vals.next(); // ox
                            let y = vals.next().unwrap_or(LuaValue::Nil);
                            v.call_method::<()>("set_selection", (next_item, y))?;
                        }
                    }
                } else {
                    v.call_method::<()>("toggle_expand", (true,))?;
                }
                Ok(())
            })?
        })?;

        command.call_function::<()>("add", (tree_view.clone(), cmds))?;
    }

    // ── treeview:new-file, treeview:new-folder, treeview:open-in-system ──────
    {
        let vk = Arc::clone(&view_key);
        let tik = Arc::clone(&treeitem_key);
        let pred = lua.create_function(move |lua, active_view: LuaValue| {
            let ti_fn: LuaFunction = lua.registry_value(&tik)?;
            let item: LuaValue = ti_fn.call(())?;
            if matches!(item, LuaValue::Table(_)) {
                let v: LuaTable = lua.registry_value(&vk)?;
                let av = match active_view {
                    LuaValue::Table(t) => t,
                    _ => {
                        let core: LuaTable = require_table(lua, "core")?;
                        core.get("active_view")?
                    }
                };
                if av == v {
                    return Ok((true, item));
                }
            }
            Ok((false, LuaValue::Nil))
        })?;

        let inv_k = Arc::clone(&invalidate_key);
        let ipfk = Arc::clone(&is_project_folder_key);
        let cmds = lua.create_table()?;

        cmds.set("treeview:new-file", {
            let inv_k = Arc::clone(&inv_k);
            let ipfk = Arc::clone(&ipfk);
            lua.create_function(move |lua, item: LuaTable| {
                let ipf: LuaFunction = lua.registry_value(&ipfk)?;
                let is_pf: bool = ipf.call(item.clone())?;
                let text: LuaValue = if !is_pf {
                    let item_type: String = item.get("type")?;
                    let project: LuaValue = item.get("project")?;
                    let pathsep: String = lua.globals().get("PATHSEP")?;
                    if item_type == "dir" {
                        if let LuaValue::Table(ref p) = project {
                            let normalized: String = p.call_method("normalize_path", item.get::<String>("abs_filename")?)?;
                            LuaValue::String(lua.create_string(format!("{}{}", normalized, pathsep))?)
                        } else {
                            LuaValue::Nil
                        }
                    } else {
                        let common: LuaTable = require_table(lua, "core.common")?;
                        let dirname: String = common.call_function("dirname", item.get::<String>("abs_filename")?)?;
                        if let LuaValue::Table(ref p) = project {
                            let normalized: String = p.call_method("normalize_path", dirname)?;
                            LuaValue::String(lua.create_string(format!("{}{}", normalized, pathsep))?)
                        } else {
                            LuaValue::Nil
                        }
                    }
                } else {
                    LuaValue::Nil
                };
                let core: LuaTable = require_table(lua, "core")?;
                let command_view: LuaTable = core.get("command_view")?;
                let inv_k2 = Arc::clone(&inv_k);
                let opts = lua.create_table()?;
                if !matches!(text, LuaValue::Nil) {
                    opts.set("text", text)?;
                }
                let submit = lua.create_function(move |lua, filename: String| {
                    let core: LuaTable = require_table(lua, "core")?;
                    let project: LuaValue = core.get("_treeview_newfile_project")?;
                    let doc_filename: String = if let LuaValue::Table(ref p) = project {
                        p.call_method("absolute_path", filename.clone())?
                    } else {
                        filename.clone()
                    };
                    let io: LuaTable = lua.globals().get("io")?;
                    let result: LuaMultiValue = io.call_function("open", (doc_filename.clone(), "a+"))?;
                    let mut vals = result.into_iter();
                    let file = vals.next().unwrap_or(LuaValue::Nil);
                    if matches!(file, LuaValue::Nil) {
                        let err = match vals.next() {
                            Some(LuaValue::String(s)) => s.to_string_lossy().to_string(),
                            _ => "unknown error".to_string(),
                        };
                        let err_fn: LuaFunction = core.get("error")?;
                        err_fn.call::<()>(format!("Error: unable to create a new file in \"{}\": {}", doc_filename, err))?;
                        return Ok(());
                    }
                    if let LuaValue::UserData(ref ud) = file {
                        ud.call_method::<()>("close", ())?;
                    }
                    let v: LuaTable = core.get("_treeview_view_ref2")?;
                    v.call_method::<()>("open_doc", doc_filename.clone())?;
                    let log_fn: LuaFunction = core.get("log")?;
                    log_fn.call::<()>(format!("Created {}", doc_filename))?;
                    let inv_fn: LuaFunction = lua.registry_value(&inv_k2)?;
                    inv_fn.call::<()>(core.get::<LuaValue>("_treeview_newfile_project")?)
                })?;
                opts.set("submit", submit)?;

                let project_path: Option<String> = if let LuaValue::Table(ref p) = item.get::<LuaValue>("project")? {
                    p.get("path")?
                } else {
                    None
                };
                let pp = project_path.clone();
                let suggest = lua.create_function(move |lua, text: String| -> LuaResult<LuaValue> {
                    let common: LuaTable = require_table(lua, "core.common")?;
                    common.call_function("path_suggest", (text, pp.clone()))
                })?;
                opts.set("suggest", suggest)?;

                core.set("_treeview_newfile_project", item.get::<LuaValue>("project")?)?;
                let v: LuaTable = lua.registry_value(&lua.globals().get::<LuaTable>("core")?.get::<mlua::RegistryKey>("_treeview_view_key").unwrap_or(lua.create_registry_value(lua.create_table()?)?))
                    .unwrap_or(lua.create_table()?);
                // We need the view ref - get it from core
                // Actually let's use a different approach - store view ref in core
                let _ = v;
                command_view.call_method::<()>("enter", ("Filename", opts))?;
                Ok(())
            })?
        })?;

        cmds.set("treeview:new-folder", {
            let inv_k = Arc::clone(&inv_k);
            let ipfk = Arc::clone(&ipfk);
            lua.create_function(move |lua, item: LuaTable| {
                let ipf: LuaFunction = lua.registry_value(&ipfk)?;
                let is_pf: bool = ipf.call(item.clone())?;
                let text: LuaValue = if !is_pf {
                    let item_type: String = item.get("type")?;
                    let project: LuaValue = item.get("project")?;
                    let pathsep: String = lua.globals().get("PATHSEP")?;
                    if item_type == "dir" {
                        if let LuaValue::Table(ref p) = project {
                            let normalized: String = p.call_method("normalize_path", item.get::<String>("abs_filename")?)?;
                            LuaValue::String(lua.create_string(format!("{}{}", normalized, pathsep))?)
                        } else {
                            LuaValue::Nil
                        }
                    } else {
                        let common: LuaTable = require_table(lua, "core.common")?;
                        let dirname: String = common.call_function("dirname", item.get::<String>("abs_filename")?)?;
                        if let LuaValue::Table(ref p) = project {
                            let normalized: String = p.call_method("normalize_path", dirname)?;
                            LuaValue::String(lua.create_string(format!("{}{}", normalized, pathsep))?)
                        } else {
                            LuaValue::Nil
                        }
                    }
                } else {
                    LuaValue::Nil
                };
                let core: LuaTable = require_table(lua, "core")?;
                let command_view: LuaTable = core.get("command_view")?;
                let inv_k2 = Arc::clone(&inv_k);
                let opts = lua.create_table()?;
                if !matches!(text, LuaValue::Nil) {
                    opts.set("text", text)?;
                }
                let submit = lua.create_function(move |lua, filename: String| {
                    let core: LuaTable = require_table(lua, "core")?;
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let project: LuaValue = core.get("_treeview_newfolder_project")?;
                    let dir_path: String = if let LuaValue::Table(ref p) = project {
                        p.call_method("absolute_path", filename.clone())?
                    } else {
                        filename.clone()
                    };
                    let result: LuaMultiValue = common.call_function("mkdirp", dir_path.clone())?;
                    let mut vals = result.into_iter();
                    let created = !matches!(vals.next(), Some(LuaValue::Boolean(false)) | Some(LuaValue::Nil));
                    if !created {
                        let err = match vals.next() {
                            Some(LuaValue::String(s)) => s.to_string_lossy().to_string(),
                            _ => "unknown error".to_string(),
                        };
                        let err_path = match vals.next() {
                            Some(LuaValue::String(s)) => s.to_string_lossy().to_string(),
                            _ => dir_path.clone(),
                        };
                        let err_fn: LuaFunction = core.get("error")?;
                        err_fn.call::<()>(format!("Error: unable to create folder \"{}\": {} ({})", dir_path, err, err_path))?;
                        return Ok(());
                    }
                    let log_fn: LuaFunction = core.get("log")?;
                    log_fn.call::<()>(format!("Created {}", dir_path))?;
                    let inv_fn: LuaFunction = lua.registry_value(&inv_k2)?;
                    inv_fn.call::<()>(core.get::<LuaValue>("_treeview_newfolder_project")?)
                })?;
                opts.set("submit", submit)?;

                let project_path: Option<String> = if let LuaValue::Table(ref p) = item.get::<LuaValue>("project")? {
                    p.get("path")?
                } else {
                    None
                };
                let pp = project_path.clone();
                let suggest = lua.create_function(move |lua, text: String| -> LuaResult<LuaValue> {
                    let common: LuaTable = require_table(lua, "core.common")?;
                    common.call_function("path_suggest", (text, pp.clone()))
                })?;
                opts.set("suggest", suggest)?;

                core.set("_treeview_newfolder_project", item.get::<LuaValue>("project")?)?;
                command_view.call_method::<()>("enter", ("Folder Name", opts))?;
                Ok(())
            })?
        })?;

        cmds.set(
            "treeview:open-in-system",
            lua.create_function(|lua, item: LuaTable| {
                let abs_filename: String = item.get("abs_filename")?;
                let platform: String = lua.globals().get("PLATFORM")?;
                let system: LuaTable = lua.globals().get("system")?;
                let exec: LuaFunction = system.get("exec")?;
                let cmd = if platform == "Windows" {
                    format!("start \"\" {:?}", abs_filename)
                } else if platform.contains("Mac") {
                    format!("open {:?}", abs_filename)
                } else {
                    format!("xdg-open {:?}", abs_filename)
                };
                exec.call::<()>(cmd)?;
                Ok(())
            })?,
        )?;

        command.call_function::<()>("add", (pred, cmds))?;
    }

    // ── treeview:remove-project-directory ─────────────────────────────────────
    {
        let vk = Arc::clone(&view_key);
        let tik = Arc::clone(&treeitem_key);
        let ipfk = Arc::clone(&is_project_folder_key);
        let ipk = Arc::clone(&is_primary_key);
        let pred = lua.create_function(move |lua, active_view: LuaValue| {
            let ti_fn: LuaFunction = lua.registry_value(&tik)?;
            let item: LuaValue = ti_fn.call(())?;
            if let LuaValue::Table(ref t) = item {
                let abs_filename: String = t.get("abs_filename")?;
                let ip_fn: LuaFunction = lua.registry_value(&ipk)?;
                let is_primary: bool = ip_fn.call(abs_filename)?;
                if is_primary {
                    return Ok((false, LuaValue::Nil));
                }
                let ipf_fn: LuaFunction = lua.registry_value(&ipfk)?;
                let is_pf: bool = ipf_fn.call(t.clone())?;
                if !is_pf {
                    return Ok((false, LuaValue::Nil));
                }
                let v: LuaTable = lua.registry_value(&vk)?;
                let av = match active_view {
                    LuaValue::Table(t) => t,
                    _ => {
                        let core: LuaTable = require_table(lua, "core")?;
                        core.get("active_view")?
                    }
                };
                if av == v {
                    return Ok((true, item));
                }
            }
            Ok((false, LuaValue::Nil))
        })?;

        let cmds = lua.create_table()?;
        cmds.set(
            "treeview:remove-project-directory",
            lua.create_function(|lua, item: LuaTable| {
                let project: LuaValue = item.get("project")?;
                let core: LuaTable = require_table(lua, "core")?;
                let remove: LuaFunction = core.get("remove_project")?;
                remove.call::<()>(project)
            })?,
        )?;
        command.call_function::<()>("add", (pred, cmds))?;
    }

    // ── Keybindings ──────────────────────────────────────────────────────────
    let keybinds = lua.create_table()?;
    keybinds.set("ctrl+\\", "treeview:toggle")?;
    keybinds.set("ctrl+h", "treeview:toggle-hidden")?;
    keybinds.set("ctrl+i", "treeview:toggle-ignored")?;
    keybinds.set("up", "treeview:previous")?;
    keybinds.set("down", "treeview:next")?;
    keybinds.set("left", "treeview:collapse")?;
    keybinds.set("right", "treeview:expand")?;
    keybinds.set("return", "treeview:open")?;
    keybinds.set("escape", "treeview:deselect")?;
    keybinds.set("delete", "treeview:delete")?;
    keybinds.set("ctrl+return", "treeview:new-folder")?;
    keybinds.set("lclick", "treeview:select-and-open")?;
    keybinds.set("mclick", "treeview:select")?;
    keybinds.set("ctrl+lclick", "treeview:new-folder")?;
    keymap.call_function::<()>("add", keybinds)?;

    // ── Config spec ──────────────────────────────────────────────────────────
    let tv_cfg: LuaTable = plugins.get("treeview")?;
    let config_spec = lua.create_table()?;
    config_spec.set("name", "Treeview")?;

    let size_spec = lua.create_table()?;
    size_spec.set("label", "Size")?;
    size_spec.set("description", "Default treeview width.")?;
    size_spec.set("path", "size")?;
    size_spec.set("type", "number")?;

    let toolbar_min = if let LuaValue::Table(ref tb) = toolbar_view_val {
        let min_w: f64 = tb.call_method("get_min_width", ())?;
        min_w
    } else {
        200.0 * scale
    };
    let default_size = if matches!(toolbar_view_val, LuaValue::Table(_)) {
        (toolbar_min / scale).ceil()
    } else {
        200.0 * scale
    };
    size_spec.set("default", default_size)?;
    let min_size = if matches!(toolbar_view_val, LuaValue::Table(_)) {
        toolbar_min / scale
    } else {
        200.0 * scale
    };
    size_spec.set("min", min_size)?;
    size_spec.set(
        "get_value",
        lua.create_function(|lua, value: f64| {
            let s: f64 = lua.globals().get("SCALE")?;
            Ok(value / s)
        })?,
    )?;
    size_spec.set(
        "set_value",
        lua.create_function(|lua, value: f64| {
            let s: f64 = lua.globals().get("SCALE")?;
            Ok(value * s)
        })?,
    )?;
    let vk_apply = Arc::clone(&view_key);
    let tb_val_clone = toolbar_view_val.clone();
    size_spec.set(
        "on_apply",
        lua.create_function(move |lua, value: f64| {
            let v: LuaTable = lua.registry_value(&vk_apply)?;
            let min = if let LuaValue::Table(ref tb) = tb_val_clone {
                let min_w: f64 = tb.call_method("get_min_width", ())?;
                min_w
            } else {
                let s: f64 = lua.globals().get("SCALE")?;
                200.0 * s
            };
            v.call_method::<LuaValue>("set_target_size", ("x", value.max(min)))?;
            Ok(())
        })?,
    )?;
    config_spec.raw_set(1, size_spec)?;

    let hide_spec = lua.create_table()?;
    hide_spec.set("label", "Hide on Startup")?;
    hide_spec.set("description", "Show or hide the treeview on startup.")?;
    hide_spec.set("path", "visible")?;
    hide_spec.set("type", "toggle")?;
    hide_spec.set("default", false)?;
    let vk_hide = Arc::clone(&view_key);
    hide_spec.set(
        "on_apply",
        lua.create_function(move |lua, value: bool| {
            let v: LuaTable = lua.registry_value(&vk_hide)?;
            v.set("visible", !value)
        })?,
    )?;
    config_spec.raw_set(2, hide_spec)?;
    tv_cfg.set("config_spec", config_spec)?;

    view.set("toolbar", toolbar_view_val)?;

    // Store view ref for new-file command
    core.set("_treeview_view_ref2", view.clone())?;

    Ok(LuaValue::Table(view))
}
