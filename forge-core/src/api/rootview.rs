use mlua::prelude::*;

use std::sync::Arc;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Computes the sub-rectangle for a drag-split overlay based on split direction.
fn split_rect(split_type: &str, x: f64, y: f64, w: f64, h: f64) -> (f64, f64, f64, f64) {
    match split_type {
        "left" => (x, y, w * 0.5, h),
        "right" => (x + w * 0.5, y, w * 0.5, h),
        "up" => (x, y, w, h * 0.5),
        "down" => (x, y + h * 0.5, w, h * 0.5),
        _ => (x, y, w, h),
    }
}

/// Recursively collects DocView instances from the node tree.
fn collect_docviews(lua: &Lua, node: &LuaTable, out: &LuaTable, set: &LuaTable) -> LuaResult<()> {
    let docview_class: LuaTable = require_table(lua, "core.docview")?;
    let node_type: String = node.get("type")?;
    if node_type == "leaf" {
        let views: LuaTable = node.get::<Option<LuaTable>>("views")?.unwrap_or(lua.create_table()?);
        for pair in views.sequence_values::<LuaTable>() {
            let view = pair?;
            let is_doc: bool = view.call_method("is", docview_class.clone())?;
            if is_doc {
                let already: LuaValue = set.get(view.clone())?;
                if matches!(already, LuaValue::Nil) {
                    let len = out.raw_len();
                    out.raw_set(len + 1, view.clone())?;
                    set.set(view, true)?;
                }
            }
        }
    } else {
        let a: LuaTable = node.get("a")?;
        let b: LuaTable = node.get("b")?;
        collect_docviews(lua, &a, out, set)?;
        collect_docviews(lua, &b, out, set)?;
    }
    Ok(())
}

/// Recursively serializes node tree using view-to-id mapping.
fn serialize_node_ids(lua: &Lua, node: &LuaTable, view_to_id: &LuaTable) -> LuaResult<LuaTable> {
    let emptyview_class: LuaTable = require_table(lua, "core.emptyview")?;
    let state = lua.create_table()?;
    let node_type: String = node.get("type")?;
    state.set("type", node_type.as_str())?;
    state.set("divider", node.get::<LuaValue>("divider")?)?;
    state.set("locked", node.get::<LuaValue>("locked")?)?;
    state.set("resizable", node.get::<LuaValue>("resizable")?)?;
    state.set("is_primary_node", node.get::<LuaValue>("is_primary_node")?)?;

    if node_type == "leaf" {
        let views_tbl = lua.create_table()?;
        let active_view: LuaValue = node.get("active_view")?;
        let active_id: LuaValue = if let LuaValue::Table(ref av) = active_view {
            view_to_id.get(av.clone())?
        } else {
            LuaValue::Nil
        };
        state.set("active_view", active_id)?;
        state.set("tab_offset", node.get::<LuaValue>("tab_offset")?)?;

        let views: LuaTable =
            node.get::<Option<LuaTable>>("views")?.unwrap_or(lua.create_table()?);
        for pair in views.sequence_values::<LuaTable>() {
            let view = pair?;
            let is_empty: bool = view.call_method("is", emptyview_class.clone())?;
            if !is_empty {
                let entry = lua.create_table()?;
                let id: LuaValue = view_to_id.get(view.clone())?;
                let docview_class: LuaTable = require_table(lua, "core.docview")?;
                let is_doc: bool = view.call_method("is", docview_class)?;
                entry.set("id", id)?;
                entry.set("doc", is_doc)?;
                let len = views_tbl.raw_len();
                views_tbl.raw_set(len + 1, entry)?;
            }
        }
        state.set("views", views_tbl)?;
    } else {
        let a: LuaTable = node.get("a")?;
        let b: LuaTable = node.get("b")?;
        state.set("a", serialize_node_ids(lua, &a, view_to_id)?)?;
        state.set("b", serialize_node_ids(lua, &b, view_to_id)?)?;
    }
    Ok(state)
}

/// Builds bidirectional view-to-id and id-to-view maps for a node tree.
fn build_view_maps(
    lua: &Lua,
    node: &LuaTable,
    view_to_id: &LuaTable,
    views_by_id: &LuaTable,
    next_id: &mut i64,
) -> LuaResult<()> {
    let emptyview_class: LuaTable = require_table(lua, "core.emptyview")?;
    let node_type: String = node.get("type")?;
    if node_type == "leaf" {
        let views: LuaTable =
            node.get::<Option<LuaTable>>("views")?.unwrap_or(lua.create_table()?);
        for pair in views.sequence_values::<LuaTable>() {
            let view = pair?;
            let is_empty: bool = view.call_method("is", emptyview_class.clone())?;
            if !is_empty {
                let existing: LuaValue = view_to_id.get(view.clone())?;
                if matches!(existing, LuaValue::Nil) {
                    view_to_id.set(view.clone(), *next_id)?;
                    views_by_id.set(*next_id, view)?;
                    *next_id += 1;
                }
            }
        }
    } else {
        let a: LuaTable = node.get("a")?;
        let b: LuaTable = node.get("b")?;
        build_view_maps(lua, &a, view_to_id, views_by_id, next_id)?;
        build_view_maps(lua, &b, view_to_id, views_by_id, next_id)?;
    }
    Ok(())
}

/// Collects live view IDs from the node tree.
fn collect_live_view_ids(
    lua: &Lua,
    node: &LuaTable,
    view_to_id: &LuaTable,
    only_docviews: bool,
) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    let set = lua.create_table()?;
    collect_live_view_ids_inner(lua, node, view_to_id, only_docviews, &out, &set)?;
    Ok(out)
}

fn collect_live_view_ids_inner(
    lua: &Lua,
    node: &LuaTable,
    view_to_id: &LuaTable,
    only_docviews: bool,
    out: &LuaTable,
    set: &LuaTable,
) -> LuaResult<()> {
    let emptyview_class: LuaTable = require_table(lua, "core.emptyview")?;
    let node_type: String = node.get("type")?;
    if node_type == "leaf" {
        let docview_class: LuaTable = require_table(lua, "core.docview")?;
        let views: LuaTable =
            node.get::<Option<LuaTable>>("views")?.unwrap_or(lua.create_table()?);
        for pair in views.sequence_values::<LuaTable>() {
            let view = pair?;
            let is_empty: bool = view.call_method("is", emptyview_class.clone())?;
            if is_empty {
                continue;
            }
            if only_docviews {
                let is_doc: bool = view.call_method("is", docview_class.clone())?;
                if !is_doc {
                    continue;
                }
            }
            let id: LuaValue = view_to_id.get(view)?;
            if let LuaValue::Integer(id_val) = id {
                let already: LuaValue = set.get(id_val)?;
                if matches!(already, LuaValue::Nil) {
                    let len = out.raw_len();
                    out.raw_set(len + 1, id_val)?;
                    set.set(id_val, true)?;
                }
            }
        }
    } else {
        let a: LuaTable = node.get("a")?;
        let b: LuaTable = node.get("b")?;
        collect_live_view_ids_inner(lua, &a, view_to_id, only_docviews, out, set)?;
        collect_live_view_ids_inner(lua, &b, view_to_id, only_docviews, out, set)?;
    }
    Ok(())
}

/// Restores a node tree from serialized ID state.
fn restore_node_from_ids(
    lua: &Lua,
    state: &LuaTable,
    views_by_id: &LuaTable,
) -> LuaResult<LuaTable> {
    let node_class: LuaTable = require_table(lua, "core.node")?;
    let emptyview_class: LuaTable = require_table(lua, "core.emptyview")?;
    let common: LuaTable = require_table(lua, "core.common")?;
    let state_type: String = state.get("type")?;

    let node: LuaTable = if state_type == "leaf" {
        node_class.call(LuaValue::Nil)?
    } else {
        node_class.call(state_type.as_str())?
    };
    node.set("is_primary_node", state.get::<LuaValue>("is_primary_node")?)?;

    if state_type == "leaf" {
        node.set("views", lua.create_table()?)?;
        node.set("active_view", LuaValue::Nil)?;

        if let Some(view_entries) = state.get::<Option<LuaTable>>("views")? {
            for entry in view_entries.sequence_values::<LuaValue>() {
                let view_id = entry?;
                // view_id can be a table {id=..., doc=...} or just an integer
                let id_val: LuaValue = match &view_id {
                    LuaValue::Table(t) => t.get("id")?,
                    _ => view_id.clone(),
                };
                let view: LuaValue = views_by_id.get(id_val)?;
                if let LuaValue::Table(ref v) = view {
                    node.call_method::<()>("add_view", v.clone())?;
                }
            }
        }

        let node_views: LuaTable = node.get("views")?;
        if node_views.raw_len() == 0 {
            let empty: LuaTable = emptyview_class.call(())?;
            node.call_method::<()>("add_view", empty)?;
        } else {
            let active_id: LuaValue = state.get("active_view")?;
            if !matches!(active_id, LuaValue::Nil) {
                let active_view: LuaValue = views_by_id.get(active_id)?;
                if let LuaValue::Table(ref av) = active_view {
                    let idx: LuaValue = node.call_method("get_view_idx", av.clone())?;
                    if !matches!(idx, LuaValue::Nil | LuaValue::Boolean(false)) {
                        node.call_method::<()>("set_active_view", av.clone())?;
                    }
                }
            }
        }

        let tab_offset: i64 = state.get::<Option<i64>>("tab_offset")?.unwrap_or(1);
        let views_len = node_views.raw_len().max(1) as i64;
        let clamped: i64 = common.call_function("clamp", (tab_offset, 1, views_len))?;
        node.set("tab_offset", clamped)?;
        node.set("locked", state.get::<LuaValue>("locked")?)?;
        node.set("resizable", state.get::<LuaValue>("resizable")?)?;
    } else {
        let a_state: LuaTable = state.get("a")?;
        let b_state: LuaTable = state.get("b")?;
        node.set("a", restore_node_from_ids(lua, &a_state, views_by_id)?)?;
        node.set("b", restore_node_from_ids(lua, &b_state, views_by_id)?)?;
        node.set(
            "divider",
            state.get::<Option<f64>>("divider")?.unwrap_or(0.5),
        )?;
        node.set("locked", state.get::<LuaValue>("locked")?)?;
        node.set("resizable", state.get::<LuaValue>("resizable")?)?;
    }
    Ok(node)
}

/// Returns the edge node for a given placement if applicable.
fn get_edge_node(root: &LuaTable, placement: &str) -> LuaResult<Option<LuaTable>> {
    let target = if placement == "left" || placement == "top" {
        "a"
    } else {
        "b"
    };
    let split_type = match placement {
        "left" | "right" => "hsplit",
        "top" | "bottom" => "vsplit",
        _ => return Ok(None),
    };
    let root_type: String = root.get("type")?;
    if root_type == split_type {
        if let Some(child) = root.get::<Option<LuaTable>>(target)? {
            let child_type: String = child.get("type")?;
            let locked: LuaValue = child.get("locked")?;
            if child_type == "leaf" && matches!(locked, LuaValue::Nil | LuaValue::Boolean(false)) {
                return Ok(Some(child));
            }
        }
    }
    Ok(None)
}

