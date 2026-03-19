use mlua::prelude::*;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
struct ViewEntry {
    id: i64,
    is_doc: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct LockedState {
    x: bool,
    y: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct NodePlan {
    node_type: String,
    divider: f64,
    locked: Option<LockedState>,
    resizable: bool,
    is_primary_node: bool,
    views: Vec<ViewEntry>,
    active_view: Option<i64>,
    tab_offset: usize,
    a: Option<Box<NodePlan>>,
    b: Option<Box<NodePlan>>,
}

fn parse_locked(table: Option<LuaTable>) -> LuaResult<Option<LockedState>> {
    let Some(table) = table else {
        return Ok(None);
    };
    Ok(Some(LockedState {
        x: table.get::<Option<bool>>("x")?.unwrap_or(false),
        y: table.get::<Option<bool>>("y")?.unwrap_or(false),
    }))
}

fn parse_view_entry(table: LuaTable) -> LuaResult<ViewEntry> {
    Ok(ViewEntry {
        id: table.get("id")?,
        is_doc: table.get::<Option<bool>>("doc")?.unwrap_or(false),
    })
}

fn parse_node_plan(table: LuaTable) -> LuaResult<NodePlan> {
    let node_type: String = table.get("type")?;
    let divider = table.get::<Option<f64>>("divider")?.unwrap_or(0.5);
    let locked = parse_locked(table.get::<Option<LuaTable>>("locked")?)?;
    let resizable = table.get::<Option<bool>>("resizable")?.unwrap_or(false);
    let is_primary_node = table
        .get::<Option<bool>>("is_primary_node")?
        .unwrap_or(false);
    let active_view = table.get::<Option<i64>>("active_view")?;
    let tab_offset = table.get::<Option<usize>>("tab_offset")?.unwrap_or(1);
    let mut views = Vec::new();
    if node_type == "leaf" {
        if let Some(view_table) = table.get::<Option<LuaTable>>("views")? {
            for value in view_table.sequence_values::<LuaTable>() {
                views.push(parse_view_entry(value?)?);
            }
        }
    }
    let a = if node_type != "leaf" {
        table
            .get::<Option<LuaTable>>("a")?
            .map(parse_node_plan)
            .transpose()?
            .map(Box::new)
    } else {
        None
    };
    let b = if node_type != "leaf" {
        table
            .get::<Option<LuaTable>>("b")?
            .map(parse_node_plan)
            .transpose()?
            .map(Box::new)
    } else {
        None
    };
    Ok(NodePlan {
        node_type,
        divider,
        locked,
        resizable,
        is_primary_node,
        views,
        active_view,
        tab_offset,
        a,
        b,
    })
}

fn node_to_lua(lua: &Lua, node: &NodePlan) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    out.set("type", node.node_type.clone())?;
    out.set("divider", node.divider)?;
    out.set("resizable", node.resizable)?;
    out.set("is_primary_node", node.is_primary_node)?;
    if let Some(active) = node.active_view {
        out.set("active_view", active)?;
    }
    out.set("tab_offset", node.tab_offset.max(1) as i64)?;
    if let Some(locked) = &node.locked {
        let lt = lua.create_table()?;
        lt.set("x", locked.x)?;
        lt.set("y", locked.y)?;
        out.set("locked", lt)?;
    }
    if node.node_type == "leaf" {
        let views = lua.create_table()?;
        for (i, view) in node.views.iter().enumerate() {
            views.set(i + 1, view.id)?;
        }
        out.set("views", views)?;
    } else {
        if let Some(a) = &node.a {
            out.set("a", node_to_lua(lua, a)?)?;
        }
        if let Some(b) = &node.b {
            out.set("b", node_to_lua(lua, b)?)?;
        }
    }
    Ok(out)
}

fn collect_present_ids(node: &NodePlan, out: &mut HashSet<i64>) {
    if node.node_type == "leaf" {
        for view in &node.views {
            out.insert(view.id);
        }
    } else {
        if let Some(a) = &node.a {
            collect_present_ids(a, out);
        }
        if let Some(b) = &node.b {
            collect_present_ids(b, out);
        }
    }
}

fn find_primary_leaf_mut(node: &mut NodePlan) -> Option<&mut NodePlan> {
    if node.node_type == "leaf" {
        return if node.is_primary_node {
            Some(node)
        } else {
            None
        };
    }
    if let Some(a) = node.a.as_mut()
        && let Some(found) = find_primary_leaf_mut(a)
    {
        return Some(found);
    }
    if let Some(b) = node.b.as_mut()
        && let Some(found) = find_primary_leaf_mut(b)
    {
        return Some(found);
    }
    None
}

fn has_primary(node: &NodePlan) -> bool {
    if node.node_type == "leaf" {
        return node.is_primary_node;
    }
    node.a.as_ref().is_some_and(|a| has_primary(a))
        || node.b.as_ref().is_some_and(|b| has_primary(b))
}

fn first_leaf_mut(node: &mut NodePlan) -> &mut NodePlan {
    if node.node_type == "leaf" {
        return node;
    }
    if let Some(a) = node.a.as_mut() {
        return first_leaf_mut(a);
    }
    if let Some(b) = node.b.as_mut() {
        return first_leaf_mut(b);
    }
    unreachable!("branch node missing children")
}

fn primary_or_first_leaf_mut(node: &mut NodePlan) -> &mut NodePlan {
    if node.node_type == "leaf" {
        return node;
    }
    if node.a.as_ref().is_some_and(|a| has_primary(a)) {
        return find_primary_leaf_mut(node.a.as_mut().expect("left child")).expect("primary leaf");
    }
    if node.b.as_ref().is_some_and(|b| has_primary(b)) {
        return find_primary_leaf_mut(node.b.as_mut().expect("right child")).expect("primary leaf");
    }
    if let Some(a) = node.a.as_mut() {
        return first_leaf_mut(a);
    }
    first_leaf_mut(node.b.as_mut().expect("branch node missing children"))
}

fn contains_view_id(node: &NodePlan, target: i64) -> bool {
    if node.node_type == "leaf" {
        return node.views.iter().any(|view| view.id == target);
    }
    node.a.as_ref().is_some_and(|a| contains_view_id(a, target))
        || node.b.as_ref().is_some_and(|b| contains_view_id(b, target))
}

fn restore_focus_node(
    state: &NodePlan,
    live_doc_ids: &HashSet<i64>,
    live_view_ids: &HashSet<i64>,
    assigned: &mut HashSet<i64>,
) -> NodePlan {
    if state.node_type == "leaf" {
        let mut restored_views = Vec::new();
        for view in &state.views {
            let live = if view.is_doc {
                live_doc_ids.contains(&view.id)
            } else {
                live_view_ids.contains(&view.id)
            };
            if live && assigned.insert(view.id) {
                restored_views.push(view.clone());
            }
        }
        let active_view = state
            .active_view
            .filter(|id| restored_views.iter().any(|view| view.id == *id));
        let max_views = restored_views.len().max(1);
        return NodePlan {
            node_type: "leaf".to_string(),
            divider: state.divider,
            locked: state.locked.clone(),
            resizable: state.resizable,
            is_primary_node: state.is_primary_node,
            views: restored_views,
            active_view,
            tab_offset: state.tab_offset.clamp(1, max_views),
            a: None,
            b: None,
        };
    }

    let a = state.a.as_ref().map(|child| {
        Box::new(restore_focus_node(
            child,
            live_doc_ids,
            live_view_ids,
            assigned,
        ))
    });
    let b = state.b.as_ref().map(|child| {
        Box::new(restore_focus_node(
            child,
            live_doc_ids,
            live_view_ids,
            assigned,
        ))
    });
    NodePlan {
        node_type: state.node_type.clone(),
        divider: state.divider,
        locked: state.locked.clone(),
        resizable: state.resizable,
        is_primary_node: state.is_primary_node,
        views: Vec::new(),
        active_view: None,
        tab_offset: state.tab_offset,
        a,
        b,
    }
}

fn restore_focus_layout(
    mut snapshot: NodePlan,
    live_doc_ids: &HashSet<i64>,
    live_view_ids: &HashSet<i64>,
    current_active_id: Option<i64>,
    previous_active_id: Option<i64>,
) -> (NodePlan, Option<i64>) {
    let mut assigned = HashSet::new();
    let mut restored = restore_focus_node(&snapshot, live_doc_ids, live_view_ids, &mut assigned);

    let mut remaining = Vec::new();
    for id in live_view_ids {
        if !assigned.contains(id) {
            remaining.push(*id);
        }
    }
    remaining.sort_unstable();

    {
        let primary = primary_or_first_leaf_mut(&mut restored);
        for id in remaining {
            primary.views.push(ViewEntry {
                id,
                is_doc: live_doc_ids.contains(&id),
            });
        }
        if primary.active_view.is_none() {
            primary.active_view = primary.views.first().map(|view| view.id);
        }
        let max_views = primary.views.len().max(1);
        primary.tab_offset = primary.tab_offset.clamp(1, max_views);
    }

    let target_view_id = current_active_id
        .filter(|id| contains_view_id(&restored, *id))
        .or_else(|| previous_active_id.filter(|id| contains_view_id(&restored, *id)))
        .or_else(|| {
            let mut ids = HashSet::new();
            collect_present_ids(&restored, &mut ids);
            ids.into_iter().next()
        });

    snapshot = restored;
    (snapshot, target_view_id)
}

fn set_from_sequence(table: LuaTable) -> LuaResult<HashSet<i64>> {
    let mut out = HashSet::new();
    for value in table.sequence_values::<i64>() {
        out.insert(value?);
    }
    Ok(out)
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set(
        "restore_focus_layout",
        lua.create_function(
            |lua,
             (snapshot, live_doc_ids, live_view_ids, current_active_id, previous_active_id): (
                LuaTable,
                LuaTable,
                LuaTable,
                Option<i64>,
                Option<i64>,
            )| {
                let snapshot = parse_node_plan(snapshot)?;
                let live_doc_ids = set_from_sequence(live_doc_ids)?;
                let live_view_ids = set_from_sequence(live_view_ids)?;
                let (restored, target_view_id) = restore_focus_layout(
                    snapshot,
                    &live_doc_ids,
                    &live_view_ids,
                    current_active_id,
                    previous_active_id,
                );
                let out = lua.create_table()?;
                out.set("root", node_to_lua(lua, &restored)?)?;
                out.set("target_view_id", target_view_id)?;
                Ok(out)
            },
        )?,
    )?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::{LockedState, NodePlan, ViewEntry, restore_focus_layout};
    use std::collections::HashSet;

    fn leaf(primary: bool, views: &[(i64, bool)], active: Option<i64>) -> NodePlan {
        NodePlan {
            node_type: "leaf".to_string(),
            divider: 0.5,
            locked: None,
            resizable: false,
            is_primary_node: primary,
            views: views
                .iter()
                .map(|(id, is_doc)| ViewEntry {
                    id: *id,
                    is_doc: *is_doc,
                })
                .collect(),
            active_view: active,
            tab_offset: 1,
            a: None,
            b: None,
        }
    }

    #[test]
    fn restore_focus_layout_filters_closed_docs_and_preserves_primary() {
        let snapshot = NodePlan {
            node_type: "hsplit".to_string(),
            divider: 0.5,
            locked: Some(LockedState { x: false, y: false }),
            resizable: false,
            is_primary_node: false,
            views: Vec::new(),
            active_view: None,
            tab_offset: 1,
            a: Some(Box::new(leaf(true, &[(1, true), (2, false)], Some(2)))),
            b: Some(Box::new(leaf(false, &[(3, true)], Some(3)))),
        };
        let live_doc_ids = HashSet::from([1_i64]);
        let live_view_ids = HashSet::from([1_i64, 2_i64, 4_i64]);
        let (restored, target) =
            restore_focus_layout(snapshot, &live_doc_ids, &live_view_ids, Some(4), Some(2));

        assert_eq!(target, Some(4));
        let primary = restored.a.expect("primary leaf");
        let ids: Vec<i64> = primary.views.into_iter().map(|view| view.id).collect();
        assert_eq!(ids, vec![1, 2, 4]);
    }
}
