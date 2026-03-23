use std::sync::Arc;

use mlua::prelude::*;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Remove element at 1-based `pos` from a Lua array table, shifting subsequent elements down.
fn lua_table_remove(t: &LuaTable, pos: i64) -> LuaResult<LuaValue> {
    let len = t.raw_len() as i64;
    let removed: LuaValue = t.raw_get(pos)?;
    for i in pos..len {
        let next: LuaValue = t.raw_get(i + 1)?;
        t.raw_set(i, next)?;
    }
    t.raw_set(len, LuaValue::Nil)?;
    Ok(removed)
}

/// Insert `val` at 1-based `pos` in a Lua array table, shifting subsequent elements up.
fn lua_table_insert(t: &LuaTable, pos: usize, val: LuaValue) -> LuaResult<()> {
    let len = t.raw_len();
    for i in (pos..=len).rev() {
        let v: LuaValue = t.raw_get(i)?;
        t.raw_set(i + 1, v)?;
    }
    t.raw_set(pos, val)?;
    Ok(())
}

/// Registers `core.node` as a pure Rust preload implementing the editor split-tree layout.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "core.node",
        lua.create_function(|lua, ()| {
            let object: LuaTable = require_table(lua, "core.object")?;
            let empty_view_class: LuaTable = require_table(lua, "core.emptyview")?;
            let view_class: LuaTable = require_table(lua, "core.view")?;
            let node_model: LuaTable = require_table(lua, "node_model")?;

            let node = object.call_method::<LuaTable>("extend", ())?;

            node.set(
                "__tostring",
                lua.create_function(|_lua, _self: LuaTable| Ok("Node"))?,
            )?;

            let class_key = Arc::new(lua.create_registry_value(node.clone())?);
            let ev_key = Arc::new(lua.create_registry_value(empty_view_class)?);
            let _view_key = Arc::new(lua.create_registry_value(view_class)?);
            let nm_key = Arc::new(lua.create_registry_value(node_model)?);

            // Node:new(node_type)
            node.set("new", {
                let ck = Arc::clone(&class_key);
                let ek = Arc::clone(&ev_key);
                lua.create_function(move |lua, (this, node_type): (LuaTable, Option<String>)| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let view_cls: LuaTable = lua.registry_value(&ck)?;
                    let move_towards: LuaValue = view_cls.get("move_towards")?;
                    // Walk up to find View's move_towards
                    let actual_mt = {
                        let sup: LuaValue = view_cls.get("super")?;
                        if let LuaValue::Table(ref s) = sup {
                            let mt: LuaValue = s.get("move_towards")?;
                            if matches!(mt, LuaValue::Function(_)) { mt } else { move_towards }
                        } else {
                            move_towards
                        }
                    };
                    // Actually get View.move_towards from the view class
                    let view_cls_actual: LuaTable = require_table(lua, "core.view")?;
                    let view_mt: LuaValue = view_cls_actual.get("move_towards")?;

                    let ntype = node_type.unwrap_or_else(|| "leaf".to_string());
                    this.set("type", ntype.as_str())?;

                    let pos = lua.create_table()?;
                    pos.set("x", 0.0)?;
                    pos.set("y", 0.0)?;
                    this.set("position", pos)?;

                    let size = lua.create_table()?;
                    size.set("x", 0.0)?;
                    size.set("y", 0.0)?;
                    this.set("size", size)?;

                    let views = lua.create_table()?;
                    this.set("views", views)?;
                    this.set("divider", 0.5)?;

                    if ntype == "leaf" {
                        let ev_cls: LuaTable = lua.registry_value(&ek)?;
                        let ev: LuaTable = ev_cls.call(())?;
                        this.call_method::<()>("add_view", ev)?;
                    }

                    this.set("hovered_close", 0)?;
                    this.set("tab_shift", 0.0)?;
                    this.set("tab_offset", 1)?;

                    let tab_width: f64 = style.get("tab_width")?;
                    this.set("tab_width", tab_width)?;

                    // weak-keyed cache for tab titles
                    let setmetatable: LuaFunction = lua.globals().get("setmetatable")?;
                    let cache = lua.create_table()?;
                    let cache_mt = lua.create_table()?;
                    cache_mt.set("__mode", "k")?;
                    setmetatable.call::<LuaTable>((cache.clone(), cache_mt))?;
                    this.set("tab_title_cache", cache)?;

                    this.set("move_towards", view_mt)?;
                    let _ = actual_mt;

                    Ok(())
                })?
            })?;

            // Node:propagate(fn_name, ...)
            node.set(
                "propagate",
                lua.create_function(|_lua, (this, fn_name, args): (LuaTable, String, LuaMultiValue)| {
                    let a: LuaTable = this.get("a")?;
                    let b: LuaTable = this.get("b")?;
                    let a_fn: LuaFunction = a.get(fn_name.as_str())?;
                    let b_fn: LuaFunction = b.get(fn_name.as_str())?;
                    let mut a_args = vec![LuaValue::Table(a.clone())];
                    a_args.extend(args.iter().cloned());
                    a_fn.call::<()>(LuaMultiValue::from_vec(a_args))?;
                    let mut b_args = vec![LuaValue::Table(b.clone())];
                    b_args.extend(args.iter().cloned());
                    b_fn.call::<()>(LuaMultiValue::from_vec(b_args))?;
                    Ok(())
                })?,
            )?;

            // Node:on_mouse_moved (deprecated)
            node.set(
                "on_mouse_moved",
                lua.create_function(|_lua, (this, args): (LuaTable, LuaMultiValue)| {
                    let core: LuaTable = require_table(_lua, "core")?;
                    core.call_function::<()>("deprecation_log", "Node:on_mouse_moved")?;
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let av: LuaTable = this.get("active_view")?;
                        let f: LuaFunction = av.get("on_mouse_moved")?;
                        let mut call_args = vec![LuaValue::Table(av)];
                        call_args.extend(args.iter().cloned());
                        f.call::<()>(LuaMultiValue::from_vec(call_args))?;
                    } else {
                        let propagate: LuaFunction = this.get("propagate")?;
                        let mut call_args = vec![LuaValue::Table(this), LuaValue::String(_lua.create_string("on_mouse_moved")?)];
                        call_args.extend(args.iter().cloned());
                        propagate.call::<()>(LuaMultiValue::from_vec(call_args))?;
                    }
                    Ok(())
                })?,
            )?;

            // Node:on_mouse_released (deprecated)
            node.set(
                "on_mouse_released",
                lua.create_function(|lua, (this, args): (LuaTable, LuaMultiValue)| {
                    let core: LuaTable = require_table(lua, "core")?;
                    core.call_function::<()>("deprecation_log", "Node:on_mouse_released")?;
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let av: LuaTable = this.get("active_view")?;
                        let f: LuaFunction = av.get("on_mouse_released")?;
                        let mut call_args = vec![LuaValue::Table(av)];
                        call_args.extend(args.iter().cloned());
                        f.call::<()>(LuaMultiValue::from_vec(call_args))?;
                    } else {
                        let propagate: LuaFunction = this.get("propagate")?;
                        let mut call_args = vec![LuaValue::Table(this), LuaValue::String(lua.create_string("on_mouse_released")?)];
                        call_args.extend(args.iter().cloned());
                        propagate.call::<()>(LuaMultiValue::from_vec(call_args))?;
                    }
                    Ok(())
                })?,
            )?;

            // Node:on_mouse_left (deprecated)
            node.set(
                "on_mouse_left",
                lua.create_function(|lua, this: LuaTable| {
                    let core: LuaTable = require_table(lua, "core")?;
                    core.call_function::<()>("deprecation_log", "Node:on_mouse_left")?;
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let av: LuaTable = this.get("active_view")?;
                        av.call_method::<()>("on_mouse_left", ())?;
                    } else {
                        this.call_method::<()>("propagate", "on_mouse_left")?;
                    }
                    Ok(())
                })?,
            )?;

            // Node:on_touch_moved (deprecated)
            node.set(
                "on_touch_moved",
                lua.create_function(|lua, (this, args): (LuaTable, LuaMultiValue)| {
                    let core: LuaTable = require_table(lua, "core")?;
                    core.call_function::<()>("deprecation_log", "Node:on_touch_moved")?;
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let av: LuaTable = this.get("active_view")?;
                        let f: LuaFunction = av.get("on_touch_moved")?;
                        let mut call_args = vec![LuaValue::Table(av)];
                        call_args.extend(args.iter().cloned());
                        f.call::<()>(LuaMultiValue::from_vec(call_args))?;
                    } else {
                        let propagate: LuaFunction = this.get("propagate")?;
                        let mut call_args = vec![LuaValue::Table(this), LuaValue::String(lua.create_string("on_touch_moved")?)];
                        call_args.extend(args.iter().cloned());
                        propagate.call::<()>(LuaMultiValue::from_vec(call_args))?;
                    }
                    Ok(())
                })?,
            )?;

            // Node:consume(node)
            node.set(
                "consume",
                lua.create_function(|lua, (this, other): (LuaTable, LuaTable)| {
                    // Clear self
                    let keys_to_remove: Vec<LuaValue> = this
                        .pairs::<LuaValue, LuaValue>()
                        .filter_map(|p| p.ok().map(|(k, _)| k))
                        .collect();
                    for k in &keys_to_remove {
                        this.set(k.clone(), LuaValue::Nil)?;
                    }
                    // Copy from other
                    for pair in other.pairs::<LuaValue, LuaValue>() {
                        let (k, v) = pair?;
                        this.set(k, v)?;
                    }
                    // Restore the metatable from the class
                    let setmetatable: LuaFunction = lua.globals().get("setmetatable")?;
                    let getmetatable: LuaFunction = lua.globals().get("getmetatable")?;
                    let mt: LuaValue = getmetatable.call(other)?;
                    if matches!(mt, LuaValue::Table(_)) {
                        setmetatable.call::<LuaTable>((this, mt))?;
                    }
                    Ok(())
                })?,
            )?;

            // Node:split(dir, view, locked, resizable)
            node.set("split", {
                let ck = Arc::clone(&class_key);
                lua.create_function(move |lua, (this, dir, view, locked, resizable): (LuaTable, String, LuaValue, LuaValue, LuaValue)| {
                    let ntype: String = this.get("type")?;
                    if ntype != "leaf" {
                        return Err(LuaError::runtime("Tried to split non-leaf node"));
                    }
                    let node_type = match dir.as_str() {
                        "up" | "down" => "vsplit",
                        "left" | "right" => "hsplit",
                        _ => return Err(LuaError::runtime("Invalid direction")),
                    };

                    let core: LuaTable = require_table(lua, "core")?;
                    let last_active: LuaValue = core.get("active_view")?;

                    let node_cls: LuaTable = lua.registry_value(&ck)?;
                    let child: LuaTable = node_cls.call(())?;
                    child.call_method::<()>("consume", this.clone())?;

                    let type_node: LuaTable = node_cls.call(node_type)?;
                    this.call_method::<()>("consume", type_node)?;

                    this.set("a", child.clone())?;

                    let b: LuaTable = node_cls.call(())?;
                    this.set("b", b.clone())?;

                    if let LuaValue::Table(_) | LuaValue::UserData(_) = &view {
                        b.call_method::<()>("add_view", view)?;
                    }

                    if let LuaValue::Table(ref locked_tbl) = locked {
                        let _ = locked_tbl; // type assertion
                        b.set("locked", locked.clone())?;
                        let is_resizable = match &resizable {
                            LuaValue::Boolean(v) => *v,
                            _ => false,
                        };
                        b.set("resizable", is_resizable)?;
                        let set_active: LuaFunction = core.get("set_active_view")?;
                        set_active.call::<()>(last_active)?;
                    }

                    if dir == "up" || dir == "left" {
                        let a_val: LuaValue = this.get("a")?;
                        let b_val: LuaValue = this.get("b")?;
                        this.set("a", b_val)?;
                        this.set("b", a_val)?;
                        let a: LuaTable = this.get("a")?;
                        Ok(LuaValue::Table(a))
                    } else {
                        let b: LuaTable = this.get("b")?;
                        Ok(LuaValue::Table(b))
                    }
                })?
            })?;

            // Node:remove_view(root, view)
            node.set("remove_view", {
                let ek = Arc::clone(&ev_key);
                lua.create_function(move |lua, (this, root, view): (LuaTable, LuaTable, LuaTable)| {
                    let views: LuaTable = this.get("views")?;
                    let view_count = views.raw_len();
                    if view_count > 1 {
                        let idx: LuaValue = this.call_method("get_view_idx", view.clone())?;
                        if let Some(idx_num) = idx.as_integer() {
                            let tab_offset: i64 = this.get("tab_offset")?;
                            if idx_num < tab_offset {
                                this.set("tab_offset", tab_offset - 1)?;
                            }
                            lua_table_remove(&views, idx_num)?;
                            let active_view: LuaTable = this.get("active_view")?;
                            if active_view == view {
                                let new_active: LuaValue = views.raw_get(idx_num)?;
                                let fallback = if matches!(new_active, LuaValue::Nil) {
                                    views.raw_get(views.raw_len() as i64)?
                                } else {
                                    new_active
                                };
                                this.call_method::<()>("set_active_view", fallback)?;
                            }
                        }
                    } else {
                        if this == root {
                            let empty_views = lua.create_table()?;
                            this.set("views", empty_views)?;
                            let ev_cls: LuaTable = lua.registry_value(&ek)?;
                            let ev: LuaTable = ev_cls.call(())?;
                            this.call_method::<()>("add_view", ev)?;
                            let core: LuaTable = require_table(lua, "core")?;
                            core.set("last_active_view", LuaValue::Nil)?;
                            return Ok(());
                        }
                        let parent: LuaTable = this.call_method("get_parent_node", root.clone())?;
                        let a: LuaTable = parent.get("a")?;
                        let is_a = a == this;
                        let other: LuaTable = if is_a {
                            parent.get("b")?
                        } else {
                            parent.get("a")?
                        };
                        let (locked_size_x, locked_size_y): (LuaValue, LuaValue) =
                            other.call_method("get_locked_size", ())?;
                        let parent_type: String = parent.get("type")?;
                        let locked_size = if parent_type == "hsplit" {
                            &locked_size_x
                        } else {
                            &locked_size_y
                        };
                        let has_locked_size = matches!(locked_size, LuaValue::Number(_) | LuaValue::Integer(_));

                        let is_primary: bool = this.get::<LuaValue>("is_primary_node")?
                            .as_boolean().unwrap_or(false);

                        let next_primary: LuaValue = if is_primary {
                            let core: LuaTable = require_table(lua, "core")?;
                            let rv: LuaTable = core.get("root_view")?;
                            rv.call_method("select_next_primary_node", ())?
                        } else {
                            LuaValue::Nil
                        };

                        let has_next_primary = !matches!(next_primary, LuaValue::Nil | LuaValue::Boolean(false));

                        if has_locked_size || (is_primary && !has_next_primary) {
                            let empty_views = lua.create_table()?;
                            this.set("views", empty_views)?;
                            let ev_cls: LuaTable = lua.registry_value(&ek)?;
                            let ev: LuaTable = ev_cls.call(())?;
                            this.call_method::<()>("add_view", ev)?;
                        } else {
                            let next_primary_tbl = if let LuaValue::Table(ref t) = next_primary {
                                Some(t.clone())
                            } else {
                                None
                            };

                            let actual_next_primary = if let Some(ref npt) = next_primary_tbl {
                                if *npt == other {
                                    Some(parent.clone())
                                } else {
                                    Some(npt.clone())
                                }
                            } else {
                                None
                            };

                            parent.call_method::<()>("consume", other)?;

                            let mut p: LuaTable = parent.clone();
                            loop {
                                let ptype: String = p.get("type")?;
                                if ptype == "leaf" {
                                    break;
                                }
                                p = if is_a {
                                    p.get("a")?
                                } else {
                                    p.get("b")?
                                };
                            }
                            let p_av: LuaTable = p.get("active_view")?;
                            p.call_method::<()>("set_active_view", p_av)?;

                            if is_primary {
                                if let Some(np) = actual_next_primary {
                                    np.set("is_primary_node", true)?;
                                }
                            }
                        }
                    }
                    let core: LuaTable = require_table(lua, "core")?;
                    core.set("last_active_view", LuaValue::Nil)?;
                    Ok(())
                })?
            })?;

            // Node:close_view(root, view)
            node.set(
                "close_view",
                lua.create_function(|lua, (this, root, view): (LuaTable, LuaTable, LuaTable)| {
                    let do_close = lua.create_function({
                        let this = this.clone();
                        let root = root.clone();
                        let view = view.clone();
                        move |_lua, ()| {
                            this.call_method::<()>("remove_view", (root.clone(), view.clone()))
                        }
                    })?;
                    view.call_method::<()>("try_close", do_close)?;
                    Ok(())
                })?,
            )?;

            // Node:close_active_view(root)
            node.set(
                "close_active_view",
                lua.create_function(|_lua, (this, root): (LuaTable, LuaTable)| {
                    let av: LuaTable = this.get("active_view")?;
                    this.call_method::<()>("close_view", (root, av))
                })?,
            )?;

            // Node:add_view(view, idx)
            node.set("add_view", {
                let ek = Arc::clone(&ev_key);
                lua.create_function(move |lua, (this, view, idx): (LuaTable, LuaValue, Option<i64>)| {
                    let ntype: String = this.get("type")?;
                    if ntype != "leaf" {
                        return Err(LuaError::runtime("Tried to add view to non-leaf node"));
                    }
                    let locked: LuaValue = this.get("locked")?;
                    if matches!(locked, LuaValue::Table(_)) {
                        return Err(LuaError::runtime("Tried to add view to locked node"));
                    }
                    let views: LuaTable = this.get("views")?;
                    let ev_cls: LuaTable = lua.registry_value(&ek)?;
                    let first_view: LuaValue = views.raw_get(1)?;
                    let mut adjusted_idx = idx;
                    if let LuaValue::Table(ref fv) = first_view {
                        let is_empty: bool = fv.call_method("is", ev_cls)?;
                        if is_empty {
                            lua_table_remove(&views, 1)?;
                            if let Some(i) = adjusted_idx {
                                if i > 1 {
                                    adjusted_idx = Some(i - 1);
                                }
                            }
                        }
                    }

                    let len = views.raw_len() as i64;
                    let insert_idx = adjusted_idx
                        .unwrap_or(len + 1)
                        .max(1)
                        .min(len + 1) as usize;
                    lua_table_insert(&views, insert_idx, view.clone())?;
                    this.call_method::<()>("set_active_view", view)?;
                    Ok(())
                })?
            })?;

            // Node:set_active_view(view)
            node.set(
                "set_active_view",
                lua.create_function(|lua, (this, view): (LuaTable, LuaValue)| {
                    let ntype: String = this.get("type")?;
                    if ntype != "leaf" {
                        return Err(LuaError::runtime("Tried to set active view on non-leaf node"));
                    }
                    let last_active: LuaValue = this.get("active_view")?;
                    this.set("active_view", view.clone())?;
                    let core: LuaTable = require_table(lua, "core")?;
                    let set_active: LuaFunction = core.get("set_active_view")?;
                    set_active.call::<()>(view.clone())?;
                    if let LuaValue::Table(ref lav) = last_active {
                        if let LuaValue::Table(ref v) = view {
                            if *lav != *v {
                                lav.call_method::<()>("on_mouse_left", ())?;
                            }
                        }
                    }
                    Ok(())
                })?,
            )?;

            // Node:get_view_idx(view)
            node.set(
                "get_view_idx",
                lua.create_function(|_lua, (this, view): (LuaTable, LuaTable)| {
                    let views: LuaTable = this.get("views")?;
                    for i in 1..=views.raw_len() as i64 {
                        let v: LuaValue = views.raw_get(i)?;
                        if let LuaValue::Table(ref vt) = v {
                            if *vt == view {
                                return Ok(LuaValue::Integer(i));
                            }
                        }
                    }
                    Ok(LuaValue::Nil)
                })?,
            )?;

            // Node:get_views_to_right(view)
            node.set(
                "get_views_to_right",
                lua.create_function(|lua, (this, view): (LuaTable, LuaTable)| {
                    let idx: LuaValue = this.call_method("get_view_idx", view)?;
                    let result = lua.create_table()?;
                    if let Some(idx_num) = idx.as_integer() {
                        let views: LuaTable = this.get("views")?;
                        let mut j = 1i64;
                        for i in (idx_num + 1)..=views.raw_len() as i64 {
                            let v: LuaValue = views.raw_get(i)?;
                            result.raw_set(j, v)?;
                            j += 1;
                        }
                    }
                    Ok(result)
                })?,
            )?;

            // Node:get_node_for_view(view)
            node.set(
                "get_node_for_view",
                lua.create_function(|_lua, (this, view): (LuaTable, LuaTable)| {
                    let views: LuaTable = this.get("views")?;
                    for i in 1..=views.raw_len() as i64 {
                        let v: LuaValue = views.raw_get(i)?;
                        if let LuaValue::Table(ref vt) = v {
                            if *vt == view {
                                return Ok(LuaValue::Table(this));
                            }
                        }
                    }
                    let ntype: String = this.get("type")?;
                    if ntype != "leaf" {
                        let a: LuaTable = this.get("a")?;
                        let result: LuaValue = a.call_method("get_node_for_view", view.clone())?;
                        if !matches!(result, LuaValue::Nil) {
                            return Ok(result);
                        }
                        let b: LuaTable = this.get("b")?;
                        return b.call_method("get_node_for_view", view);
                    }
                    Ok(LuaValue::Nil)
                })?,
            )?;

            // Node:get_parent_node(root)
            node.set(
                "get_parent_node",
                lua.create_function(|_lua, (this, root): (LuaTable, LuaTable)| {
                    let a: LuaValue = root.get("a")?;
                    let b: LuaValue = root.get("b")?;
                    if let LuaValue::Table(ref at) = a {
                        if *at == this {
                            return Ok(LuaValue::Table(root));
                        }
                    }
                    if let LuaValue::Table(ref bt) = b {
                        if *bt == this {
                            return Ok(LuaValue::Table(root));
                        }
                    }
                    let ntype: String = root.get("type")?;
                    if ntype != "leaf" {
                        if let LuaValue::Table(at) = a {
                            let result: LuaValue = this.call_method("get_parent_node", at)?;
                            if !matches!(result, LuaValue::Nil) {
                                return Ok(result);
                            }
                        }
                        if let LuaValue::Table(bt) = b {
                            return this.call_method("get_parent_node", bt);
                        }
                    }
                    Ok(LuaValue::Nil)
                })?,
            )?;

            // Node:get_children(t)
            node.set(
                "get_children",
                lua.create_function(|lua, (this, t): (LuaTable, Option<LuaTable>)| {
                    let result = t.unwrap_or(lua.create_table()?);
                    let views: LuaTable = this.get("views")?;
                    for i in 1..=views.raw_len() as i64 {
                        let v: LuaValue = views.raw_get(i)?;
                        let len = result.raw_len() as i64;
                        result.raw_set(len + 1, v)?;
                    }
                    let a: LuaValue = this.get("a")?;
                    if let LuaValue::Table(at) = a {
                        at.call_method::<()>("get_children", result.clone())?;
                    }
                    let b: LuaValue = this.get("b")?;
                    if let LuaValue::Table(bt) = b {
                        bt.call_method::<()>("get_children", result.clone())?;
                    }
                    Ok(result)
                })?,
            )?;

            // Node:get_divider_overlapping_point(px, py)
            node.set(
                "get_divider_overlapping_point",
                lua.create_function(|_lua, (this, px, py): (LuaTable, f64, f64)| {
                    let ntype: String = this.get("type")?;
                    if ntype != "leaf" {
                        let axis = if ntype == "hsplit" { "x" } else { "y" };
                        let a: LuaTable = this.get("a")?;
                        let b: LuaTable = this.get("b")?;
                        let a_resizable: bool = a.call_method("is_resizable", axis)?;
                        let b_resizable: bool = b.call_method("is_resizable", axis)?;
                        if a_resizable && b_resizable {
                            let p = 6.0;
                            let (x, y, w, h): (f64, f64, f64, f64) =
                                this.call_method("get_divider_rect", ())?;
                            let x = x - p;
                            let y = y - p;
                            let w = w + p * 2.0;
                            let h = h + p * 2.0;
                            if px > x && py > y && px < x + w && py < y + h {
                                return Ok(LuaValue::Table(this));
                            }
                        }
                        let result: LuaValue = a.call_method("get_divider_overlapping_point", (px, py))?;
                        if !matches!(result, LuaValue::Nil) {
                            return Ok(result);
                        }
                        return b.call_method("get_divider_overlapping_point", (px, py));
                    }
                    Ok(LuaValue::Nil)
                })?,
            )?;

            // Node:get_visible_tabs_number()
            node.set("get_visible_tabs_number", {
                let nmk = Arc::clone(&nm_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let nm: LuaTable = lua.registry_value(&nmk)?;
                    let views: LuaTable = this.get("views")?;
                    let tab_offset: i64 = this.get("tab_offset")?;
                    let config: LuaTable = require_table(lua, "core.config")?;
                    let max_tabs: i64 = config.get("max_tabs")?;
                    let result: i64 = nm.call_function("visible_tabs", (views.raw_len() as i64, tab_offset, max_tabs))?;
                    Ok(result)
                })?
            })?;

            // Node:get_tab_overlapping_point(px, py)
            node.set("get_tab_overlapping_point", {
                let nmk = Arc::clone(&nm_key);
                lua.create_function(move |lua, (this, px, py): (LuaTable, f64, f64)| {
                    let should_show: bool = this.call_method("should_show_tabs", ())?;
                    if !should_show {
                        return Ok(LuaValue::Nil);
                    }
                    let tab_offset: i64 = this.get("tab_offset")?;
                    let (_, y1, _, h): (f64, f64, f64, f64) =
                        this.call_method("get_tab_rect", tab_offset)?;
                    if py < y1 || py >= y1 + h {
                        return Ok(LuaValue::Nil);
                    }
                    let nm: LuaTable = lua.registry_value(&nmk)?;
                    let views: LuaTable = this.get("views")?;
                    let config: LuaTable = require_table(lua, "core.config")?;
                    let max_tabs: i64 = config.get("max_tabs")?;
                    let tab_width: f64 = this.get("tab_width")?;
                    let tab_shift: f64 = this.get("tab_shift")?;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    let position: LuaTable = this.get("position")?;
                    let pos_x: f64 = position.get("x")?;
                    let idx: i64 = nm.call_function("tab_hit_index", (
                        views.raw_len() as i64, tab_offset, max_tabs,
                        tab_width, tab_shift, size_x,
                        px - pos_x,
                    ))?;
                    if idx > 0 {
                        Ok(LuaValue::Integer(idx))
                    } else {
                        Ok(LuaValue::Nil)
                    }
                })?
            })?;

            // Node:should_show_tabs()
            node.set("should_show_tabs", {
                let ek = Arc::clone(&ev_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let locked: LuaValue = this.get("locked")?;
                    if matches!(locked, LuaValue::Table(_)) {
                        return Ok(false);
                    }
                    let core: LuaTable = require_table(lua, "core")?;
                    let rv: LuaValue = core.get("root_view")?;
                    if let LuaValue::Table(ref rv_tbl) = rv {
                        let is_focus: LuaValue = rv_tbl.call_method("is_focus_mode_active", ())?;
                        if matches!(is_focus, LuaValue::Boolean(true)) {
                            return Ok(false);
                        }
                    }
                    let dn: LuaValue = if let LuaValue::Table(ref rv_tbl) = rv {
                        rv_tbl.get("dragged_node")?
                    } else {
                        LuaValue::Nil
                    };
                    let views: LuaTable = this.get("views")?;
                    let view_count = views.raw_len();
                    if view_count > 1 {
                        return Ok(true);
                    }
                    if let LuaValue::Table(ref dn_tbl) = dn {
                        let dragging: LuaValue = dn_tbl.get("dragging")?;
                        if matches!(dragging, LuaValue::Boolean(true)) {
                            return Ok(true);
                        }
                    }
                    let config: LuaTable = require_table(lua, "core.config")?;
                    let always_show: bool = config.get::<LuaValue>("always_show_tabs")?
                        .as_boolean().unwrap_or(false);
                    if always_show {
                        let first: LuaValue = views.raw_get(1)?;
                        if let LuaValue::Table(ref fv) = first {
                            let ev_cls: LuaTable = lua.registry_value(&ek)?;
                            let is_empty: bool = fv.call_method("is", ev_cls)?;
                            return Ok(!is_empty);
                        }
                    }
                    Ok(false)
                })?
            })?;

            // Node:get_scroll_button_index(px, py)
            node.set(
                "get_scroll_button_index",
                lua.create_function(|_lua, (this, px, py): (LuaTable, f64, f64)| {
                    let views: LuaTable = this.get("views")?;
                    if views.raw_len() == 1 {
                        return Ok(LuaValue::Nil);
                    }
                    for i in 1..=2i64 {
                        let (x, y, w, h, _pad): (f64, f64, f64, f64, f64) =
                            this.call_method("get_scroll_button_rect", i)?;
                        if px >= x && px < x + w && py >= y && py < y + h {
                            return Ok(LuaValue::Integer(i));
                        }
                    }
                    Ok(LuaValue::Nil)
                })?,
            )?;

            // Node:tab_hovered_update(px, py)
            node.set(
                "tab_hovered_update",
                lua.create_function(|lua, (this, px, py): (LuaTable, f64, f64)| {
                    let should_show: bool = this.call_method("should_show_tabs", ())?;
                    if !should_show {
                        this.set("hovered_tab", LuaValue::Nil)?;
                        this.set("hovered_close", 0)?;
                        this.set("hovered_scroll_button", 0)?;
                        return Ok(());
                    }
                    let tab_offset: i64 = this.get("tab_offset")?;
                    let (_, _, _, h): (f64, f64, f64, f64) =
                        this.call_method("get_tab_rect", tab_offset)?;
                    let position: LuaTable = this.get("position")?;
                    let pos_x: f64 = position.get("x")?;
                    let pos_y: f64 = position.get("y")?;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    if py < pos_y || py >= pos_y + h || px < pos_x || px >= pos_x + size_x {
                        this.set("hovered_tab", LuaValue::Nil)?;
                        this.set("hovered_close", 0)?;
                        this.set("hovered_scroll_button", 0)?;
                        return Ok(());
                    }
                    let tab_index: LuaValue = this.call_method("get_tab_overlapping_point", (px, py))?;
                    this.set("hovered_tab", tab_index.clone())?;
                    this.set("hovered_close", 0)?;
                    this.set("hovered_scroll_button", 0)?;
                    if let Some(tab_idx) = tab_index.as_integer() {
                        let (x, y, w, h): (f64, f64, f64, f64) =
                            this.call_method("get_tab_rect", tab_idx)?;
                        let style: LuaTable = require_table(lua, "core.style")?;
                        let icon_font: LuaValue = style.get("icon_font")?;
                        let style_font: LuaValue = style.get("font")?;
                        let style_padding: LuaTable = style.get("padding")?;
                        let pad_x: f64 = style_padding.get("x")?;
                        let icon_w: f64 = match &icon_font {
                            LuaValue::Table(t) => t.call_method("get_width", "C")?,
                            LuaValue::UserData(ud) => ud.call_method("get_width", "C")?,
                            _ => 14.0,
                        };
                        let font_h: f64 = match &style_font {
                            LuaValue::Table(t) => t.call_method("get_height", ())?,
                            LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                            _ => 14.0,
                        };
                        let hit_w = (icon_w + pad_x).max(font_h);
                        let cx = x + w - hit_w;
                        let config: LuaTable = require_table(lua, "core.config")?;
                        let tab_close_button: bool = config.get::<LuaValue>("tab_close_button")?
                            .as_boolean().unwrap_or(true);
                        if px >= cx && px < cx + hit_w && py >= y && py < y + h && tab_close_button {
                            this.set("hovered_close", tab_idx)?;
                        }
                    } else {
                        let views: LuaTable = this.get("views")?;
                        let visible: i64 = this.call_method("get_visible_tabs_number", ())?;
                        if (views.raw_len() as i64) > visible {
                            let sb_idx: LuaValue = this.call_method("get_scroll_button_index", (px, py))?;
                            let sb_val = sb_idx.as_integer().unwrap_or(0);
                            this.set("hovered_scroll_button", sb_val)?;
                        }
                    }
                    Ok(())
                })?,
            )?;

            // Node:get_child_overlapping_point(x, y)
            node.set(
                "get_child_overlapping_point",
                lua.create_function(|_lua, (this, x, y): (LuaTable, f64, f64)| {
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        return Ok(this);
                    }
                    let child: LuaTable = if ntype == "hsplit" {
                        let b: LuaTable = this.get("b")?;
                        let b_pos: LuaTable = b.get("position")?;
                        let bx: f64 = b_pos.get("x")?;
                        if x < bx { this.get("a")? } else { b }
                    } else {
                        let b: LuaTable = this.get("b")?;
                        let b_pos: LuaTable = b.get("position")?;
                        let by: f64 = b_pos.get("y")?;
                        if y < by { this.get("a")? } else { b }
                    };
                    child.call_method("get_child_overlapping_point", (x, y))
                })?,
            )?;

            // Node:get_scroll_button_rect(index)
            node.set(
                "get_scroll_button_rect",
                lua.create_function(|lua, (this, index): (LuaTable, i64)| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let style_font: LuaValue = style.get("font")?;
                    let style_padding: LuaTable = style.get("padding")?;
                    let pad_y: f64 = style_padding.get("y")?;
                    let style_margin: LuaTable = style.get("margin")?;
                    let tab_margin: LuaTable = style_margin.get("tab")?;
                    let margin_top: f64 = tab_margin.get("top")?;
                    let font_h: f64 = match &style_font {
                        LuaValue::Table(t) => t.call_method("get_height", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                        _ => 14.0,
                    };
                    let h = font_h + pad_y * 2.0 + margin_top;

                    let w_char: f64 = match &style_font {
                        LuaValue::Table(t) => t.call_method("get_width", ">")?,
                        LuaValue::UserData(ud) => ud.call_method("get_width", ">")?,
                        _ => 8.0,
                    };
                    let pad = w_char;
                    let w = w_char + 2.0 * pad;
                    let position: LuaTable = this.get("position")?;
                    let pos_x: f64 = position.get("x")?;
                    let pos_y: f64 = position.get("y")?;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    let x = if index == 1 {
                        pos_x + size_x - w * 2.0
                    } else {
                        pos_x + size_x - w
                    };
                    Ok((x, pos_y, w, h, pad))
                })?,
            )?;

            // Node:get_tab_rect(idx)
            node.set(
                "get_tab_rect",
                lua.create_function(|lua, (this, idx): (LuaTable, i64)| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let style_font: LuaValue = style.get("font")?;
                    let style_padding: LuaTable = style.get("padding")?;
                    let pad_y: f64 = style_padding.get("y")?;
                    let style_margin: LuaTable = style.get("margin")?;
                    let tab_margin: LuaTable = style_margin.get("tab")?;
                    let margin_y: f64 = tab_margin.get("top")?;
                    let font_h: f64 = match &style_font {
                        LuaValue::Table(t) => t.call_method("get_height", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                        _ => 14.0,
                    };
                    let h = font_h + pad_y * 2.0 + margin_y;

                    let size: LuaTable = this.get("size")?;
                    let maxw: f64 = size.get("x")?;
                    let position: LuaTable = this.get("position")?;
                    let x0: f64 = position.get("x")?;
                    let tab_width: f64 = this.get("tab_width")?;
                    let tab_shift: f64 = this.get("tab_shift")?;

                    let x1 = x0 + (tab_width * (idx - 1) as f64 - tab_shift).clamp(0.0, maxw);
                    let x2 = x0 + (tab_width * idx as f64 - tab_shift).clamp(0.0, maxw);
                    let pos_y: f64 = position.get("y")?;
                    Ok((x1, pos_y, x2 - x1, h, margin_y))
                })?,
            )?;

            // Node:get_divider_rect()
            node.set(
                "get_divider_rect",
                lua.create_function(|lua, this: LuaTable| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let ds: f64 = style.get("divider_size")?;
                    let position: LuaTable = this.get("position")?;
                    let x: f64 = position.get("x")?;
                    let y: f64 = position.get("y")?;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    let size_y: f64 = size.get("y")?;
                    let ntype: String = this.get("type")?;
                    let a: LuaTable = this.get("a")?;
                    let a_size: LuaTable = a.get("size")?;
                    if ntype == "hsplit" {
                        let a_sx: f64 = a_size.get("x")?;
                        Ok((x + a_sx, y, ds, size_y))
                    } else {
                        let a_sy: f64 = a_size.get("y")?;
                        Ok((x, y + a_sy, size_x, ds))
                    }
                })?,
            )?;

            // Node:get_locked_size()
            node.set(
                "get_locked_size",
                lua.create_function(|lua, this: LuaTable| {
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let locked: LuaValue = this.get("locked")?;
                        if let LuaValue::Table(ref locked_tbl) = locked {
                            let av: LuaTable = this.get("active_view")?;
                            let av_size: LuaTable = av.get("size")?;
                            let locked_x: LuaValue = locked_tbl.get("x")?;
                            let locked_y: LuaValue = locked_tbl.get("y")?;
                            let sx: LuaValue = if matches!(locked_x, LuaValue::Boolean(true)) {
                                let v: f64 = av_size.get("x")?;
                                LuaValue::Number(v)
                            } else {
                                LuaValue::Nil
                            };
                            let sy: LuaValue = if matches!(locked_y, LuaValue::Boolean(true)) {
                                let v: f64 = av_size.get("y")?;
                                LuaValue::Number(v)
                            } else {
                                LuaValue::Nil
                            };
                            return Ok((sx, sy));
                        }
                        return Ok((LuaValue::Nil, LuaValue::Nil));
                    }
                    let a: LuaTable = this.get("a")?;
                    let b: LuaTable = this.get("b")?;
                    let (x1, y1): (LuaValue, LuaValue) = a.call_method("get_locked_size", ())?;
                    let (x2, y2): (LuaValue, LuaValue) = b.call_method("get_locked_size", ())?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let divider_size: f64 = style.get("divider_size")?;
                    if ntype == "hsplit" {
                        let sx = match (&x1, &x2) {
                            (LuaValue::Number(v1), LuaValue::Number(v2)) => {
                                let dsx = if *v1 < 1.0 || *v2 < 1.0 { 0.0 } else { divider_size };
                                LuaValue::Number(v1 + v2 + dsx)
                            }
                            _ => LuaValue::Nil,
                        };
                        let sy = if !matches!(y1, LuaValue::Nil) { y1 } else { y2 };
                        Ok((sx, sy))
                    } else {
                        let sy = match (&y1, &y2) {
                            (LuaValue::Number(v1), LuaValue::Number(v2)) => {
                                let dsy = if *v1 < 1.0 || *v2 < 1.0 { 0.0 } else { divider_size };
                                LuaValue::Number(v1 + v2 + dsy)
                            }
                            _ => LuaValue::Nil,
                        };
                        let sx = if !matches!(x1, LuaValue::Nil) { x1 } else { x2 };
                        Ok((sx, sy))
                    }
                })?,
            )?;

            // Node.copy_position_and_size(dst, src) — static method
            node.set(
                "copy_position_and_size",
                lua.create_function(|_lua, (dst, src): (LuaTable, LuaTable)| {
                    let src_pos: LuaTable = src.get("position")?;
                    let src_size: LuaTable = src.get("size")?;
                    let dst_pos: LuaTable = dst.get("position")?;
                    let dst_size: LuaTable = dst.get("size")?;
                    let sx: f64 = src_pos.get("x")?;
                    let sy: f64 = src_pos.get("y")?;
                    dst_pos.set("x", sx)?;
                    dst_pos.set("y", sy)?;
                    let sw: f64 = src_size.get("x")?;
                    let sh: f64 = src_size.get("y")?;
                    dst_size.set("x", sw)?;
                    dst_size.set("y", sh)?;
                    Ok(())
                })?,
            )?;

            // Node:update_layout()
            node.set("update_layout", {
                let ck = Arc::clone(&class_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let av: LuaTable = this.get("active_view")?;
                        let should_show: bool = this.call_method("should_show_tabs", ())?;
                        if should_show {
                            let (_, _, _, th): (f64, f64, f64, f64) =
                                this.call_method("get_tab_rect", 1)?;
                            let av_pos: LuaTable = av.get("position")?;
                            let av_size: LuaTable = av.get("size")?;
                            let pos: LuaTable = this.get("position")?;
                            let size: LuaTable = this.get("size")?;
                            let px: f64 = pos.get("x")?;
                            let py: f64 = pos.get("y")?;
                            let sx: f64 = size.get("x")?;
                            let sy: f64 = size.get("y")?;
                            av_pos.set("x", px)?;
                            av_pos.set("y", py + th)?;
                            av_size.set("x", sx)?;
                            av_size.set("y", sy - th)?;
                        } else {
                            let node_cls: LuaTable = lua.registry_value(&ck)?;
                            let copy_fn: LuaFunction = node_cls.get("copy_position_and_size")?;
                            copy_fn.call::<()>((av, this))?;
                        }
                    } else {
                        let a: LuaTable = this.get("a")?;
                        let b: LuaTable = this.get("b")?;
                        let (x1, y1): (LuaValue, LuaValue) = a.call_method("get_locked_size", ())?;
                        let (x2, y2): (LuaValue, LuaValue) = b.call_method("get_locked_size", ())?;

                        let style: LuaTable = require_table(lua, "core.style")?;
                        let divider_size: f64 = style.get("divider_size")?;

                        if ntype == "hsplit" {
                            calc_split_sizes(lua, &this, "x", "y", &x1, &x2, divider_size)?;
                        } else {
                            calc_split_sizes(lua, &this, "y", "x", &y1, &y2, divider_size)?;
                        }
                        a.call_method::<()>("update_layout", ())?;
                        b.call_method::<()>("update_layout", ())?;
                    }
                    Ok(())
                })?
            })?;

            // Node:scroll_tabs_to_visible()
            node.set("scroll_tabs_to_visible", {
                let nmk = Arc::clone(&nm_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let av: LuaTable = this.get("active_view")?;
                    let index: LuaValue = this.call_method("get_view_idx", av)?;
                    if let Some(idx) = index.as_integer() {
                        let nm: LuaTable = lua.registry_value(&nmk)?;
                        let views: LuaTable = this.get("views")?;
                        let tab_offset: i64 = this.get("tab_offset")?;
                        let config: LuaTable = require_table(lua, "core.config")?;
                        let max_tabs: i64 = config.get("max_tabs")?;
                        let new_offset: i64 = nm.call_function("ensure_visible_tab_offset", (
                            views.raw_len() as i64, tab_offset, max_tabs, idx,
                        ))?;
                        this.set("tab_offset", new_offset)?;
                    }
                    Ok(())
                })?
            })?;

            // Node:scroll_tabs(dir)
            node.set("scroll_tabs", {
                let nmk = Arc::clone(&nm_key);
                lua.create_function(move |lua, (this, dir): (LuaTable, i64)| {
                    let av: LuaTable = this.get("active_view")?;
                    let view_index_val: LuaValue = this.call_method("get_view_idx", av)?;
                    let view_index = view_index_val.as_integer().unwrap_or(1);
                    let nm: LuaTable = lua.registry_value(&nmk)?;
                    let views: LuaTable = this.get("views")?;
                    let tab_offset: i64 = this.get("tab_offset")?;
                    let config: LuaTable = require_table(lua, "core.config")?;
                    let max_tabs: i64 = config.get("max_tabs")?;
                    let scroll_dir = if dir == 1 { -1i64 } else { 1i64 };
                    let (new_offset, new_active): (i64, i64) = nm.call_function("scroll_tab_offset", (
                        views.raw_len() as i64, tab_offset, max_tabs, view_index, scroll_dir,
                    ))?;
                    this.set("tab_offset", new_offset)?;
                    if new_active != view_index && new_active >= 1 && new_active <= views.raw_len() as i64 {
                        let v: LuaValue = views.raw_get(new_active)?;
                        this.call_method::<()>("set_active_view", v)?;
                    }
                    Ok(())
                })?
            })?;

            // Node:target_tab_width()
            node.set("target_tab_width", {
                let nmk = Arc::clone(&nm_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let visible: i64 = this.call_method("get_visible_tabs_number", ())?;
                    let views: LuaTable = this.get("views")?;
                    let size: LuaTable = this.get("size")?;
                    let mut w: f64 = size.get("x")?;
                    if (views.raw_len() as i64) > visible {
                        let style: LuaTable = require_table(lua, "core.style")?;
                        let style_font: LuaValue = style.get("font")?;
                        let w_char: f64 = match &style_font {
                            LuaValue::Table(t) => t.call_method("get_width", ">")?,
                            LuaValue::UserData(ud) => ud.call_method("get_width", ">")?,
                            _ => 8.0,
                        };
                        let scroll_w = w_char + 2.0 * w_char;
                        w -= scroll_w * 2.0;
                    }
                    let nm: LuaTable = lua.registry_value(&nmk)?;
                    let tab_offset: i64 = this.get("tab_offset")?;
                    let config: LuaTable = require_table(lua, "core.config")?;
                    let max_tabs: i64 = config.get("max_tabs")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let style_tab_width: f64 = style.get("tab_width")?;
                    let result: f64 = nm.call_function("target_tab_width", (
                        w, views.raw_len() as i64, tab_offset, max_tabs, style_tab_width,
                    ))?;
                    Ok(result)
                })?
            })?;

            // Node:update()
            node.set(
                "update",
                lua.create_function(|lua, this: LuaTable| {
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        this.call_method::<()>("scroll_tabs_to_visible", ())?;
                        let views: LuaTable = this.get("views")?;
                        for i in 1..=views.raw_len() as i64 {
                            let v: LuaTable = views.raw_get(i)?;
                            v.call_method::<()>("update", ())?;
                        }
                        let core: LuaTable = require_table(lua, "core")?;
                        let rv: LuaTable = core.get("root_view")?;
                        let mouse: LuaTable = rv.get("mouse")?;
                        let mx: f64 = mouse.get("x")?;
                        let my: f64 = mouse.get("y")?;
                        this.call_method::<()>("tab_hovered_update", (mx, my))?;
                        let tab_width: f64 = this.call_method("target_tab_width", ())?;
                        let tab_offset: i64 = this.get("tab_offset")?;
                        let move_towards: LuaFunction = this.get("move_towards")?;
                        move_towards.call::<()>((
                            this.clone(),
                            "tab_shift",
                            tab_width * (tab_offset - 1) as f64,
                            LuaValue::Nil,
                            "tabs",
                        ))?;
                        move_towards.call::<()>((
                            this.clone(),
                            "tab_width",
                            tab_width,
                            LuaValue::Nil,
                            "tabs",
                        ))?;
                    } else {
                        let a: LuaTable = this.get("a")?;
                        let b: LuaTable = this.get("b")?;
                        a.call_method::<()>("update", ())?;
                        b.call_method::<()>("update", ())?;
                    }
                    Ok(())
                })?,
            )?;

            // Node:get_cached_tab_title(view, font, w)
            node.set(
                "get_cached_tab_title",
                lua.create_function(|lua, (this, view, font, w): (LuaTable, LuaTable, LuaValue, f64)| {
                    let text: String = view.call_method("get_name", ())?;
                    let doc_val: LuaValue = view.get("doc")?;
                    let dirty = if let LuaValue::Table(ref doc) = doc_val {
                        let d: LuaValue = doc.call_method("is_dirty", ())?;
                        matches!(d, LuaValue::Boolean(true))
                    } else {
                        false
                    };

                    let cache_tbl: LuaTable = this.get("tab_title_cache")?;
                    let cached: LuaValue = cache_tbl.get(view.clone())?;
                    let width_key = w.floor();

                    if let LuaValue::Table(ref c) = cached {
                        let c_text: String = c.get("text")?;
                        let c_width: f64 = c.get("width")?;
                        let c_dirty: bool = c.get::<LuaValue>("dirty")?
                            .as_boolean().unwrap_or(false);
                        let c_font: LuaValue = c.get("font")?;
                        if c_text == text && c_width == width_key && c_dirty == dirty && c_font == font {
                            return Ok(c.clone());
                        }
                    }

                    let get_width: LuaFunction = font.as_table()
                        .ok_or_else(|| LuaError::runtime("font expected"))?
                        .get("get_width")?;
                    let dots_width: f64 = get_width.call((font.clone(), "\u{2026}"))?;

                    let mut align = "center".to_string();
                    let mut display_text = text.clone();
                    let mut available_w = w;

                    if dirty {
                        let marker_w: f64 = get_width.call((font.clone(), "\u{2022} "))?;
                        available_w = (available_w - marker_w).max(0.0);
                    }

                    let text_w: f64 = get_width.call((font.clone(), display_text.as_str()))?;
                    if text_w > available_w {
                        align = "left".to_string();
                        let chars: Vec<char> = display_text.chars().collect();
                        for i in 1..=chars.len() {
                            let reduced: String = chars[..chars.len() - i].iter().collect();
                            let reduced_w: f64 = get_width.call((font.clone(), reduced.as_str()))?;
                            if reduced_w + dots_width <= available_w {
                                display_text = format!("{reduced}\u{2026}");
                                break;
                            }
                        }
                    }

                    let new_cache = lua.create_table()?;
                    new_cache.set("text", text)?;
                    new_cache.set("display_text", display_text)?;
                    new_cache.set("width", width_key)?;
                    new_cache.set("dirty", dirty)?;
                    new_cache.set("align", align)?;
                    new_cache.set("font", font)?;
                    cache_tbl.set(view, new_cache.clone())?;
                    Ok(new_cache)
                })?,
            )?;

            // Node:draw_tab_title(view, font, is_active, is_hovered, x, y, w, h)
            node.set(
                "draw_tab_title",
                lua.create_function(|lua, (this, view, font, is_active, is_hovered, x, y, w, h): (LuaTable, LuaTable, LuaValue, bool, bool, f64, f64, f64, f64)| {
                    let common: LuaTable = require_table(lua, "core.common")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let cache: LuaTable = this.call_method("get_cached_tab_title", (view, font.clone(), w))?;
                    let display_text: String = cache.get("display_text")?;
                    let align: String = cache.get("align")?;
                    let is_dirty: bool = cache.get::<LuaValue>("dirty")?.as_boolean().unwrap_or(false);

                    let mut draw_x = x;
                    let mut draw_w = w;

                    if is_dirty {
                        let marker = "\u{2022} ";
                        let get_width: LuaFunction = font.as_table()
                            .ok_or_else(|| LuaError::runtime("font expected"))?
                            .get("get_width")?;
                        let marker_w: f64 = get_width.call((font.clone(), marker))?;
                        let modified_color: LuaValue = style.get("modified")?;
                        let accent_color: LuaValue = style.get("accent")?;
                        let color = if !matches!(modified_color, LuaValue::Nil) {
                            modified_color
                        } else {
                            accent_color
                        };
                        common.call_function::<()>("draw_text", (font.clone(), color, marker, "left", draw_x, y, marker_w, h))?;
                        draw_x += marker_w;
                        draw_w = (draw_w - marker_w).max(0.0);
                    }

                    let color: LuaValue = if is_active || is_hovered {
                        style.get("text")?
                    } else {
                        style.get("dim")?
                    };
                    common.call_function::<()>("draw_text", (font, color, display_text, align, draw_x, y, draw_w, h))?;
                    Ok(())
                })?,
            )?;

            // Node:draw_tab_borders(view, is_active, is_hovered, x, y, w, h, standalone)
            node.set(
                "draw_tab_borders",
                lua.create_function(|lua, (this, _view, is_active, _is_hovered, x, y, w, h, standalone): (LuaTable, LuaValue, bool, bool, f64, f64, f64, f64, Option<bool>)| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let ds: f64 = style.get("divider_size")?;
                    let style_dim: LuaValue = style.get("dim")?;
                    let style_padding: LuaTable = style.get("padding")?;
                    let pad_y: f64 = style_padding.get("y")?;
                    let padding_y = 2.0_f64.max((pad_y * 0.75).floor());
                    let renderer: LuaTable = lua.globals().get("renderer")?;
                    let draw_rect: LuaFunction = renderer.get("draw_rect")?;
                    draw_rect.call::<()>((x + w, y + padding_y, ds, h - padding_y * 2.0, style_dim))?;

                    let is_standalone = standalone.unwrap_or(false);
                    if is_standalone {
                        let bg2: LuaValue = style.get("background2")?;
                        draw_rect.call::<()>((x - 1.0, y - 1.0, w + 2.0, h + 2.0, bg2))?;
                    }
                    if is_active {
                        let bg: LuaValue = style.get("background")?;
                        let divider: LuaValue = style.get("divider")?;
                        draw_rect.call::<()>((x, y, w, h, bg))?;
                        draw_rect.call::<()>((x, y, w, ds, divider.clone()))?;
                        draw_rect.call::<()>((x + w, y, ds, h, divider.clone()))?;
                        draw_rect.call::<()>((x - ds, y, ds, h, divider))?;
                    }
                    let _ = &this;
                    Ok((x + ds, y, w - ds * 2.0, h))
                })?,
            )?;

            // Node:draw_tab(view, is_active, is_hovered, is_close_hovered, x, y, w, h, standalone)
            #[allow(clippy::too_many_arguments)]
            node.set(
                "draw_tab",
                lua.create_function(|lua, (this, view, is_active, is_hovered, is_close_hovered, x, y, w, h, standalone): (LuaTable, LuaTable, bool, bool, bool, f64, f64, f64, f64, Option<bool>)| {
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let style_font: LuaValue = style.get("font")?;
                    let style_padding: LuaTable = style.get("padding")?;
                    let pad_y: f64 = style_padding.get("y")?;
                    let pad_x: f64 = style_padding.get("x")?;
                    let style_margin: LuaTable = style.get("margin")?;
                    let tab_margin: LuaTable = style_margin.get("tab")?;
                    let margin_y: f64 = tab_margin.get("top")?;
                    let _ = pad_y;

                    let is_standalone = standalone.unwrap_or(false);

                    let (bx, by, bw, bh): (f64, f64, f64, f64) = this.call_method(
                        "draw_tab_borders",
                        (view.clone(), is_active, is_hovered, x, y + margin_y, w, h - margin_y, is_standalone),
                    )?;

                    // Close button
                    let icon_font: LuaValue = style.get("icon_font")?;
                    let icon_w: f64 = match &icon_font {
                        LuaValue::Table(t) => t.call_method("get_width", "C")?,
                        LuaValue::UserData(ud) => ud.call_method("get_width", "C")?,
                        _ => 14.0,
                    };
                    let font_h: f64 = match &style_font {
                        LuaValue::Table(t) => t.call_method("get_height", ())?,
                        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
                        _ => 14.0,
                    };
                    let hit_w = (icon_w + pad_x).max(font_h);
                    let cpad = (pad_x / 2.0).max(((hit_w - icon_w) / 2.0).floor());
                    let cx = bx + bw - hit_w;

                    let config: LuaTable = require_table(lua, "core.config")?;
                    let tab_close_button: bool = config.get::<LuaValue>("tab_close_button")?
                        .as_boolean().unwrap_or(true);
                    let show_close = (is_active || is_hovered) && !is_standalone && tab_close_button;

                    if show_close {
                        let renderer: LuaTable = lua.globals().get("renderer")?;
                        let draw_rect: LuaFunction = renderer.get("draw_rect")?;
                        let common: LuaTable = require_table(lua, "core.common")?;
                        let close_style: LuaValue = if is_close_hovered {
                            style.get("text")?
                        } else {
                            style.get("dim")?
                        };
                        if is_close_hovered {
                            let line_hl: LuaTable = style.get("line_highlight")?;
                            let hover_bg = lua.create_table()?;
                            for i in 1..=4i64 {
                                let v: LuaValue = line_hl.raw_get(i)?;
                                hover_bg.raw_set(i, v)?;
                            }
                            hover_bg.raw_set(4, 150)?;
                            let padding_y_val: f64 = style_padding.get("y")?;
                            draw_rect.call::<()>((cx, by + padding_y_val / 2.0, hit_w, bh - padding_y_val, hover_bg))?;
                        }
                        common.call_function::<()>("draw_text", (icon_font, close_style, "C", LuaValue::Nil, cx + cpad, by, icon_w, bh))?;
                    }

                    // Title
                    let title_x = bx + cpad;
                    let title_w = cx - title_x;
                    let core: LuaTable = require_table(lua, "core")?;
                    let push_clip: LuaFunction = core.get("push_clip_rect")?;
                    let pop_clip: LuaFunction = core.get("pop_clip_rect")?;
                    push_clip.call::<()>((title_x, by, title_w, bh))?;
                    this.call_method::<()>("draw_tab_title", (view, style_font, is_active, is_hovered, title_x, by, title_w, bh))?;
                    pop_clip.call::<()>(())?;
                    Ok(())
                })?,
            )?;

            // Node:draw_tabs()
            node.set(
                "draw_tabs",
                lua.create_function(|lua, this: LuaTable| {
                    let (_, y, _w, h, scroll_padding): (f64, f64, f64, f64, f64) =
                        this.call_method("get_scroll_button_rect", 1)?;
                    let position: LuaTable = this.get("position")?;
                    let x: f64 = position.get("x")?;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    let style: LuaTable = require_table(lua, "core.style")?;
                    let ds: f64 = style.get("divider_size")?;
                    let core: LuaTable = require_table(lua, "core")?;
                    let push_clip: LuaFunction = core.get("push_clip_rect")?;
                    let pop_clip: LuaFunction = core.get("pop_clip_rect")?;
                    push_clip.call::<()>((x, y, size_x, h))?;

                    let renderer: LuaTable = lua.globals().get("renderer")?;
                    let draw_rect: LuaFunction = renderer.get("draw_rect")?;
                    let bg2: LuaValue = style.get("background2")?;
                    let divider: LuaValue = style.get("divider")?;
                    draw_rect.call::<()>((x, y, size_x, h, bg2))?;
                    draw_rect.call::<()>((x, y + h - ds, size_x, ds, divider))?;

                    let tabs_number: i64 = this.call_method("get_visible_tabs_number", ())?;
                    let tab_offset: i64 = this.get("tab_offset")?;
                    let views: LuaTable = this.get("views")?;
                    let active_view: LuaTable = this.get("active_view")?;
                    let hovered_tab: LuaValue = this.get("hovered_tab")?;
                    let hovered_close: LuaValue = this.get("hovered_close")?;

                    for i in tab_offset..=(tab_offset + tabs_number - 1) {
                        let view: LuaTable = views.raw_get(i)?;
                        let (tx, ty, tw, th): (f64, f64, f64, f64) =
                            this.call_method("get_tab_rect", i)?;
                        let is_active = view == active_view;
                        let is_hovered = hovered_tab.as_integer() == Some(i);
                        let is_close_hovered = hovered_close.as_integer() == Some(i);
                        this.call_method::<()>("draw_tab", (
                            view, is_active, is_hovered, is_close_hovered,
                            tx, ty, tw, th,
                        ))?;
                    }

                    if (views.raw_len() as i64) > tabs_number {
                        let style_font: LuaValue = style.get("font")?;
                        let w_char: f64 = match &style_font {
                            LuaValue::Table(t) => t.call_method("get_width", ">")?,
                            LuaValue::UserData(ud) => ud.call_method("get_width", ">")?,
                            _ => 8.0,
                        };
                        let pad = w_char;
                        let (xrb, yrb, wrb, _hrb): (f64, f64, f64, f64) =
                            this.call_method("get_scroll_button_rect", 1)?;
                        draw_rect.call::<()>((xrb + pad, yrb, wrb * 2.0, h, style.get::<LuaValue>("background2")?))?;

                        let hovered_scroll: i64 = this.get::<LuaValue>("hovered_scroll_button")?
                            .as_integer().unwrap_or(0);
                        let left_style: LuaValue = if hovered_scroll == 1 && tab_offset > 1 {
                            style.get("text")?
                        } else {
                            style.get("dim")?
                        };
                        let common: LuaTable = require_table(lua, "core.common")?;
                        common.call_function::<()>("draw_text", (style_font.clone(), left_style, "<", LuaValue::Nil, xrb + scroll_padding, yrb, 0.0, h))?;

                        let (xrb2, yrb2, _wrb2, _hrb2): (f64, f64, f64, f64) =
                            this.call_method("get_scroll_button_rect", 2)?;
                        let right_style: LuaValue = if hovered_scroll == 2 && (views.raw_len() as i64) > tab_offset + tabs_number - 1 {
                            style.get("text")?
                        } else {
                            style.get("dim")?
                        };
                        common.call_function::<()>("draw_text", (style_font, right_style, ">", LuaValue::Nil, xrb2 + scroll_padding, yrb2, 0.0, h))?;
                    }

                    pop_clip.call::<()>(())?;
                    Ok(())
                })?,
            )?;

            // Node:draw()
            node.set(
                "draw",
                lua.create_function(|lua, this: LuaTable| {
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let should_show: bool = this.call_method("should_show_tabs", ())?;
                        if should_show {
                            this.call_method::<()>("draw_tabs", ())?;
                        }
                        let av: LuaTable = this.get("active_view")?;
                        let pos: LuaTable = av.get("position")?;
                        let size: LuaTable = av.get("size")?;
                        let px: f64 = pos.get("x")?;
                        let py: f64 = pos.get("y")?;
                        let sx: f64 = size.get("x")?;
                        let sy: f64 = size.get("y")?;
                        let core: LuaTable = require_table(lua, "core")?;
                        let push_clip: LuaFunction = core.get("push_clip_rect")?;
                        let pop_clip: LuaFunction = core.get("pop_clip_rect")?;
                        push_clip.call::<()>((px, py, sx, sy))?;
                        av.call_method::<()>("draw", ())?;
                        pop_clip.call::<()>(())?;
                    } else {
                        let (x, y, w, h): (f64, f64, f64, f64) =
                            this.call_method("get_divider_rect", ())?;
                        let style: LuaTable = require_table(lua, "core.style")?;
                        let divider: LuaValue = style.get("divider")?;
                        let renderer: LuaTable = lua.globals().get("renderer")?;
                        let draw_rect: LuaFunction = renderer.get("draw_rect")?;
                        draw_rect.call::<()>((x, y, w, h, divider))?;
                        this.call_method::<()>("propagate", "draw")?;
                    }
                    Ok(())
                })?,
            )?;

            // Node:is_empty()
            node.set("is_empty", {
                let ek = Arc::clone(&ev_key);
                lua.create_function(move |lua, this: LuaTable| {
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let views: LuaTable = this.get("views")?;
                        let len = views.raw_len();
                        if len == 0 {
                            return Ok(true);
                        }
                        if len == 1 {
                            let first: LuaValue = views.raw_get(1)?;
                            if let LuaValue::Table(ref fv) = first {
                                let ev_cls: LuaTable = lua.registry_value(&ek)?;
                                let is_empty: bool = fv.call_method("is", ev_cls)?;
                                return Ok(is_empty);
                            }
                        }
                        Ok(false)
                    } else {
                        let a: LuaTable = this.get("a")?;
                        let b: LuaTable = this.get("b")?;
                        let a_empty: bool = a.call_method("is_empty", ())?;
                        let b_empty: bool = b.call_method("is_empty", ())?;
                        Ok(a_empty && b_empty)
                    }
                })?
            })?;

            // Node:is_in_tab_area(x, y)
            node.set(
                "is_in_tab_area",
                lua.create_function(|_lua, (this, x, y): (LuaTable, f64, f64)| {
                    let should_show: bool = this.call_method("should_show_tabs", ())?;
                    if !should_show {
                        return Ok(false);
                    }
                    let (_, ty, _, th, _pad): (f64, f64, f64, f64, f64) =
                        this.call_method("get_scroll_button_rect", 1)?;
                    let _ = x;
                    Ok(y >= ty && y < ty + th)
                })?,
            )?;

            // Node:close_all_docviews(keep_active)
            node.set("close_all_docviews", {
                let ek = Arc::clone(&ev_key);
                lua.create_function(move |lua, (this, keep_active): (LuaTable, Option<bool>)| {
                    let keep = keep_active.unwrap_or(false);
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let node_active_view: LuaValue = this.get("active_view")?;
                        let mut lost_active_view = false;
                        let views: LuaTable = this.get("views")?;
                        let mut i = 1i64;
                        loop {
                            let len = views.raw_len() as i64;
                            if i > len {
                                break;
                            }
                            let view: LuaTable = views.raw_get(i)?;
                            let ctx: String = view.get::<LuaValue>("context")?
                                .as_string()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let is_active_view = if let LuaValue::Table(ref av) = node_active_view {
                                view == *av
                            } else {
                                false
                            };
                            if ctx == "session" && (!keep || !is_active_view) {
                                lua_table_remove(&views, i)?;
                                if is_active_view {
                                    lost_active_view = true;
                                }
                            } else {
                                i += 1;
                            }
                        }
                        this.set("tab_offset", 1)?;
                        let is_primary: bool = this.get::<LuaValue>("is_primary_node")?
                            .as_boolean().unwrap_or(false);
                        let remaining = views.raw_len();
                        if remaining == 0 && is_primary {
                            let ev_cls: LuaTable = lua.registry_value(&ek)?;
                            let ev: LuaTable = ev_cls.call(())?;
                            this.call_method::<()>("add_view", ev)?;
                        } else if remaining > 0 && lost_active_view {
                            let first: LuaValue = views.raw_get(1)?;
                            this.call_method::<()>("set_active_view", first)?;
                        }
                    } else {
                        let a: LuaTable = this.get("a")?;
                        let b: LuaTable = this.get("b")?;
                        a.call_method::<()>("close_all_docviews", keep)?;
                        b.call_method::<()>("close_all_docviews", keep)?;
                        let a: LuaTable = this.get("a")?;
                        let b: LuaTable = this.get("b")?;
                        let a_empty: bool = a.call_method("is_empty", ())?;
                        let a_primary: bool = a.get::<LuaValue>("is_primary_node")?
                            .as_boolean().unwrap_or(false);
                        let b_empty: bool = b.call_method("is_empty", ())?;
                        let b_primary: bool = b.get::<LuaValue>("is_primary_node")?
                            .as_boolean().unwrap_or(false);
                        if a_empty && !a_primary {
                            this.call_method::<()>("consume", b)?;
                        } else if b_empty && !b_primary {
                            this.call_method::<()>("consume", a)?;
                        }
                    }
                    Ok(())
                })?
            })?;

            // Node:is_resizable(axis)
            node.set(
                "is_resizable",
                lua.create_function(|_lua, (this, axis): (LuaTable, String)| {
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let locked: LuaValue = this.get("locked")?;
                        if let LuaValue::Table(ref locked_tbl) = locked {
                            let locked_axis: LuaValue = locked_tbl.get(axis.as_str())?;
                            if matches!(locked_axis, LuaValue::Boolean(true)) {
                                let resizable: bool = this.get::<LuaValue>("resizable")?
                                    .as_boolean().unwrap_or(false);
                                return Ok(resizable);
                            }
                            return Ok(true);
                        }
                        Ok(true)
                    } else {
                        let a: LuaTable = this.get("a")?;
                        let b: LuaTable = this.get("b")?;
                        let a_r: bool = a.call_method("is_resizable", axis.clone())?;
                        let b_r: bool = b.call_method("is_resizable", axis)?;
                        Ok(a_r && b_r)
                    }
                })?,
            )?;

            // Node:is_locked_resizable(axis)
            node.set(
                "is_locked_resizable",
                lua.create_function(|_lua, (this, axis): (LuaTable, String)| {
                    let locked: LuaValue = this.get("locked")?;
                    if let LuaValue::Table(ref locked_tbl) = locked {
                        let locked_axis: LuaValue = locked_tbl.get(axis.as_str())?;
                        if matches!(locked_axis, LuaValue::Boolean(true)) {
                            let resizable: bool = this.get::<LuaValue>("resizable")?
                                .as_boolean().unwrap_or(false);
                            return Ok(resizable);
                        }
                    }
                    Ok(false)
                })?,
            )?;

            // Node:resize(axis, value)
            node.set(
                "resize",
                lua.create_function(|_lua, (this, axis, value): (LuaTable, String, f64)| {
                    let value = value.floor();
                    let ntype: String = this.get("type")?;
                    if ntype == "leaf" {
                        let is_lr: bool = this.call_method("is_locked_resizable", axis.clone())?;
                        if is_lr {
                            let av: LuaTable = this.get("active_view")?;
                            av.call_method::<()>("set_target_size", (axis, value))?;
                        }
                    } else {
                        let split_axis = if axis == "x" { "hsplit" } else { "vsplit" };
                        if ntype == split_axis {
                            let a: LuaTable = this.get("a")?;
                            let b: LuaTable = this.get("b")?;
                            let a_lr: bool = a.call_method("is_locked_resizable", axis.clone())?;
                            let b_lr: bool = b.call_method("is_locked_resizable", axis.clone())?;
                            if a_lr && b_lr {
                                let a_size: LuaTable = a.get("size")?;
                                let a_sz: f64 = a_size.get(axis.as_str())?;
                                let rem = value - a_sz;
                                if rem >= 0.0 {
                                    let b_av: LuaTable = b.get("active_view")?;
                                    b_av.call_method::<()>("set_target_size", (axis, rem))?;
                                } else {
                                    let b_av: LuaTable = b.get("active_view")?;
                                    b_av.call_method::<()>("set_target_size", (axis.clone(), 0.0))?;
                                    let a_av: LuaTable = a.get("active_view")?;
                                    a_av.call_method::<()>("set_target_size", (axis, value))?;
                                }
                            }
                        } else {
                            let a: LuaTable = this.get("a")?;
                            let b: LuaTable = this.get("b")?;
                            let a_r: bool = a.call_method("is_resizable", axis.clone())?;
                            let b_r: bool = b.call_method("is_resizable", axis.clone())?;
                            if a_r && b_r {
                                a.call_method::<()>("resize", (axis.clone(), value))?;
                                b.call_method::<()>("resize", (axis, value))?;
                            }
                        }
                    }
                    Ok(())
                })?,
            )?;

            // Node:get_split_type(mouse_x, mouse_y)
            node.set("get_split_type", {
                let nmk = Arc::clone(&nm_key);
                lua.create_function(move |lua, (this, mouse_x, mouse_y): (LuaTable, f64, f64)| {
                    let (_, _, _, tab_h, _pad): (f64, f64, f64, f64, f64) =
                        this.call_method("get_scroll_button_rect", 1)?;
                    let nm: LuaTable = lua.registry_value(&nmk)?;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    let size_y: f64 = size.get("y")?;
                    let position: LuaTable = this.get("position")?;
                    let pos_x: f64 = position.get("x")?;
                    let pos_y: f64 = position.get("y")?;
                    let result: String = nm.call_function("split_type", (
                        size_x, size_y, tab_h,
                        mouse_x - pos_x, mouse_y - pos_y,
                    ))?;
                    Ok(result)
                })?
            })?;

            // Node:get_drag_overlay_tab_position(x, y, dragged_node, dragged_index)
            node.set("get_drag_overlay_tab_position", {
                let nmk = Arc::clone(&nm_key);
                lua.create_function(move |lua, (this, x, _y, dragged_node, dragged_index): (LuaTable, f64, f64, Option<LuaTable>, Option<i64>)| {
                    let nm: LuaTable = lua.registry_value(&nmk)?;
                    let views: LuaTable = this.get("views")?;
                    let tab_offset: i64 = this.get("tab_offset")?;
                    let config: LuaTable = require_table(lua, "core.config")?;
                    let max_tabs: i64 = config.get("max_tabs")?;
                    let tab_width: f64 = this.get("tab_width")?;
                    let tab_shift: f64 = this.get("tab_shift")?;
                    let size: LuaTable = this.get("size")?;
                    let size_x: f64 = size.get("x")?;
                    let position: LuaTable = this.get("position")?;
                    let pos_x: f64 = position.get("x")?;

                    let di = if dragged_node.as_ref() == Some(&this) {
                        dragged_index.unwrap_or(0)
                    } else {
                        0
                    };

                    let (tab_index, tab_x, tab_w): (i64, f64, f64) = nm.call_function("drag_overlay_tab_position", (
                        views.raw_len() as i64, tab_offset, max_tabs,
                        tab_width, tab_shift, size_x,
                        x - pos_x, di,
                    ))?;

                    let clamped = tab_index.max(1).min(views.raw_len() as i64);
                    let (_, tab_y, _, tab_h, margin_y): (f64, f64, f64, f64, f64) =
                        this.call_method("get_tab_rect", clamped)?;

                    Ok((tab_index, pos_x + tab_x, tab_y + margin_y, tab_w, tab_h - margin_y))
                })?
            })?;

            Ok(LuaValue::Table(node))
        })?,
    )
}