/// Recursively finds the primary node in the tree.
fn get_primary_node_recursive(node: &LuaTable) -> LuaResult<Option<LuaTable>> {
    let is_primary: bool = node
        .get::<Option<bool>>("is_primary_node")?
        .unwrap_or(false);
    if is_primary {
        return Ok(Some(node.clone()));
    }
    let node_type: String = node.get("type")?;
    if node_type != "leaf" {
        let a: LuaTable = node.get("a")?;
        if let Some(found) = get_primary_node_recursive(&a)? {
            return Ok(Some(found));
        }
        let b: LuaTable = node.get("b")?;
        return get_primary_node_recursive(&b);
    }
    Ok(None)
}

/// Finds the next candidate for primary node (unlocked leaf).
fn select_next_primary_node_recursive(node: &LuaTable) -> LuaResult<Option<LuaTable>> {
    let is_primary: bool = node
        .get::<Option<bool>>("is_primary_node")?
        .unwrap_or(false);
    if is_primary {
        return Ok(None);
    }
    let node_type: String = node.get("type")?;
    if node_type != "leaf" {
        let a: LuaTable = node.get("a")?;
        if let Some(found) = select_next_primary_node_recursive(&a)? {
            return Ok(Some(found));
        }
        let b: LuaTable = node.get("b")?;
        return select_next_primary_node_recursive(&b);
    }
    // leaf node
    let (lx, ly): (LuaValue, LuaValue) = node.call_method("get_locked_size", ())?;
    if matches!(lx, LuaValue::Nil | LuaValue::Boolean(false))
        && matches!(ly, LuaValue::Nil | LuaValue::Boolean(false))
    {
        return Ok(Some(node.clone()));
    }
    Ok(None)
}

/// Determines whether a view has context == "session".
fn is_session_view(view: &LuaTable) -> LuaResult<bool> {
    let ctx: LuaValue = view.get("context")?;
    match ctx {
        LuaValue::String(s) => Ok(s.to_str()? == "session"),
        _ => Ok(false),
    }
}

/// Resizes a divider's child node, used during drag.
fn resize_child_node(
    node: &LuaTable,
    axis: &str,
    value: f64,
    delta: f64,
) -> LuaResult<()> {
    let a: LuaTable = node.get("a")?;
    let accept: bool = a
        .call_method::<LuaValue>("resize", (axis, value))?
        .as_boolean()
        .unwrap_or(false);
    if !accept {
        let b: LuaTable = node.get("b")?;
        let size: LuaTable = node.get("size")?;
        let size_val: f64 = size.get(axis)?;
        let accept2: bool = b
            .call_method::<LuaValue>("resize", (axis, size_val - value))?
            .as_boolean()
            .unwrap_or(false);
        if !accept2 {
            let size: LuaTable = node.get("size")?;
            let size_axis: f64 = size.get(axis)?;
            let divider: f64 = node.get("divider")?;
            node.set("divider", divider + delta / size_axis)?;
        }
    }
    Ok(())
}

/// Registers `core.rootview` — routes all input events to the node tree and manages overlays.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "core.rootview",
        lua.create_function(|lua, ()| build_rootview(lua))?,
    )?;
    Ok(())
}