/// Shared split-size calculation for hsplit/vsplit layouts.
fn calc_split_sizes(
    _lua: &Lua,
    this: &LuaTable,
    x_axis: &str,
    y_axis: &str,
    x1: &LuaValue,
    x2: &LuaValue,
    divider_size: f64,
) -> LuaResult<()> {
    let x1_num = match x1 {
        LuaValue::Number(v) => Some(*v),
        LuaValue::Integer(v) => Some(*v as f64),
        _ => None,
    };
    let x2_num = match x2 {
        LuaValue::Number(v) => Some(*v),
        LuaValue::Integer(v) => Some(*v as f64),
        _ => None,
    };

    let ds = if (x1_num.is_some() && x1_num.unwrap_or(0.0) < 1.0)
        || (x2_num.is_some() && x2_num.unwrap_or(0.0) < 1.0)
    {
        0.0
    } else {
        divider_size
    };

    let self_size: LuaTable = this.get("size")?;
    let self_pos: LuaTable = this.get("position")?;
    let size_x: f64 = self_size.get(x_axis)?;
    let size_y: f64 = self_size.get(y_axis)?;
    let pos_x: f64 = self_pos.get(x_axis)?;
    let pos_y: f64 = self_pos.get(y_axis)?;
    let divider: f64 = this.get("divider")?;

    let n = if let Some(v1) = x1_num {
        v1 + ds
    } else if let Some(v2) = x2_num {
        size_x - v2
    } else {
        (size_x * divider).floor()
    };

    let a: LuaTable = this.get("a")?;
    let b: LuaTable = this.get("b")?;
    let a_pos: LuaTable = a.get("position")?;
    let a_size: LuaTable = a.get("size")?;
    let b_pos: LuaTable = b.get("position")?;
    let b_size: LuaTable = b.get("size")?;

    a_pos.set(x_axis, pos_x)?;
    a_pos.set(y_axis, pos_y)?;
    a_size.set(x_axis, n - ds)?;
    a_size.set(y_axis, size_y)?;
    b_pos.set(x_axis, pos_x + n)?;
    b_pos.set(y_axis, pos_y)?;
    b_size.set(x_axis, size_x - n)?;
    b_size.set(y_axis, size_y)?;
    Ok(())
}