fn build_rootview(lua: &Lua) -> LuaResult<LuaTable> {
    let view_class: LuaTable = require_table(lua, "core.view")?;
    let root_view = view_class.call_method::<LuaTable>("extend", ())?;

    root_view.set(
        "__tostring",
        lua.create_function(|_lua, _self: LuaTable| Ok("RootView"))?,
    )?;

    let class_key = Arc::new(lua.create_registry_value(root_view.clone())?);

    // RootView:new()
    root_view.set("new", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, this: LuaTable| {
            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_new: LuaFunction = super_tbl.get("new")?;
            super_new.call::<()>(this.clone())?;

            let node_class: LuaTable = require_table(lua, "core.node")?;
            let root_node: LuaTable = node_class.call(())?;
            this.set("root_node", root_node)?;

            this.set("deferred_draws", lua.create_table()?)?;

            let mouse = lua.create_table()?;
            mouse.set("x", 0.0)?;
            mouse.set("y", 0.0)?;
            this.set("mouse", mouse)?;

            // drag_overlay
            let style: LuaTable = require_table(lua, "core.style")?;
            let drag_overlay_color: LuaTable = style.get("drag_overlay")?;
            let drag_overlay = lua.create_table()?;
            drag_overlay.set("x", 0.0)?;
            drag_overlay.set("y", 0.0)?;
            drag_overlay.set("w", 0.0)?;
            drag_overlay.set("h", 0.0)?;
            drag_overlay.set("visible", false)?;
            drag_overlay.set("opacity", 0.0)?;
            drag_overlay.set("base_color", drag_overlay_color.clone())?;
            let color = lua.create_table()?;
            color.raw_set(1, drag_overlay_color.get::<LuaValue>(1)?)?;
            color.raw_set(2, drag_overlay_color.get::<LuaValue>(2)?)?;
            color.raw_set(3, drag_overlay_color.get::<LuaValue>(3)?)?;
            color.raw_set(4, drag_overlay_color.get::<LuaValue>(4)?)?;
            drag_overlay.set("color", color)?;
            let to = lua.create_table()?;
            to.set("x", 0.0)?;
            to.set("y", 0.0)?;
            to.set("w", 0.0)?;
            to.set("h", 0.0)?;
            drag_overlay.set("to", to)?;
            this.set("drag_overlay", drag_overlay)?;

            // drag_overlay_tab
            let drag_overlay_tab_color: LuaTable = style.get("drag_overlay_tab")?;
            let drag_overlay_tab = lua.create_table()?;
            drag_overlay_tab.set("x", 0.0)?;
            drag_overlay_tab.set("y", 0.0)?;
            drag_overlay_tab.set("w", 0.0)?;
            drag_overlay_tab.set("h", 0.0)?;
            drag_overlay_tab.set("visible", false)?;
            drag_overlay_tab.set("opacity", 0.0)?;
            drag_overlay_tab.set("base_color", drag_overlay_tab_color.clone())?;
            let color_tab = lua.create_table()?;
            color_tab.raw_set(1, drag_overlay_tab_color.get::<LuaValue>(1)?)?;
            color_tab.raw_set(2, drag_overlay_tab_color.get::<LuaValue>(2)?)?;
            color_tab.raw_set(3, drag_overlay_tab_color.get::<LuaValue>(3)?)?;
            color_tab.raw_set(4, drag_overlay_tab_color.get::<LuaValue>(4)?)?;
            drag_overlay_tab.set("color", color_tab)?;
            let to_tab = lua.create_table()?;
            to_tab.set("x", 0.0)?;
            to_tab.set("y", 0.0)?;
            to_tab.set("w", 0.0)?;
            to_tab.set("h", 0.0)?;
            drag_overlay_tab.set("to", to_tab)?;
            this.set("drag_overlay_tab", drag_overlay_tab)?;

            this.set("grab", LuaValue::Nil)?;
            this.set("overlapping_view", LuaValue::Nil)?;
            this.set("touched_view", LuaValue::Nil)?;
            this.set("defer_open_docs", lua.create_table()?)?;
            this.set("first_dnd_processed", false)?;
            this.set("first_update_done", false)?;

            let context_menu_class: LuaTable = require_table(lua, "core.contextmenu")?;
            let ctx_menu: LuaTable = context_menu_class.call(())?;
            this.set("context_menu", ctx_menu)?;
            this.set("focus_mode", LuaValue::Nil)?;

            Ok(())
        })?
    })?;

    // RootView:defer_draw(fn, ...)
    root_view.set(
        "defer_draw",
        lua.create_function(|lua, (this, func, rest): (LuaTable, LuaFunction, LuaMultiValue)| {
            let deferred: LuaTable = this.get("deferred_draws")?;
            let entry = lua.create_table()?;
            entry.set("fn", func)?;
            for (i, val) in rest.into_iter().enumerate() {
                entry.raw_set((i + 1) as i64, val)?;
            }
            // Insert at position 1 (prepend)
            let table_mod: LuaTable = lua.globals().get("table")?;
            let insert: LuaFunction = table_mod.get("insert")?;
            insert.call::<()>((deferred, 1, entry))?;
            Ok(())
        })?,
    )?;

    // RootView:get_active_node()
    root_view.set("get_active_node", {
        lua.create_function(|lua, this: LuaTable| {
            let core: LuaTable = require_table(lua, "core")?;
            let root_node: LuaTable = this.get("root_node")?;
            let active_view: LuaValue = core.get("active_view")?;
            let node: LuaValue = root_node.call_method("get_node_for_view", active_view)?;
            if matches!(node, LuaValue::Nil | LuaValue::Boolean(false)) {
                let primary: LuaTable = this.call_method("get_primary_node", ())?;
                return Ok(primary);
            }
            match node {
                LuaValue::Table(t) => Ok(t),
                _ => this.call_method("get_primary_node", ()),
            }
        })?
    })?;

    // RootView:get_active_node_default()
    root_view.set("get_active_node_default", {
        lua.create_function(|lua, this: LuaTable| {
            let core: LuaTable = require_table(lua, "core")?;
            let root_node: LuaTable = this.get("root_node")?;
            let active_view: LuaValue = core.get("active_view")?;
            let node_val: LuaValue = root_node.call_method("get_node_for_view", active_view)?;
            let mut node: LuaTable = if matches!(node_val, LuaValue::Nil | LuaValue::Boolean(false))
            {
                this.call_method("get_primary_node", ())?
            } else {
                match node_val {
                    LuaValue::Table(t) => t,
                    _ => this.call_method("get_primary_node", ())?,
                }
            };
            let locked: LuaValue = node.get("locked")?;
            if !matches!(locked, LuaValue::Nil | LuaValue::Boolean(false)) {
                let primary: LuaTable = this.call_method("get_primary_node", ())?;
                let views: LuaTable = primary.get("views")?;
                let default_view: LuaValue = views.get(1)?;
                if matches!(default_view, LuaValue::Nil) {
                    return Err(LuaError::runtime(
                        "internal error: cannot find original document node.",
                    ));
                }
                let set_active: LuaFunction = core.get("set_active_view")?;
                set_active.call::<()>(default_view)?;
                node = this.call_method("get_active_node", ())?;
            }
            Ok(node)
        })?
    })?;

    // RootView:get_primary_node()
    root_view.set(
        "get_primary_node",
        lua.create_function(|_lua, this: LuaTable| {
            let root_node: LuaTable = this.get("root_node")?;
            get_primary_node_recursive(&root_node)?
                .ok_or_else(|| LuaError::runtime("no primary node found"))
        })?,
    )?;

    // RootView:select_next_primary_node()
    root_view.set(
        "select_next_primary_node",
        lua.create_function(|_lua, this: LuaTable| {
            let root_node: LuaTable = this.get("root_node")?;
            select_next_primary_node_recursive(&root_node)
        })?,
    )?;

    // RootView:open_doc(doc)
    root_view.set(
        "open_doc",
        lua.create_function(|lua, (this, doc): (LuaTable, LuaTable)| {
            let node: LuaTable = this.call_method("get_active_node_default", ())?;
            let views: LuaTable = node.get("views")?;
            for pair in views.pairs::<i64, LuaTable>() {
                let (_, view) = pair?;
                let view_doc: LuaValue = view.get("doc")?;
                if let LuaValue::Table(ref vd) = view_doc {
                    if *vd == doc {
                        node.call_method::<()>("set_active_view", view.clone())?;
                        return Ok(view);
                    }
                }
            }
            let docview_class: LuaTable = require_table(lua, "core.docview")?;
            let view: LuaTable = docview_class.call(doc)?;
            node.call_method::<()>("add_view", view.clone())?;
            let root_node: LuaTable = this.get("root_node")?;
            root_node.call_method::<()>("update_layout", ())?;
            let view_doc: LuaTable = view.get("doc")?;
            let (line,): (LuaValue,) = view_doc.call_method("get_selection", ())?;
            view.call_method::<()>("scroll_to_line", (line, true, true))?;
            Ok(view)
        })?,
    )?;

    // RootView:add_view(view, placement)
    root_view.set("add_view", {
        lua.create_function(|lua, (this, view, placement): (LuaTable, LuaTable, Option<String>)| {
            let placement = placement.unwrap_or_else(|| "tab".to_string());
            let core: LuaTable = require_table(lua, "core")?;
            let docview_class: LuaTable = require_table(lua, "core.docview")?;
            let root_node: LuaTable = this.get("root_node")?;

            // Exit focus mode for non-tab or non-docview
            let focus_mode: LuaValue = this.get("focus_mode")?;
            if !matches!(focus_mode, LuaValue::Nil | LuaValue::Boolean(false)) {
                let is_doc: bool = view.call_method("is", docview_class)?;
                if placement != "tab" || !is_doc {
                    this.call_method::<()>("exit_focus_mode", ())?;
                }
            }

            if placement == "tab" {
                let node: LuaTable = this.call_method("get_active_node_default", ())?;
                node.call_method::<()>("add_view", view.clone())?;
                root_node.call_method::<()>("update_layout", ())?;
                let set_active: LuaFunction = core.get("set_active_view")?;
                set_active.call::<()>(view.clone())?;
                return Ok(view);
            }

            if let Some(edge) = get_edge_node(&root_node, &placement)? {
                edge.call_method::<()>("add_view", view.clone())?;
                root_node.call_method::<()>("update_layout", ())?;
                let set_active: LuaFunction = core.get("set_active_view")?;
                set_active.call::<()>(view.clone())?;
                return Ok(view);
            }

            let split_type_str = match placement.as_str() {
                "left" | "right" => "hsplit",
                "top" | "bottom" => "vsplit",
                _ => {
                    return Err(LuaError::runtime(format!(
                        "invalid root placement: {placement}"
                    )))
                }
            };

            let node_class: LuaTable = require_table(lua, "core.node")?;
            let existing: LuaTable = node_class.call(())?;
            existing.call_method::<()>("consume", root_node.clone())?;

            let sibling: LuaTable = node_class.call(())?;
            sibling.set("views", lua.create_table()?)?;
            sibling.call_method::<()>("add_view", view.clone())?;

            let new_root: LuaTable = node_class.call(split_type_str)?;
            new_root.set("a", existing.clone())?;
            new_root.set("b", sibling.clone())?;
            if placement == "left" || placement == "top" {
                new_root.set("a", sibling)?;
                new_root.set("b", existing)?;
            }

            root_node.call_method::<()>("consume", new_root)?;
            root_node.call_method::<()>("update_layout", ())?;
            let set_active: LuaFunction = core.get("set_active_view")?;
            set_active.call::<()>(view.clone())?;
            Ok(view)
        })?
    })?;

    // RootView:get_session_views()
    root_view.set(
        "get_session_views",
        lua.create_function(|lua, this: LuaTable| {
            let emptyview_class: LuaTable = require_table(lua, "core.emptyview")?;
            let views = lua.create_table()?;
            let root_node: LuaTable = this.get("root_node")?;
            fn walk(
                lua: &Lua,
                node: &LuaTable,
                views: &LuaTable,
                emptyview_class: &LuaTable,
            ) -> LuaResult<()> {
                let node_type: String = node.get("type")?;
                if node_type == "leaf" {
                    let node_views: LuaTable = node
                        .get::<Option<LuaTable>>("views")?
                        .unwrap_or(lua.create_table()?);
                    for pair in node_views.sequence_values::<LuaTable>() {
                        let view = pair?;
                        let is_empty: bool =
                            view.call_method("is", emptyview_class.clone())?;
                        if !is_empty && is_session_view(&view)? {
                            let entry = lua.create_table()?;
                            entry.set("node", node.clone())?;
                            entry.set("view", view)?;
                            let len = views.raw_len();
                            views.raw_set(len + 1, entry)?;
                        }
                    }
                } else {
                    let a: LuaTable = node.get("a")?;
                    let b: LuaTable = node.get("b")?;
                    walk(lua, &a, views, emptyview_class)?;
                    walk(lua, &b, views, emptyview_class)?;
                }
                Ok(())
            }
            walk(lua, &root_node, &views, &emptyview_class)?;
            Ok(views)
        })?,
    )?;

    // RootView:is_focus_mode_active()
    root_view.set(
        "is_focus_mode_active",
        lua.create_function(|_lua, this: LuaTable| {
            let fm: LuaValue = this.get("focus_mode")?;
            Ok(!matches!(fm, LuaValue::Nil | LuaValue::Boolean(false)))
        })?,
    )?;

    // RootView:enter_focus_mode()
    root_view.set("enter_focus_mode", {
        lua.create_function(|lua, this: LuaTable| {
            let core: LuaTable = require_table(lua, "core")?;
            let docview_class: LuaTable = require_table(lua, "core.docview")?;
            let node_class: LuaTable = require_table(lua, "core.node")?;

            let active_view: LuaValue = core.get("active_view")?;
            let is_doc = match &active_view {
                LuaValue::Table(av) => {
                    let r: bool = av.call_method("is", docview_class)?;
                    r
                }
                _ => false,
            };
            if !is_doc {
                return Ok(false);
            }

            let fm: LuaValue = this.get("focus_mode")?;
            if !matches!(fm, LuaValue::Nil | LuaValue::Boolean(false)) {
                return Ok(true);
            }

            let root_node: LuaTable = this.get("root_node")?;
            let out = lua.create_table()?;
            let set = lua.create_table()?;
            collect_docviews(lua, &root_node, &out, &set)?;
            if out.raw_len() == 0 {
                return Ok(false);
            }

            let focus_root: LuaTable = node_class.call(LuaValue::Nil)?;
            focus_root.set("views", lua.create_table()?)?;
            focus_root.set("active_view", LuaValue::Nil)?;
            focus_root.set("is_primary_node", true)?;

            for view in out.sequence_values::<LuaTable>() {
                let view = view?;
                focus_root.call_method::<()>("add_view", view)?;
            }

            if let LuaValue::Table(ref av) = active_view {
                let idx: LuaValue = focus_root.call_method("get_view_idx", av.clone())?;
                if !matches!(idx, LuaValue::Nil | LuaValue::Boolean(false)) {
                    focus_root.call_method::<()>("set_active_view", av.clone())?;
                }
            }

            let focus_state = lua.create_table()?;
            focus_state.set("view_to_id", LuaValue::Nil)?;
            focus_state.set("views_by_id", LuaValue::Nil)?;
            focus_state.set("snapshot_ids", LuaValue::Nil)?;
            focus_state.set("previous_active_view", active_view.clone())?;
            focus_state.set("previous_active_view_id", LuaValue::Nil)?;

            let view_to_id = lua.create_table()?;
            let vti_mt = lua.create_table()?;
            vti_mt.set("__mode", "k")?;
            view_to_id.set_metatable(Some(vti_mt))?;
            let views_by_id = lua.create_table()?;
            let mut next_id: i64 = 1;
            build_view_maps(lua, &root_node, &view_to_id, &views_by_id, &mut next_id)?;
            focus_state.set("view_to_id", view_to_id.clone())?;
            focus_state.set("views_by_id", views_by_id)?;

            let snapshot = serialize_node_ids(lua, &root_node, &view_to_id)?;
            focus_state.set("snapshot_ids", snapshot)?;

            let prev_id: LuaValue = if let LuaValue::Table(ref av) = active_view {
                view_to_id.get(av.clone())?
            } else {
                LuaValue::Nil
            };
            focus_state.set("previous_active_view_id", prev_id)?;

            this.set("focus_mode", focus_state)?;
            root_node.call_method::<()>("consume", focus_root)?;
            root_node.call_method::<()>("update_layout", ())?;
            core.set("redraw", true)?;
            Ok(true)
        })?
    })?;

    // RootView:exit_focus_mode()
    root_view.set("exit_focus_mode", {
        lua.create_function(|lua, this: LuaTable| {
            let fm: LuaValue = this.get("focus_mode")?;
            let focus_state = match fm {
                LuaValue::Table(t) => t,
                _ => return Ok(false),
            };

            let core: LuaTable = require_table(lua, "core")?;
            let native_root_model: LuaTable = require_table(lua, "root_model")?;
            let root_node: LuaTable = this.get("root_node")?;
            let view_to_id: LuaTable = focus_state.get("view_to_id")?;
            let views_by_id: LuaTable = focus_state.get("views_by_id")?;
            let snapshot_ids: LuaTable = focus_state.get("snapshot_ids")?;

            let live_doc_ids = collect_live_view_ids(lua, &root_node, &view_to_id, true)?;
            let live_view_ids = collect_live_view_ids(lua, &root_node, &view_to_id, false)?;

            let active_view: LuaValue = core.get("active_view")?;
            let current_active_id: LuaValue = if let LuaValue::Table(ref av) = active_view {
                view_to_id.get(av.clone())?
            } else {
                LuaValue::Nil
            };
            let prev_active_id: LuaValue = focus_state.get("previous_active_view_id")?;

            let restore_fn: LuaFunction = native_root_model.get("restore_focus_layout")?;
            let restored: LuaTable = restore_fn.call((
                snapshot_ids,
                live_doc_ids,
                live_view_ids,
                current_active_id,
                prev_active_id,
            ))?;

            let restored_root_state: LuaTable = restored.get("root")?;
            let restored_root = restore_node_from_ids(lua, &restored_root_state, &views_by_id)?;
            let target_view_id: LuaValue = restored.get("target_view_id")?;
            let mut target_view: LuaValue = if !matches!(target_view_id, LuaValue::Nil) {
                views_by_id.get(target_view_id)?
            } else {
                LuaValue::Nil
            };

            this.set("focus_mode", LuaValue::Nil)?;
            root_node.call_method::<()>("consume", restored_root)?;
            root_node.call_method::<()>("update_layout", ())?;

            if matches!(target_view, LuaValue::Nil) {
                target_view = core.get("active_view")?;
            }
            if let LuaValue::Table(ref tv) = target_view {
                let node_for: LuaValue =
                    root_node.call_method("get_node_for_view", tv.clone())?;
                if matches!(node_for, LuaValue::Nil | LuaValue::Boolean(false)) {
                    target_view = focus_state.get("previous_active_view")?;
                }
            }

            let target_node_val: LuaValue = if let LuaValue::Table(ref tv) = target_view {
                root_node.call_method("get_node_for_view", tv.clone())?
            } else {
                LuaValue::Nil
            };
            let target_node: LuaTable =
                if let LuaValue::Table(tn) = target_node_val {
                    tn
                } else {
                    this.call_method("get_primary_node", ())?
                };

            let av_to_set: LuaValue = if !matches!(target_view, LuaValue::Nil) {
                target_view
            } else {
                let av: LuaValue = target_node.get("active_view")?;
                if !matches!(av, LuaValue::Nil) {
                    av
                } else {
                    let views: LuaTable = target_node.get("views")?;
                    views.get(1)?
                }
            };
            target_node.call_method::<()>("set_active_view", av_to_set)?;
            core.set("redraw", true)?;
            Ok(true)
        })?
    })?;

    // RootView:toggle_focus_mode()
    root_view.set(
        "toggle_focus_mode",
        lua.create_function(|_lua, this: LuaTable| {
            let fm: LuaValue = this.get("focus_mode")?;
            if !matches!(fm, LuaValue::Nil | LuaValue::Boolean(false)) {
                this.call_method::<LuaValue>("exit_focus_mode", ())
            } else {
                this.call_method::<LuaValue>("enter_focus_mode", ())
            }
        })?,
    )?;

    // RootView:close_views(entries)
    root_view.set(
        "close_views",
        lua.create_function(|_lua, (this, entries): (LuaTable, LuaTable)| {
            let root_node: LuaTable = this.get("root_node")?;
            let len = entries.raw_len() as i64;
            for i in (1..=len).rev() {
                let entry: LuaTable = entries.get(i)?;
                let node: LuaValue = entry.get("node")?;
                let view: LuaValue = entry.get("view")?;
                if let (LuaValue::Table(n), LuaValue::Table(v)) = (&node, &view) {
                    let idx: LuaValue = n.call_method("get_view_idx", v.clone())?;
                    if !matches!(idx, LuaValue::Nil | LuaValue::Boolean(false)) {
                        let doc_val: LuaValue = v.get("doc")?;
                        if !matches!(doc_val, LuaValue::Nil | LuaValue::Boolean(false)) {
                            n.call_method::<()>("remove_view", (root_node.clone(), v.clone()))?;
                        } else {
                            let n2 = n.clone();
                            let v2 = v.clone();
                            let rn2 = root_node.clone();
                            let close_fn = _lua.create_function(move |_lua, ()| {
                                let idx2: LuaValue =
                                    n2.call_method("get_view_idx", v2.clone())?;
                                if !matches!(idx2, LuaValue::Nil | LuaValue::Boolean(false)) {
                                    n2.call_method::<()>(
                                        "remove_view",
                                        (rn2.clone(), v2.clone()),
                                    )?;
                                }
                                Ok(())
                            })?;
                            v.call_method::<()>("try_close", close_fn)?;
                        }
                    }
                }
            }
            root_node.call_method::<()>("update_layout", ())?;
            Ok(())
        })?,
    )?;

    // RootView:confirm_close_views(entries)
    root_view.set("confirm_close_views", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, (this, entries): (LuaTable, LuaTable)| {
            let core: LuaTable = require_table(lua, "core")?;
            let docs = lua.create_table()?;
            let seen = lua.create_table()?;
            for pair in entries.sequence_values::<LuaTable>() {
                let entry = pair?;
                let view: LuaValue = entry.get("view")?;
                if let LuaValue::Table(ref v) = view {
                    let doc_val: LuaValue = v.get("doc")?;
                    if let LuaValue::Table(ref doc) = doc_val {
                        let dirty: bool = doc.call_method("is_dirty", ())?;
                        if dirty {
                            let already: LuaValue = seen.get(doc.clone())?;
                            if matches!(already, LuaValue::Nil) {
                                seen.set(doc.clone(), true)?;
                                let len = docs.raw_len();
                                docs.raw_set(len + 1, doc.clone())?;
                            }
                        }
                    }
                }
            }

            let class: LuaTable = lua.registry_value(&k)?;
            let close_views_fn: LuaFunction = class.get("close_views")?;
            let this2 = this.clone();
            let entries2 = entries.clone();
            let do_close =
                lua.create_function(move |_lua, ()| {
                    close_views_fn.call::<()>((this2.clone(), entries2.clone()))
                })?;

            if docs.raw_len() > 0 {
                let confirm: LuaFunction = core.get("confirm_close_docs")?;
                confirm.call::<()>((docs, do_close))?;
            } else {
                do_close.call::<()>(())?;
            }
            Ok(())
        })?
    })?;

    // RootView:show_tab_context_menu(node, idx, x, y)
    root_view.set(
        "show_tab_context_menu",
        lua.create_function(|lua, (this, node, idx, x, y): (LuaTable, LuaTable, i64, f64, f64)| {
            let views: LuaTable = node.get("views")?;
            let view: LuaValue = views.get(idx)?;
            let view = match view {
                LuaValue::Table(v) => v,
                _ => return Ok(LuaValue::Boolean(false)),
            };

            // right entries
            let right = lua.create_table()?;
            let right_views: LuaTable = node.call_method("get_views_to_right", view.clone())?;
            for entry_val in right_views.sequence_values::<LuaTable>() {
                let entry = entry_val?;
                let item = lua.create_table()?;
                item.set("node", node.clone())?;
                item.set("view", entry)?;
                let len = right.raw_len();
                right.raw_set(len + 1, item)?;
            }

            let all: LuaTable = this.call_method("get_session_views", ())?;
            let others = lua.create_table()?;
            let saved = lua.create_table()?;
            for entry_val in all.sequence_values::<LuaTable>() {
                let entry = entry_val?;
                let entry_view: LuaTable = entry.get("view")?;
                if entry_view != view {
                    let len = others.raw_len();
                    others.raw_set(len + 1, entry.clone())?;
                }
                let doc_val: LuaValue = entry_view.get("doc")?;
                if let LuaValue::Table(ref doc) = doc_val {
                    let dirty: bool = doc.call_method("is_dirty", ())?;
                    if !dirty {
                        let len = saved.raw_len();
                        saved.raw_set(len + 1, entry)?;
                    }
                }
            }

            // Build menu items
            let items = lua.create_table()?;

            // Helper to create a menu item with a command closure
            macro_rules! menu_item {
                ($text:expr, $this:expr, $entries:expr) => {{
                    let item = lua.create_table()?;
                    item.set("text", $text)?;
                    let this_ref = $this.clone();
                    let entries_ref = $entries.clone();
                    item.set(
                        "command",
                        lua.create_function(move |_lua, ()| {
                            this_ref.call_method::<()>(
                                "confirm_close_views",
                                entries_ref.clone(),
                            )
                        })?,
                    )?;
                    item
                }};
            }

            // Close single
            let close_single = lua.create_table()?;
            let single_entry = lua.create_table()?;
            single_entry.set("node", node.clone())?;
            single_entry.set("view", view)?;
            close_single.raw_set(1, single_entry)?;

            items.raw_set(1, menu_item!("Close", this, close_single))?;
            items.raw_set(2, menu_item!("Close Right", this, right))?;
            items.raw_set(3, menu_item!("Close Others", this, others))?;
            items.raw_set(4, menu_item!("Close Saved", this, saved))?;
            items.raw_set(5, menu_item!("Close All", this, all))?;

            let ctx_menu: LuaTable = this.get("context_menu")?;
            ctx_menu.call_method("show", (x, y, items))
        })?,
    )?;

    // RootView:close_all_docviews(keep_active)
    root_view.set(
        "close_all_docviews",
        lua.create_function(|_lua, (this, keep_active): (LuaTable, LuaValue)| {
            let root_node: LuaTable = this.get("root_node")?;
            root_node.call_method::<()>("close_all_docviews", keep_active)
        })?,
    )?;

    // RootView:grab_mouse(button, view)
    root_view.set(
        "grab_mouse",
        lua.create_function(|lua, (this, button, view): (LuaTable, LuaValue, LuaTable)| {
            let grab_val: LuaValue = this.get("grab")?;
            if !matches!(grab_val, LuaValue::Nil) {
                return Err(LuaError::runtime("grab_mouse: grab already held"));
            }
            let grab = lua.create_table()?;
            grab.set("view", view)?;
            grab.set("button", button)?;
            this.set("grab", grab)?;
            Ok(())
        })?,
    )?;

    // RootView:ungrab_mouse(button)
    root_view.set(
        "ungrab_mouse",
        lua.create_function(|_lua, (this, button): (LuaTable, LuaValue)| {
            let grab: LuaTable = this.get::<LuaTable>("grab").map_err(|_| {
                LuaError::runtime("ungrab_mouse: no grab held")
            })?;
            let held: LuaValue = grab.get("button")?;
            if held != button {
                return Err(LuaError::runtime(
                    "ungrab_mouse: button does not match grab",
                ));
            }
            this.set("grab", LuaValue::Nil)?;
            Ok(())
        })?,
    )?;

    // RootView.on_view_mouse_pressed(button, x, y, clicks) -- class-level no-op
    root_view.set(
        "on_view_mouse_pressed",
        lua.create_function(
            |_lua, (_button, _x, _y, _clicks): (LuaValue, LuaValue, LuaValue, LuaValue)| {
                Ok(LuaValue::Nil)
            },
        )?,
    )?;

    // RootView:on_mouse_pressed(button, x, y, clicks)
    root_view.set("on_mouse_pressed", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, (this, button, x, y, clicks): (LuaTable, LuaValue, f64, f64, LuaValue)| {
            // If there is a grab, release it first
            let grab_val: LuaValue = this.get("grab")?;
            if let LuaValue::Table(ref grab) = grab_val {
                let held_button: LuaValue = grab.get("button")?;
                this.call_method::<()>("on_mouse_released", (held_button, x, y))?;
            }

            let ctx_menu: LuaTable = this.get("context_menu")?;
            let ctx_result: LuaValue = ctx_menu.call_method(
                "on_mouse_pressed",
                (button.clone(), x, y, clicks.clone()),
            )?;
            if !matches!(ctx_result, LuaValue::Nil | LuaValue::Boolean(false)) {
                return Ok(LuaValue::Boolean(true));
            }

            let root_node: LuaTable = this.get("root_node")?;
            let div: LuaValue = root_node.call_method("get_divider_overlapping_point", (x, y))?;
            let node: LuaTable = root_node.call_method("get_child_overlapping_point", (x, y))?;

            if let LuaValue::Table(ref div_tbl) = div {
                let active_view: LuaTable = node.get("active_view")?;
                let sb_overlaps: bool =
                    active_view.call_method("scrollbar_overlaps_point", (x, y))?;
                if !sb_overlaps {
                    this.set("dragged_divider", div_tbl.clone())?;
                    return Ok(LuaValue::Boolean(true));
                }
            }

            let hovered_scroll: i64 = node.get::<Option<i64>>("hovered_scroll_button")?.unwrap_or(0);
            if hovered_scroll > 0 {
                node.call_method::<()>("scroll_tabs", hovered_scroll)?;
                return Ok(LuaValue::Boolean(true));
            }

            let idx: LuaValue = node.call_method("get_tab_overlapping_point", (x, y))?;
            if let LuaValue::Integer(tab_idx) = idx {
                let button_str = match &button {
                    LuaValue::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
                    _ => String::new(),
                };
                if button_str == "right" {
                    let views: LuaTable = node.get("views")?;
                    let view: LuaTable = views.get(tab_idx)?;
                    node.call_method::<()>("set_active_view", view)?;
                    return this.call_method("show_tab_context_menu", (node, tab_idx, x, y));
                }
                let hovered_close: i64 = node.get::<Option<i64>>("hovered_close")?.unwrap_or(0);
                if button_str == "middle" || hovered_close == tab_idx {
                    let views: LuaTable = node.get("views")?;
                    let view: LuaTable = views.get(tab_idx)?;
                    node.call_method::<()>("close_view", (root_node, view))?;
                    return Ok(LuaValue::Boolean(true));
                }
                if button_str == "left" {
                    let dn = lua.create_table()?;
                    dn.set("node", node.clone())?;
                    dn.set("idx", tab_idx)?;
                    dn.set("dragging", false)?;
                    dn.set("drag_start_x", x)?;
                    dn.set("drag_start_y", y)?;
                    this.set("dragged_node", dn)?;
                }
                let views: LuaTable = node.get("views")?;
                let view: LuaTable = views.get(tab_idx)?;
                node.call_method::<()>("set_active_view", view)?;
                return Ok(LuaValue::Boolean(true));
            }

            // No tab clicked and not dragging a node
            let dn_val: LuaValue = this.get("dragged_node")?;
            if matches!(dn_val, LuaValue::Nil) {
                let core: LuaTable = require_table(lua, "core")?;
                let active_view: LuaTable = node.get("active_view")?;
                let set_active: LuaFunction = core.get("set_active_view")?;
                set_active.call::<()>(active_view.clone())?;
                this.call_method::<()>("grab_mouse", (button.clone(), active_view.clone()))?;
                let class: LuaTable = lua.registry_value(&k)?;
                let on_view_fn: LuaFunction = class.get("on_view_mouse_pressed")?;
                let view_result: LuaValue =
                    on_view_fn.call((button.clone(), x, y, clicks.clone()))?;
                if !matches!(view_result, LuaValue::Nil | LuaValue::Boolean(false)) {
                    return Ok(view_result);
                }
                return active_view.call_method("on_mouse_pressed", (button, x, y, clicks));
            }

            Ok(LuaValue::Nil)
        })?
    })?;

    // RootView:get_overlay_base_color(overlay)
    root_view.set(
        "get_overlay_base_color",
        lua.create_function(|lua, (this, overlay): (LuaTable, LuaTable)| {
            let style: LuaTable = require_table(lua, "core.style")?;
            let drag_overlay: LuaTable = this.get("drag_overlay")?;
            if overlay == drag_overlay {
                style.get::<LuaValue>("drag_overlay")
            } else {
                style.get::<LuaValue>("drag_overlay_tab")
            }
        })?,
    )?;

    // RootView:set_show_overlay(overlay, status)
    root_view.set(
        "set_show_overlay",
        lua.create_function(|_lua, (this, overlay, status): (LuaTable, LuaTable, bool)| {
            overlay.set("visible", status)?;
            if status {
                let base_color: LuaValue =
                    this.call_method("get_overlay_base_color", overlay.clone())?;
                overlay.set("base_color", base_color.clone())?;
                if let LuaValue::Table(ref bc) = base_color {
                    let color: LuaTable = overlay.get("color")?;
                    color.raw_set(1, bc.get::<LuaValue>(1)?)?;
                    color.raw_set(2, bc.get::<LuaValue>(2)?)?;
                    color.raw_set(3, bc.get::<LuaValue>(3)?)?;
                    color.raw_set(4, bc.get::<LuaValue>(4)?)?;
                }
                overlay.set("opacity", 0.0)?;
            }
            Ok(())
        })?,
    )?;

    // RootView:on_mouse_released(button, x, y, ...)
    root_view.set(
        "on_mouse_released",
        lua.create_function(
            |lua, (this, button, x, y, rest): (LuaTable, LuaValue, f64, f64, LuaMultiValue)| {
                let grab_val: LuaValue = this.get("grab")?;
                if let LuaValue::Table(ref grab) = grab_val {
                    let held: LuaValue = grab.get("button")?;
                    if held == button {
                        let grabbed_view: LuaTable = grab.get("view")?;
                        let mut args = LuaMultiValue::new();
                        args.push_back(LuaValue::clone(&button));
                        args.push_back(LuaValue::Number(x));
                        args.push_back(LuaValue::Number(y));
                        for v in rest.iter() {
                            args.push_back(v.clone());
                        }
                        grabbed_view.call_method::<()>("on_mouse_released", args)?;
                        this.call_method::<()>("ungrab_mouse", button)?;

                        let root_node: LuaTable = this.get("root_node")?;
                        let hovered: LuaTable =
                            root_node.call_method("get_child_overlapping_point", (x, y))?;
                        if grabbed_view != hovered {
                            this.call_method::<()>("on_mouse_moved", (x, y, 0.0, 0.0))?;
                        }
                    }
                    return Ok(LuaValue::Nil);
                }

                let ctx_menu: LuaTable = this.get("context_menu")?;
                let mut ctx_args = LuaMultiValue::new();
                ctx_args.push_back(LuaValue::clone(&button));
                ctx_args.push_back(LuaValue::Number(x));
                ctx_args.push_back(LuaValue::Number(y));
                for v in rest.iter() {
                    ctx_args.push_back(v.clone());
                }
                let ctx_result: LuaValue =
                    ctx_menu.call_method("on_mouse_released", ctx_args)?;
                if !matches!(ctx_result, LuaValue::Nil | LuaValue::Boolean(false)) {
                    return Ok(LuaValue::Boolean(true));
                }

                let div_val: LuaValue = this.get("dragged_divider")?;
                if !matches!(div_val, LuaValue::Nil) {
                    this.set("dragged_divider", LuaValue::Nil)?;
                }

                let dn_val: LuaValue = this.get("dragged_node")?;
                if let LuaValue::Table(ref dn) = dn_val {
                    let button_str = match &button {
                        LuaValue::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
                        _ => String::new(),
                    };
                    if button_str == "left" {
                        let dragging: bool = dn.get::<Option<bool>>("dragging")?.unwrap_or(false);
                        if dragging {
                            let core: LuaTable = require_table(lua, "core")?;
                            let root_node: LuaTable = this.get("root_node")?;
                            let mouse: LuaTable = this.get("mouse")?;
                            let mx: f64 = mouse.get("x")?;
                            let my: f64 = mouse.get("y")?;
                            let node: LuaValue =
                                root_node.call_method("get_child_overlapping_point", (mx, my))?;
                            let dragged_node_ref: LuaTable = dn.get("node")?;

                            if let LuaValue::Table(ref node_tbl) = node {
                                let locked: LuaValue = node_tbl.get("locked")?;
                                if matches!(locked, LuaValue::Nil | LuaValue::Boolean(false)) {
                                    let views: LuaTable = node_tbl.get("views")?;
                                    if *node_tbl != dragged_node_ref || views.raw_len() > 1 {
                                        let split_type_val: String =
                                            node_tbl.call_method("get_split_type", (mx, my))?;
                                        let idx: i64 = dn.get("idx")?;
                                        let dragged_views: LuaTable =
                                            dragged_node_ref.get("views")?;
                                        let view: LuaTable = dragged_views.get(idx)?;

                                        if split_type_val != "middle" && split_type_val != "tab" {
                                            let new_node: LuaTable = node_tbl
                                                .call_method("split", split_type_val)?;
                                            let src_node: LuaTable = root_node.call_method(
                                                "get_node_for_view",
                                                view.clone(),
                                            )?;
                                            src_node.call_method::<()>(
                                                "remove_view",
                                                (root_node.clone(), view.clone()),
                                            )?;
                                            new_node
                                                .call_method::<()>("add_view", view)?;
                                        } else if split_type_val == "middle"
                                            && *node_tbl != dragged_node_ref
                                        {
                                            dragged_node_ref.call_method::<()>(
                                                "remove_view",
                                                (root_node.clone(), view.clone()),
                                            )?;
                                            node_tbl
                                                .call_method::<()>("add_view", view.clone())?;
                                            let set_node: LuaTable = root_node.call_method(
                                                "get_node_for_view",
                                                view.clone(),
                                            )?;
                                            set_node.call_method::<()>(
                                                "set_active_view",
                                                view,
                                            )?;
                                        } else if split_type_val == "tab" {
                                            let tab_index: i64 = node_tbl.call_method(
                                                "get_drag_overlay_tab_position",
                                                (mx, my, dragged_node_ref.clone(), idx),
                                            )?;
                                            dragged_node_ref.call_method::<()>(
                                                "remove_view",
                                                (root_node.clone(), view.clone()),
                                            )?;
                                            node_tbl.call_method::<()>(
                                                "add_view",
                                                (view.clone(), tab_index),
                                            )?;
                                            let set_node: LuaTable = root_node.call_method(
                                                "get_node_for_view",
                                                view.clone(),
                                            )?;
                                            set_node.call_method::<()>(
                                                "set_active_view",
                                                view,
                                            )?;
                                        }
                                        root_node.call_method::<()>("update_layout", ())?;
                                        core.set("redraw", true)?;
                                    }
                                }
                            }
                        }

                        this.call_method::<()>(
                            "set_show_overlay",
                            {
                                let ov: LuaTable = this.get("drag_overlay")?;
                                (ov, false)
                            },
                        )?;
                        this.call_method::<()>(
                            "set_show_overlay",
                            {
                                let ov: LuaTable = this.get("drag_overlay_tab")?;
                                (ov, false)
                            },
                        )?;

                        let dn2: LuaValue = this.get("dragged_node")?;
                        if let LuaValue::Table(ref dn_tbl) = dn2 {
                            let d: bool =
                                dn_tbl.get::<Option<bool>>("dragging")?.unwrap_or(false);
                            if d {
                                let core: LuaTable = require_table(lua, "core")?;
                                let req_cursor: LuaFunction = core.get("request_cursor")?;
                                req_cursor.call::<()>("arrow")?;
                            }
                        }
                        this.set("dragged_node", LuaValue::Nil)?;
                    }
                }
                Ok(LuaValue::Nil)
            },
        )?,
    )?;

    // RootView:on_mouse_moved(x, y, dx, dy)
    root_view.set(
        "on_mouse_moved",
        lua.create_function(|lua, (this, x, y, dx, dy): (LuaTable, f64, f64, f64, f64)| {
            let mouse: LuaTable = this.get("mouse")?;
            mouse.set("x", x)?;
            mouse.set("y", y)?;

            let grab_val: LuaValue = this.get("grab")?;
            if let LuaValue::Table(ref grab) = grab_val {
                let grabbed_view: LuaTable = grab.get("view")?;
                grabbed_view.call_method::<()>("on_mouse_moved", (x, y, dx, dy))?;
                let core: LuaTable = require_table(lua, "core")?;
                let cursor: String = grabbed_view.get("cursor")?;
                let req_cursor: LuaFunction = core.get("request_cursor")?;
                req_cursor.call::<()>(cursor)?;
                return Ok(LuaValue::Nil);
            }

            let ctx_menu: LuaTable = this.get("context_menu")?;
            let ctx_result: LuaValue =
                ctx_menu.call_method("on_mouse_moved", (x, y, dx, dy))?;
            if !matches!(ctx_result, LuaValue::Nil | LuaValue::Boolean(false)) {
                return Ok(LuaValue::Boolean(true));
            }

            let core: LuaTable = require_table(lua, "core")?;
            let nag_view: LuaValue = core.get("nag_view")?;
            let active_view: LuaValue = core.get("active_view")?;
            if active_view == nag_view {
                let req_cursor: LuaFunction = core.get("request_cursor")?;
                req_cursor.call::<()>("arrow")?;
                if let LuaValue::Table(ref av) = active_view {
                    av.call_method::<()>("on_mouse_moved", (x, y, dx, dy))?;
                }
                return Ok(LuaValue::Nil);
            }

            let div_val: LuaValue = this.get("dragged_divider")?;
            if let LuaValue::Table(ref div_node) = div_val {
                let common: LuaTable = require_table(lua, "core.common")?;
                let node_type: String = div_node.get("type")?;
                let root_node: LuaTable = this.get("root_node")?;
                if node_type == "hsplit" {
                    let pos: LuaTable = div_node.get("position")?;
                    let pos_x: f64 = pos.get("x")?;
                    let rn_size: LuaTable = root_node.get("size")?;
                    let rn_sx: f64 = rn_size.get("x")?;
                    let clamped: f64 =
                        common.call_function("clamp", (x - pos_x, 0.0, rn_sx * 0.95))?;
                    resize_child_node(div_node, "x", clamped, dx)?;
                } else if node_type == "vsplit" {
                    let pos: LuaTable = div_node.get("position")?;
                    let pos_y: f64 = pos.get("y")?;
                    let rn_size: LuaTable = root_node.get("size")?;
                    let rn_sy: f64 = rn_size.get("y")?;
                    let clamped: f64 =
                        common.call_function("clamp", (y - pos_y, 0.0, rn_sy * 0.95))?;
                    resize_child_node(div_node, "y", clamped, dy)?;
                }
                let divider: f64 = div_node.get("divider")?;
                let clamped_div: f64 = common.call_function("clamp", (divider, 0.01, 0.99))?;
                div_node.set("divider", clamped_div)?;
                return Ok(LuaValue::Nil);
            }

            let dn_val: LuaValue = this.get("dragged_node")?;
            if let LuaValue::Table(ref dn) = dn_val {
                let dragging: bool = dn.get::<Option<bool>>("dragging")?.unwrap_or(false);
                if !dragging {
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let dsx: f64 = dn.get("drag_start_x")?;
                    let dsy: f64 = dn.get("drag_start_y")?;
                    let dist: f64 = common.call_function("distance", (x, y, dsx, dsy))?;
                    let tab_width: f64 = style.get("tab_width")?;
                    if dist > tab_width * 0.05 {
                        dn.set("dragging", true)?;
                        let req_cursor: LuaFunction = core.get("request_cursor")?;
                        req_cursor.call::<()>("hand")?;
                    }
                }
                return Ok(LuaValue::Nil);
            }

            let last_overlapping: LuaValue = this.get("overlapping_view")?;
            let root_node: LuaTable = this.get("root_node")?;
            let overlapping_node: LuaValue =
                root_node.call_method("get_child_overlapping_point", (x, y))?;

            let new_overlapping: LuaValue = if let LuaValue::Table(ref on) = overlapping_node {
                on.get("active_view")?
            } else {
                LuaValue::Nil
            };
            this.set("overlapping_view", new_overlapping.clone())?;

            if let LuaValue::Table(ref last) = last_overlapping {
                if LuaValue::Table(last.clone()) != new_overlapping {
                    last.call_method::<()>("on_mouse_left", ())?;
                }
            }

            if matches!(new_overlapping, LuaValue::Nil) {
                return Ok(LuaValue::Nil);
            }

            if let LuaValue::Table(ref ov) = new_overlapping {
                ov.call_method::<()>("on_mouse_moved", (x, y, dx, dy))?;
                let cursor: String = ov.get("cursor")?;
                let req_cursor: LuaFunction = core.get("request_cursor")?;
                req_cursor.call::<()>(cursor)?;
            }

            if let LuaValue::Table(ref on) = overlapping_node {
                let scroll_btn: LuaValue = on.call_method("get_scroll_button_index", (x, y))?;
                let in_tab: bool = on.call_method("is_in_tab_area", (x, y))?;
                if !matches!(scroll_btn, LuaValue::Nil | LuaValue::Boolean(false)) || in_tab {
                    let req_cursor: LuaFunction = core.get("request_cursor")?;
                    req_cursor.call::<()>("arrow")?;
                } else {
                    let div: LuaValue =
                        root_node.call_method("get_divider_overlapping_point", (x, y))?;
                    if let LuaValue::Table(ref div_tbl) = div {
                        if let LuaValue::Table(ref ov) = new_overlapping {
                            let sb_overlaps: bool =
                                ov.call_method("scrollbar_overlaps_point", (x, y))?;
                            if !sb_overlaps {
                                let div_type: String = div_tbl.get("type")?;
                                let cursor = if div_type == "hsplit" {
                                    "sizeh"
                                } else {
                                    "sizev"
                                };
                                let req_cursor: LuaFunction = core.get("request_cursor")?;
                                req_cursor.call::<()>(cursor)?;
                            }
                        }
                    }
                }
            }
            Ok(LuaValue::Nil)
        })?,
    )?;

    // RootView:on_mouse_left()
    root_view.set(
        "on_mouse_left",
        lua.create_function(|_lua, this: LuaTable| {
            let ov: LuaValue = this.get("overlapping_view")?;
            if let LuaValue::Table(ref view) = ov {
                view.call_method::<()>("on_mouse_left", ())?;
            }
            Ok(())
        })?,
    )?;

    // RootView:on_file_dropped(filename, x, y)
    root_view.set(
        "on_file_dropped",
        lua.create_function(|lua, (this, filename, x, y): (LuaTable, String, f64, f64)| {
            let root_node: LuaTable = this.get("root_node")?;
            let node: LuaTable = root_node.call_method("get_child_overlapping_point", (x, y))?;
            let active_view: LuaTable = node.get("active_view")?;
            let result: LuaValue =
                active_view.call_method("on_file_dropped", (filename.clone(), x, y))?;
            if !matches!(result, LuaValue::Nil | LuaValue::Boolean(false)) {
                return Ok(result);
            }

            let system: LuaTable = lua.globals().get("system")?;
            let info: LuaValue = system.call_function("get_file_info", filename.clone())?;
            if let LuaValue::Table(ref info_tbl) = info {
                let file_type: String = info_tbl.get("type")?;
                if file_type == "dir" {
                    let abspath: String =
                        system.call_function("absolute_path", filename.clone())?;
                    let core: LuaTable = require_table(lua, "core")?;
                    let common: LuaTable = require_table(lua, "core.common")?;

                    let first_update: bool =
                        this.get::<Option<bool>>("first_update_done")?.unwrap_or(false);
                    if first_update {
                        let nag_view: LuaTable = core.get("nag_view")?;
                        let home_encoded: String =
                            common.call_function("home_encode", abspath.clone())?;
                        let msg = format!(
                            "You are trying to open \"{home_encoded}\"\nDo you want to open this directory here, or in a new window?"
                        );
                        let options = lua.create_table()?;
                        let opt1 = lua.create_table()?;
                        opt1.set("text", "Current window")?;
                        opt1.set("default_yes", true)?;
                        let opt2 = lua.create_table()?;
                        opt2.set("text", "New window")?;
                        opt2.set("default_no", true)?;
                        let opt3 = lua.create_table()?;
                        opt3.set("text", "Cancel")?;
                        options.raw_set(1, opt1)?;
                        options.raw_set(2, opt2)?;
                        options.raw_set(3, opt3)?;

                        let abspath2 = abspath.clone();
                        let filename2 = filename.clone();
                        let callback =
                            lua.create_function(move |lua, opt: LuaTable| {
                                let text: String = opt.get("text")?;
                                if text == "Current window" {
                                    let core: LuaTable = require_table(lua, "core")?;
                                    let add_project: LuaFunction = core.get("add_project")?;
                                    add_project.call::<()>(abspath2.clone())?;
                                } else if text == "New window" {
                                    let system: LuaTable = lua.globals().get("system")?;
                                    let exefile: String = lua.globals().get("EXEFILE")?;
                                    let cmd = format!("{exefile:?} {filename2:?}");
                                    let exec: LuaFunction = system.get("exec")?;
                                    exec.call::<()>(cmd)?;
                                }
                                Ok(())
                            })?;

                        nag_view.call_method::<()>(
                            "show",
                            ("Open directory", msg, options, callback),
                        )?;
                        return Ok(LuaValue::Boolean(true));
                    }

                    let first_dnd: bool =
                        this.get::<Option<bool>>("first_dnd_processed")?.unwrap_or(false);
                    if first_dnd {
                        let exefile: String = lua.globals().get("EXEFILE")?;
                        let cmd = format!("{exefile:?} {filename:?}");
                        let exec: LuaFunction = system.get("exec")?;
                        exec.call::<()>(cmd)?;
                    } else {
                        let abs_fn: String =
                            system.call_function("absolute_path", filename)?;
                        let docs: LuaTable = core.get("docs")?;
                        let confirm: LuaFunction = core.get("confirm_close_docs")?;
                        let open_fn = lua.create_function(move |lua, dirpath: String| {
                            let core: LuaTable = require_table(lua, "core")?;
                            let open_folder: LuaFunction = core.get("open_folder_project")?;
                            open_folder.call::<()>(dirpath)
                        })?;
                        confirm.call::<()>((docs, open_fn, abs_fn))?;
                        this.set("first_dnd_processed", true)?;
                    }
                    return Ok(LuaValue::Boolean(true));
                }
            }

            // File dragged - defer opening
            let defer: LuaTable = this.get("defer_open_docs")?;
            let entry = lua.create_table()?;
            entry.raw_set(1, filename)?;
            entry.raw_set(2, x)?;
            entry.raw_set(3, y)?;
            let table_mod: LuaTable = lua.globals().get("table")?;
            let insert: LuaFunction = table_mod.get("insert")?;
            insert.call::<()>((defer, entry))?;
            Ok(LuaValue::Boolean(true))
        })?,
    )?;

    // RootView:process_defer_open_docs()
    root_view.set(
        "process_defer_open_docs",
        lua.create_function(|lua, this: LuaTable| {
            let core: LuaTable = require_table(lua, "core")?;
            let nag_view: LuaValue = core.get("nag_view")?;
            let active_view: LuaValue = core.get("active_view")?;
            if active_view == nag_view {
                return Ok(());
            }
            let defer: LuaTable = this.get("defer_open_docs")?;
            for pair in defer.sequence_values::<LuaTable>() {
                let drop = pair?;
                let filename: String = drop.get(1)?;
                let x: f64 = drop.get(2)?;
                let y: f64 = drop.get(3)?;
                let try_fn: LuaFunction = core.get("try")?;
                let open_doc: LuaFunction = core.get("open_doc")?;
                let (ok, doc): (bool, LuaValue) =
                    try_fn.call((open_doc, filename))?;
                if ok {
                    if let LuaValue::Table(ref doc_tbl) = doc {
                        let root_view_tbl: LuaTable = core.get("root_view")?;
                        let rn: LuaTable = root_view_tbl.get("root_node")?;
                        let node: LuaTable =
                            rn.call_method("get_child_overlapping_point", (x, y))?;
                        let av: LuaTable = node.get("active_view")?;
                        node.call_method::<()>("set_active_view", av)?;
                        root_view_tbl.call_method::<()>("open_doc", doc_tbl.clone())?;
                    }
                }
            }
            this.set("defer_open_docs", lua.create_table()?)?;
            Ok(())
        })?,
    )?;

    // RootView:on_mouse_wheel(...)
    root_view.set(
        "on_mouse_wheel",
        lua.create_function(|_lua, (this, rest): (LuaTable, LuaMultiValue)| {
            let mouse: LuaTable = this.get("mouse")?;
            let x: f64 = mouse.get("x")?;
            let y: f64 = mouse.get("y")?;
            let root_node: LuaTable = this.get("root_node")?;
            let node: LuaTable = root_node.call_method("get_child_overlapping_point", (x, y))?;
            let active_view: LuaTable = node.get("active_view")?;
            active_view.call_method::<LuaValue>("on_mouse_wheel", rest)
        })?,
    )?;

    // RootView:on_text_input(...)
    root_view.set(
        "on_text_input",
        lua.create_function(|lua, (_this, rest): (LuaTable, LuaMultiValue)| {
            let core: LuaTable = require_table(lua, "core")?;
            let active_view: LuaTable = core.get("active_view")?;
            active_view.call_method::<()>("on_text_input", rest)
        })?,
    )?;

    // RootView:on_touch_pressed(x, y, ...)
    root_view.set(
        "on_touch_pressed",
        lua.create_function(|_lua, (this, x, y): (LuaTable, f64, f64)| {
            let root_node: LuaTable = this.get("root_node")?;
            let node: LuaTable = root_node.call_method("get_child_overlapping_point", (x, y))?;
            let active_view: LuaValue = node.get("active_view")?;
            this.set("touched_view", active_view)?;
            Ok(())
        })?,
    )?;

    // RootView:on_touch_released(x, y, ...)
    root_view.set(
        "on_touch_released",
        lua.create_function(|_lua, (this, _x, _y): (LuaTable, f64, f64)| {
            this.set("touched_view", LuaValue::Nil)?;
            Ok(())
        })?,
    )?;

    // RootView:on_touch_moved(x, y, dx, dy, ...)
    root_view.set(
        "on_touch_moved",
        lua.create_function(
            |lua,
             (this, x, y, dx, dy, rest): (
                LuaTable,
                f64,
                f64,
                f64,
                f64,
                LuaMultiValue,
            )| {
                let touched: LuaValue = this.get("touched_view")?;
                if matches!(touched, LuaValue::Nil) {
                    return Ok(());
                }

                let core: LuaTable = require_table(lua, "core")?;
                let nag_view: LuaValue = core.get("nag_view")?;
                let active_view: LuaValue = core.get("active_view")?;
                if active_view == nag_view {
                    if let LuaValue::Table(ref av) = active_view {
                        let mut args = LuaMultiValue::new();
                        args.push_back(LuaValue::Number(x));
                        args.push_back(LuaValue::Number(y));
                        args.push_back(LuaValue::Number(dx));
                        args.push_back(LuaValue::Number(dy));
                        for v in rest.iter() {
                            args.push_back(v.clone());
                        }
                        av.call_method::<()>("on_touch_moved", args)?;
                    }
                    return Ok(());
                }

                let div_val: LuaValue = this.get("dragged_divider")?;
                if let LuaValue::Table(ref div_node) = div_val {
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let node_type: String = div_node.get("type")?;
                    let root_node: LuaTable = this.get("root_node")?;
                    if node_type == "hsplit" {
                        let pos: LuaTable = div_node.get("position")?;
                        let pos_x: f64 = pos.get("x")?;
                        let rn_size: LuaTable = root_node.get("size")?;
                        let rn_sx: f64 = rn_size.get("x")?;
                        let clamped: f64 =
                            common.call_function("clamp", (x - pos_x, 0.0, rn_sx * 0.95))?;
                        resize_child_node(div_node, "x", clamped, dx)?;
                    } else if node_type == "vsplit" {
                        let pos: LuaTable = div_node.get("position")?;
                        let pos_y: f64 = pos.get("y")?;
                        let rn_size: LuaTable = root_node.get("size")?;
                        let rn_sy: f64 = rn_size.get("y")?;
                        let clamped: f64 =
                            common.call_function("clamp", (y - pos_y, 0.0, rn_sy * 0.95))?;
                        resize_child_node(div_node, "y", clamped, dy)?;
                    }
                    let divider: f64 = div_node.get("divider")?;
                    let clamped_div: f64 =
                        common.call_function("clamp", (divider, 0.01, 0.99))?;
                    div_node.set("divider", clamped_div)?;
                    return Ok(());
                }

                let dn_val: LuaValue = this.get("dragged_node")?;
                if let LuaValue::Table(ref dn) = dn_val {
                    let dragging: bool = dn.get::<Option<bool>>("dragging")?.unwrap_or(false);
                    if !dragging {
                        let common: LuaTable = require_table(lua, "core.common")?;
                        let style: LuaTable = require_table(lua, "core.style")?;
                        let dsx: f64 = dn.get("drag_start_x")?;
                        let dsy: f64 = dn.get("drag_start_y")?;
                        let dist: f64 = common.call_function("distance", (x, y, dsx, dsy))?;
                        let tab_width: f64 = style.get("tab_width")?;
                        if dist > tab_width * 0.05 {
                            dn.set("dragging", true)?;
                            let req_cursor: LuaFunction = core.get("request_cursor")?;
                            req_cursor.call::<()>("hand")?;
                        }
                    }
                    return Ok(());
                }

                if let LuaValue::Table(ref tv) = touched {
                    let mut args = LuaMultiValue::new();
                    args.push_back(LuaValue::Number(x));
                    args.push_back(LuaValue::Number(y));
                    args.push_back(LuaValue::Number(dx));
                    args.push_back(LuaValue::Number(dy));
                    for v in rest.iter() {
                        args.push_back(v.clone());
                    }
                    tv.call_method::<()>("on_touch_moved", args)?;
                }
                Ok(())
            },
        )?,
    )?;

    // RootView:on_ime_text_editing(...)
    root_view.set(
        "on_ime_text_editing",
        lua.create_function(|lua, (_this, rest): (LuaTable, LuaMultiValue)| {
            let core: LuaTable = require_table(lua, "core")?;
            let active_view: LuaTable = core.get("active_view")?;
            active_view.call_method::<()>("on_ime_text_editing", rest)
        })?,
    )?;

    // RootView:on_focus_lost(...)
    root_view.set(
        "on_focus_lost",
        lua.create_function(|lua, _this: LuaTable| {
            let core: LuaTable = require_table(lua, "core")?;
            core.set("redraw", true)?;
            Ok(())
        })?,
    )?;

    // RootView:on_focus_gained(...)
    root_view.set(
        "on_focus_gained",
        lua.create_function(|_lua, _this: LuaTable| Ok(()))?,
    )?;

    // RootView:interpolate_drag_overlay(overlay)
    root_view.set(
        "interpolate_drag_overlay",
        lua.create_function(|_lua, (this, overlay): (LuaTable, LuaTable)| {
            let to: LuaTable = overlay.get("to")?;
            let to_x: f64 = to.get("x")?;
            let to_y: f64 = to.get("y")?;
            let to_w: f64 = to.get("w")?;
            let to_h: f64 = to.get("h")?;
            this.call_method::<()>(
                "move_towards",
                (overlay.clone(), "x", to_x, LuaValue::Nil, "tab_drag"),
            )?;
            this.call_method::<()>(
                "move_towards",
                (overlay.clone(), "y", to_y, LuaValue::Nil, "tab_drag"),
            )?;
            this.call_method::<()>(
                "move_towards",
                (overlay.clone(), "w", to_w, LuaValue::Nil, "tab_drag"),
            )?;
            this.call_method::<()>(
                "move_towards",
                (overlay.clone(), "h", to_h, LuaValue::Nil, "tab_drag"),
            )?;
            let visible: bool = overlay.get::<Option<bool>>("visible")?.unwrap_or(false);
            let target_opacity = if visible { 100.0 } else { 0.0 };
            this.call_method::<()>(
                "move_towards",
                (overlay.clone(), "opacity", target_opacity, LuaValue::Nil, "tab_drag"),
            )?;
            let base_color: LuaTable = overlay.get("base_color")?;
            let color: LuaTable = overlay.get("color")?;
            let base_a: f64 = base_color.get(4)?;
            let opacity: f64 = overlay.get("opacity")?;
            color.raw_set(4, base_a * opacity / 100.0)?;
            Ok(())
        })?,
    )?;

    // RootView:update()
    root_view.set("update", {
        lua.create_function(|_lua, this: LuaTable| {
            let root_node: LuaTable = this.get("root_node")?;
            let node_class: LuaTable = require_table(_lua, "core.node")?;
            let copy_pos_fn: LuaFunction = node_class.get("copy_position_and_size")?;
            copy_pos_fn.call::<()>((root_node.clone(), this.clone()))?;
            root_node.call_method::<()>("update", ())?;
            root_node.call_method::<()>("update_layout", ())?;

            this.call_method::<()>("update_drag_overlay", ())?;
            let drag_overlay: LuaTable = this.get("drag_overlay")?;
            this.call_method::<()>("interpolate_drag_overlay", drag_overlay)?;
            let drag_overlay_tab: LuaTable = this.get("drag_overlay_tab")?;
            this.call_method::<()>("interpolate_drag_overlay", drag_overlay_tab)?;
            this.call_method::<()>("process_defer_open_docs", ())?;
            this.set("first_update_done", true)?;
            let ctx_menu: LuaTable = this.get("context_menu")?;
            ctx_menu.call_method::<()>("update", ())?;
            this.set("first_dnd_processed", true)?;
            Ok(())
        })?
    })?;

    // RootView:set_drag_overlay(overlay, x, y, w, h, immediate)
    root_view.set(
        "set_drag_overlay",
        lua.create_function(
            |_lua,
             (this, overlay, x, y, w, h, immediate): (
                LuaTable,
                LuaTable,
                f64,
                f64,
                f64,
                f64,
                Option<bool>,
            )| {
                let to: LuaTable = overlay.get("to")?;
                to.set("x", x)?;
                to.set("y", y)?;
                to.set("w", w)?;
                to.set("h", h)?;
                if immediate.unwrap_or(false) {
                    overlay.set("x", x)?;
                    overlay.set("y", y)?;
                    overlay.set("w", w)?;
                    overlay.set("h", h)?;
                }
                let visible: bool = overlay.get::<Option<bool>>("visible")?.unwrap_or(false);
                if !visible {
                    this.call_method::<()>("set_show_overlay", (overlay, true))?;
                }
                Ok(())
            },
        )?,
    )?;

    // RootView:update_drag_overlay()
    root_view.set(
        "update_drag_overlay",
        lua.create_function(|lua, this: LuaTable| {
            let dn_val: LuaValue = this.get("dragged_node")?;
            let dn = match dn_val {
                LuaValue::Table(ref t) => {
                    let dragging: bool = t.get::<Option<bool>>("dragging")?.unwrap_or(false);
                    if !dragging {
                        return Ok(());
                    }
                    t
                }
                _ => return Ok(()),
            };

            let root_node: LuaTable = this.get("root_node")?;
            let mouse: LuaTable = this.get("mouse")?;
            let mx: f64 = mouse.get("x")?;
            let my: f64 = mouse.get("y")?;
            let over: LuaValue = root_node.call_method("get_child_overlapping_point", (mx, my))?;

            if let LuaValue::Table(ref over_tbl) = over {
                let locked: LuaValue = over_tbl.get("locked")?;
                if matches!(locked, LuaValue::Nil | LuaValue::Boolean(false)) {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let (_, _, _, tab_h): (f64, f64, f64, f64) =
                        over_tbl.call_method("get_scroll_button_rect", 1)?;
                    let pos: LuaTable = over_tbl.get("position")?;
                    let mut x: f64 = pos.get("x")?;
                    let mut y: f64 = pos.get("y")?;
                    let size: LuaTable = over_tbl.get("size")?;
                    let mut w: f64 = size.get("x")?;
                    let mut h: f64 = size.get("y")?;
                    let split_type_str: String =
                        over_tbl.call_method("get_split_type", (mx, my))?;

                    let dragged_node_ref: LuaTable = dn.get("node")?;
                    let over_views: LuaTable = over_tbl.get("views")?;

                    if split_type_str == "tab"
                        && (*over_tbl != dragged_node_ref || over_views.raw_len() > 1)
                    {
                        let idx: i64 = dn.get("idx")?;
                        let (tab_index, tab_x, tab_y, tab_w, tab_h2): (
                            LuaValue,
                            f64,
                            f64,
                            f64,
                            f64,
                        ) = over_tbl.call_method(
                            "get_drag_overlay_tab_position",
                            (mx, my),
                        )?;
                        let offset_x = if matches!(tab_index, LuaValue::Nil | LuaValue::Boolean(false))
                        {
                            tab_w
                        } else {
                            0.0
                        };
                        let caret_width: f64 = style.get("caret_width")?;

                        let drag_overlay_tab: LuaTable = this.get("drag_overlay_tab")?;
                        let last_over: LuaValue = drag_overlay_tab.get("last_over")?;
                        let immediate =
                            if let LuaValue::Table(ref lo) = last_over {
                                *lo != *over_tbl
                            } else {
                                true
                            };

                        this.call_method::<()>(
                            "set_drag_overlay",
                            (
                                drag_overlay_tab.clone(),
                                tab_x + offset_x,
                                tab_y,
                                caret_width,
                                tab_h2,
                                immediate,
                            ),
                        )?;

                        let drag_overlay: LuaTable = this.get("drag_overlay")?;
                        this.call_method::<()>("set_show_overlay", (drag_overlay, false))?;
                        drag_overlay_tab.set("last_over", over_tbl.clone())?;
                        // suppress unused warning
                        let _ = idx;
                    } else {
                        if *over_tbl != dragged_node_ref || over_views.raw_len() > 1 {
                            y += tab_h;
                            h -= tab_h;
                            let (sx, sy, sw, sh) = split_rect(&split_type_str, x, y, w, h);
                            x = sx;
                            y = sy;
                            w = sw;
                            h = sh;
                        }
                        let drag_overlay: LuaTable = this.get("drag_overlay")?;
                        this.call_method::<()>(
                            "set_drag_overlay",
                            (drag_overlay, x, y, w, h),
                        )?;
                        let drag_overlay_tab: LuaTable = this.get("drag_overlay_tab")?;
                        this.call_method::<()>(
                            "set_show_overlay",
                            (drag_overlay_tab, false),
                        )?;
                    }
                    return Ok(());
                }
            }
            // over is nil/false or locked
            let drag_overlay: LuaTable = this.get("drag_overlay")?;
            this.call_method::<()>("set_show_overlay", (drag_overlay, false))?;
            let drag_overlay_tab: LuaTable = this.get("drag_overlay_tab")?;
            this.call_method::<()>("set_show_overlay", (drag_overlay_tab, false))?;
            Ok(())
        })?,
    )?;

    // RootView:draw_grabbed_tab()
    root_view.set(
        "draw_grabbed_tab",
        lua.create_function(|_lua, this: LuaTable| {
            let dn: LuaTable = this.get("dragged_node")?;
            let dn_node: LuaTable = dn.get("node")?;
            let idx: i64 = dn.get("idx")?;
            let (_, _, w, h): (f64, f64, f64, f64) = dn_node.call_method("get_tab_rect", idx)?;
            let mouse: LuaTable = this.get("mouse")?;
            let mx: f64 = mouse.get("x")?;
            let my: f64 = mouse.get("y")?;
            let x = mx - w / 2.0;
            let y = my - h / 2.0;
            let views: LuaTable = dn_node.get("views")?;
            let view: LuaTable = views.get(idx)?;
            let root_node: LuaTable = this.get("root_node")?;
            root_node.call_method::<()>("draw_tab", (view, true, true, false, x, y, w, h, true))
        })?,
    )?;

    // RootView:draw_drag_overlay(ov)
    root_view.set(
        "draw_drag_overlay",
        lua.create_function(|lua, (_this, ov): (LuaTable, LuaTable)| {
            let opacity: f64 = ov.get("opacity")?;
            if opacity > 0.0 {
                let renderer: LuaTable = lua.globals().get("renderer")?;
                let x: f64 = ov.get("x")?;
                let y: f64 = ov.get("y")?;
                let w: f64 = ov.get("w")?;
                let h: f64 = ov.get("h")?;
                let color: LuaValue = ov.get("color")?;
                let draw_rect: LuaFunction = renderer.get("draw_rect")?;
                draw_rect.call::<()>((x, y, w, h, color))?;
            }
            Ok(())
        })?,
    )?;

    // RootView:draw()
    root_view.set(
        "draw",
        lua.create_function(|lua, this: LuaTable| {
            let root_node: LuaTable = this.get("root_node")?;
            root_node.call_method::<()>("draw", ())?;

            // Process deferred draws
            let deferred: LuaTable = this.get("deferred_draws")?;
            loop {
                let len = deferred.raw_len();
                if len == 0 {
                    break;
                }
                let table_mod: LuaTable = lua.globals().get("table")?;
                let remove: LuaFunction = table_mod.get("remove")?;
                let t: LuaTable = remove.call(deferred.clone())?;
                let func: LuaFunction = t.get("fn")?;
                // Build args from the numeric entries
                let mut args = LuaMultiValue::new();
                let t_len = t.raw_len();
                for i in 1..=t_len {
                    let val: LuaValue = t.raw_get(i)?;
                    args.push_back(val);
                }
                func.call::<()>(args)?;
            }

            let drag_overlay: LuaTable = this.get("drag_overlay")?;
            this.call_method::<()>("draw_drag_overlay", drag_overlay)?;
            let drag_overlay_tab: LuaTable = this.get("drag_overlay_tab")?;
            this.call_method::<()>("draw_drag_overlay", drag_overlay_tab)?;

            let dn_val: LuaValue = this.get("dragged_node")?;
            if let LuaValue::Table(ref dn) = dn_val {
                let dragging: bool = dn.get::<Option<bool>>("dragging")?.unwrap_or(false);
                if dragging {
                    this.call_method::<()>("draw_grabbed_tab", ())?;
                }
            }

            let ctx_menu: LuaTable = this.get("context_menu")?;
            ctx_menu.call_method::<()>("draw", ())?;

            let core: LuaTable = require_table(lua, "core")?;
            let cursor_req: LuaValue = core.get("cursor_change_req")?;
            if !matches!(cursor_req, LuaValue::Nil) {
                let system: LuaTable = lua.globals().get("system")?;
                let set_cursor: LuaFunction = system.get("set_cursor")?;
                set_cursor.call::<()>(cursor_req)?;
                core.set("cursor_change_req", LuaValue::Nil)?;
            }
            Ok(())
        })?,
    )?;

    Ok(root_view)
}
