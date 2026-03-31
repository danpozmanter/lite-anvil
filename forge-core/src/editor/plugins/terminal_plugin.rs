use mlua::prelude::*;
use serde_json::Value as JsonValue;

use super::terminal_view::walk_nodes;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Loads `{DATADIR}/assets/terminal/color_schemes.json` and returns the parsed array.
fn load_color_schemes_json(datadir: &str) -> LuaResult<Vec<JsonValue>> {
    let path = format!("{datadir}/assets/terminal/color_schemes.json");
    let source = std::fs::read_to_string(&path)
        .map_err(|e| LuaError::RuntimeError(format!("cannot read {path}: {e}")))?;
    let arr: JsonValue = serde_json::from_str(&source)
        .map_err(|e| LuaError::RuntimeError(format!("bad JSON in {path}: {e}")))?;
    arr.as_array()
        .cloned()
        .ok_or_else(|| LuaError::RuntimeError(format!("{path}: expected a JSON array")))
}

fn build_color_schemes_table(lua: &Lua, schemes: &[JsonValue]) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    for entry in schemes {
        let Some(name) = entry.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let tbl = lua.create_table()?;
        if let Some(v) = entry.get("foreground").and_then(|v| v.as_str()) {
            tbl.set("foreground", v)?;
        }
        if let Some(v) = entry.get("background").and_then(|v| v.as_str()) {
            tbl.set("background", v)?;
        }
        if let Some(v) = entry.get("cursor").and_then(|v| v.as_str()) {
            tbl.set("cursor", v)?;
        }
        if let Some(palette) = entry.get("palette").and_then(|v| v.as_array()) {
            let pal = lua.create_table()?;
            for (i, color) in palette.iter().enumerate() {
                if let Some(s) = color.as_str() {
                    pal.raw_set(i as i64 + 1, s)?;
                }
            }
            tbl.set("palette", pal)?;
        }
        out.set(name, tbl)?;
    }
    Ok(out)
}

fn set_selection_values(lua: &Lua, pairs: &[(&str, &str)]) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    for (label, value) in pairs {
        let pair = lua.create_sequence_from([*label, *value])?;
        t.push(pair)?;
    }
    Ok(t)
}

fn build_config_defaults(lua: &Lua, scheme_names: &[String]) -> LuaResult<LuaTable> {
    let cfg = lua.create_table()?;

    // config_spec
    let spec = lua.create_table()?;
    spec.set("name", "Terminal")?;

    let scrollback_entry = lua.create_table()?;
    scrollback_entry.set("label", "Scrollback")?;
    scrollback_entry.set(
        "description",
        "Maximum number of terminal history lines to keep.",
    )?;
    scrollback_entry.set("path", "scrollback")?;
    scrollback_entry.set("type", "number")?;
    scrollback_entry.set("default", 5000)?;
    scrollback_entry.set("min", 500)?;
    scrollback_entry.set("max", 50000)?;
    spec.push(scrollback_entry)?;

    let color_scheme_entry = lua.create_table()?;
    color_scheme_entry.set("label", "Color Scheme")?;
    color_scheme_entry.set("description", "Built-in terminal color scheme.")?;
    color_scheme_entry.set("path", "color_scheme")?;
    color_scheme_entry.set("type", "selection")?;
    color_scheme_entry.set("default", "eterm")?;
    let values = lua.create_table()?;
    for name in scheme_names {
        let pair = lua.create_sequence_from([name.as_str(), name.as_str()])?;
        values.push(pair)?;
    }
    color_scheme_entry.set("values", values)?;
    spec.push(color_scheme_entry)?;

    let close_entry = lua.create_table()?;
    close_entry.set("label", "Close On Exit")?;
    close_entry.set(
        "description",
        "Close terminal tabs automatically when the shell exits.",
    )?;
    close_entry.set("path", "close_on_exit")?;
    close_entry.set("type", "toggle")?;
    close_entry.set("default", true)?;
    spec.push(close_entry)?;

    let position_entry = lua.create_table()?;
    position_entry.set("label", "Open Position")?;
    position_entry.set("description", "Where new terminal views open by default.")?;
    position_entry.set("path", "open_position")?;
    position_entry.set("type", "selection")?;
    position_entry.set("default", "bottom")?;
    position_entry.set(
        "values",
        set_selection_values(
            lua,
            &[
                ("Bottom Pane", "bottom"),
                ("New Tab", "tab"),
                ("Left Pane", "left"),
                ("Right Pane", "right"),
                ("Top Pane", "top"),
            ],
        )?,
    )?;
    spec.push(position_entry)?;

    let reuse_entry = lua.create_table()?;
    reuse_entry.set("label", "Reuse Mode")?;
    reuse_entry.set(
        "description",
        "How new terminal requests reuse existing terminal views.",
    )?;
    reuse_entry.set("path", "reuse_mode")?;
    reuse_entry.set("type", "selection")?;
    reuse_entry.set("default", "pane")?;
    reuse_entry.set(
        "values",
        set_selection_values(
            lua,
            &[
                ("Same Pane", "pane"),
                ("Last Terminal", "view"),
                ("Same Project", "project"),
                ("Never Reuse", "never"),
            ],
        )?,
    )?;
    spec.push(reuse_entry)?;

    cfg.set("config_spec", spec)?;

    // Runtime defaults
    let os: LuaTable = lua.globals().get("os")?;
    let shell: String = os
        .call_function::<Option<String>>("getenv", "SHELL")?
        .unwrap_or_else(|| "sh".to_owned());
    cfg.set("shell", shell)?;
    cfg.set("shell_args", lua.create_table()?)?;
    cfg.set("scrollback", 5000)?;
    cfg.set("color_scheme", "eterm")?;
    cfg.set("close_on_exit", true)?;

    // open_position: prefer config.terminal.placement if set
    let config = require_table(lua, "core.config")?;
    let terminal_cfg: Option<LuaTable> = config.get("terminal")?;
    let placement: Option<String> = terminal_cfg
        .as_ref()
        .and_then(|t| t.get("placement").ok())
        .flatten();
    cfg.set("open_position", placement.as_deref().unwrap_or("bottom"))?;

    // reuse_mode: prefer config.terminal.reuse_mode if set
    let reuse_mode: Option<String> = terminal_cfg
        .as_ref()
        .and_then(|t| t.get("reuse_mode").ok())
        .flatten();
    cfg.set("reuse_mode", reuse_mode.as_deref().unwrap_or("pane"))?;

    Ok(cfg)
}

fn get_default_cwd(lua: &Lua) -> LuaResult<String> {
    let core = require_table(lua, "core")?;
    // Try the active view's doc abs_filename first.
    if let Ok(Some(view)) = core.get::<Option<LuaTable>>("active_view") {
        if let Ok(Some(doc)) = view.get::<Option<LuaTable>>("doc") {
            // Extract abs_filename via LuaValue to handle metamethod edge cases.
            let abs_val: LuaValue = doc.get("abs_filename").unwrap_or(LuaValue::Nil);
            if let LuaValue::String(s) = abs_val {
                if let Ok(path) = s.to_str() {
                    let path = path.to_owned();
                    if !path.is_empty() {
                        let common = require_table(lua, "core.common")?;
                        let dir: String = common.call_function("dirname", path)?;
                        return Ok(dir);
                    }
                }
            }
        }
    }
    if let Ok(Some(f)) = core.get::<Option<LuaFunction>>("root_project") {
        if let Ok(Some(p)) = f.call::<Option<LuaTable>>(()) {
            if let Ok(Some(path)) = p.get::<Option<String>>("path") {
                return Ok(path);
            }
        }
    }
    let os: LuaTable = lua.globals().get("os")?;
    let home: Option<String> = os.call_function("getenv", "HOME")?;
    Ok(home.unwrap_or_else(|| ".".to_owned()))
}

fn open_terminal(
    _lua: &Lua,
    terminal_view: &LuaTable,
    cwd: String,
    position: Option<&str>,
) -> LuaResult<()> {
    match position {
        Some(pos) => {
            terminal_view.call_function::<()>("open", (cwd, LuaValue::Nil, LuaValue::Nil, pos))?
        }
        None => terminal_view.call_function::<()>("open", cwd)?,
    }
    Ok(())
}

/// Returns `true` if the given view is a TerminalView.
fn is_terminal_view(view: &LuaTable) -> LuaResult<bool> {
    match view.get::<Option<LuaFunction>>("__tostring")? {
        Some(f) => Ok(f.call::<String>(view.clone())? == "TerminalView"),
        None => Ok(false),
    }
}

/// Switches to the terminal tab at the given offset (+1 or -1) from the active terminal
/// within the same node.
fn switch_terminal_tab(lua: &Lua, offset: i64) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let view: LuaTable = match core.get::<Option<LuaTable>>("active_view")? {
        Some(v) => v,
        None => return Ok(()),
    };
    if !is_terminal_view(&view)? {
        return Ok(());
    }
    let root_view: LuaTable = core.get("root_view")?;
    let root_node: LuaTable = root_view.get("root_node")?;
    let node: LuaTable = match root_node.call_method("get_node_for_view", view.clone())? {
        Some(n) => n,
        None => return Ok(()),
    };
    let views: LuaTable = node.get("views")?;
    let len = views.raw_len() as i64;
    let cur_idx: i64 = match node.call_method::<LuaValue>("get_view_idx", view)? {
        LuaValue::Integer(i) => i,
        _ => return Ok(()),
    };
    let next = ((cur_idx - 1 + offset).rem_euclid(len)) + 1;
    let next_view: LuaValue = views.raw_get(next)?;
    node.call_method::<()>("set_active_view", next_view)?;
    Ok(())
}

/// Switches to the Nth view (1-based) in the same node as the active terminal.
fn switch_terminal_tab_n(lua: &Lua, n: i64) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let view: LuaTable = match core.get::<Option<LuaTable>>("active_view")? {
        Some(v) => v,
        None => return Ok(()),
    };
    if !is_terminal_view(&view)? {
        return Ok(());
    }
    let root_view: LuaTable = core.get("root_view")?;
    let root_node: LuaTable = root_view.get("root_node")?;
    let node: LuaTable = match root_node.call_method("get_node_for_view", view)? {
        Some(nd) => nd,
        None => return Ok(()),
    };
    let views: LuaTable = node.get("views")?;
    let len = views.raw_len() as i64;
    if n < 1 || n > len {
        return Ok(());
    }
    let target: LuaValue = views.raw_get(n)?;
    node.call_method::<()>("set_active_view", target)?;
    Ok(())
}

fn register_commands(lua: &Lua, terminal_view_key: &LuaRegistryKey) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;
    let cmds = lua.create_table()?;

    let tv_new = {
        let key = lua.registry_value::<LuaTable>(terminal_view_key)?.clone();
        let key = lua.create_registry_value(key)?;
        lua.create_function(move |lua, ()| {
            let tv: LuaTable = lua.registry_value(&key)?;
            let cwd = get_default_cwd(lua)?;
            open_terminal(lua, &tv, cwd, None)
        })?
    };
    cmds.set("terminal:new", tv_new)?;

    for (cmd_name, pos) in &[
        ("terminal:new-tab", "tab"),
        ("terminal:new-bottom", "bottom"),
        ("terminal:new-left", "left"),
        ("terminal:new-right", "right"),
        ("terminal:new-top", "top"),
    ] {
        let tv_key =
            lua.create_registry_value(lua.registry_value::<LuaTable>(terminal_view_key)?)?;
        let pos = *pos;
        cmds.set(
            *cmd_name,
            lua.create_function(move |lua, ()| {
                let tv: LuaTable = lua.registry_value(&tv_key)?;
                let cwd = get_default_cwd(lua)?;
                open_terminal(lua, &tv, cwd, Some(pos))
            })?,
        )?;
    }

    {
        let tv_key =
            lua.create_registry_value(lua.registry_value::<LuaTable>(terminal_view_key)?)?;
        cmds.set(
            "terminal:new-in-project",
            lua.create_function(move |lua, ()| {
                let tv: LuaTable = lua.registry_value(&tv_key)?;
                let core = require_table(lua, "core")?;
                let project_path: Option<String> = (|| -> LuaResult<Option<String>> {
                    let f: Option<LuaFunction> = core.get("root_project")?;
                    if let Some(f) = f {
                        let p: Option<LuaTable> = f.call(())?;
                        if let Some(p) = p {
                            return p.get("path");
                        }
                    }
                    Ok(None)
                })()?;
                let cwd = match project_path {
                    Some(p) => p,
                    None => get_default_cwd(lua)?,
                };
                tv.call_function::<()>("open", (cwd, LuaValue::Nil, "Terminal: project"))
            })?,
        )?;
    }

    {
        let tv_key =
            lua.create_registry_value(lua.registry_value::<LuaTable>(terminal_view_key)?)?;
        cmds.set(
            "terminal:new-next-to-file",
            lua.create_function(move |lua, ()| {
                let tv: LuaTable = lua.registry_value(&tv_key)?;
                let cwd = get_default_cwd(lua)?;
                open_terminal(lua, &tv, cwd, None)
            })?,
        )?;
    }

    {
        cmds.set(
            "terminal:close",
            lua.create_function(|lua, ()| {
                let core = require_table(lua, "core")?;
                let view: Option<LuaTable> = core.get("active_view")?;
                if let Some(view) = view {
                    let name: String = view.call_method("__tostring", ())?;
                    if name == "TerminalView" {
                        let root_view: LuaTable = core.get("root_view")?;
                        let root_node: LuaTable = root_view.get("root_node")?;
                        let node: Option<LuaTable> =
                            root_node.call_method("get_node_for_view", view.clone())?;
                        if let Some(node) = node {
                            node.call_method::<()>("close_view", (root_node, view))?;
                        }
                    }
                }
                Ok(())
            })?,
        )?;
    }

    cmds.set(
        "terminal:next-tab",
        lua.create_function(|lua, ()| switch_terminal_tab(lua, 1))?,
    )?;
    cmds.set(
        "terminal:previous-tab",
        lua.create_function(|lua, ()| switch_terminal_tab(lua, -1))?,
    )?;
    for n in 1..=9i64 {
        cmds.set(
            format!("terminal:tab-{n}"),
            lua.create_function(move |lua, ()| switch_terminal_tab_n(lua, n))?,
        )?;
    }
    cmds.set(
        "terminal:list",
        lua.create_function(|lua, ()| {
            let core = require_table(lua, "core")?;
            let root_view: LuaTable = core.get("root_view")?;
            let root_node: LuaTable = root_view.get("root_node")?;
            let items = lua.create_table()?;
            walk_nodes(&root_node, &mut |node| {
                let ntype: String = node.get("type")?;
                if ntype != "leaf" {
                    return Ok(false);
                }
                let views: LuaTable = node.get("views")?;
                for i in 1..=views.raw_len() as i64 {
                    let v: LuaTable = views.raw_get(i)?;
                    if is_terminal_view(&v)? {
                        let name: String = v.call_method("get_name", ())?;
                        let cwd: String =
                            v.get::<Option<String>>("cwd")?.unwrap_or_default();
                        let item = lua.create_table()?;
                        item.set("text", name)?;
                        item.set("info", cwd)?;
                        item.set("view", v)?;
                        items.raw_set(items.raw_len() + 1, item)?;
                    }
                }
                Ok(false)
            })?;
            if items.raw_len() == 0 {
                return Ok(());
            }
            let command_view: LuaTable = core.get("command_view")?;
            let opts = lua.create_table()?;
            opts.set(
                "submit",
                lua.create_function(|lua, item: LuaTable| {
                    let view: LuaTable = item.get("view")?;
                    let core = require_table(lua, "core")?;
                    let set_active: LuaFunction = core.get("set_active_view")?;
                    set_active.call::<()>(view)
                })?,
            )?;
            command_view.call_method::<()>("enter", ("Terminal List", opts, items))
        })?,
    )?;

    command.call_function::<()>("add", (LuaValue::Nil, cmds))?;

    let keymap = require_table(lua, "core.keymap")?;
    let bindings = lua.create_table()?;
    bindings.set("ctrl+shift+t", "terminal:new")?;
    bindings.set("ctrl+alt+right", "terminal:next-tab")?;
    bindings.set("ctrl+alt+left", "terminal:previous-tab")?;
    bindings.set("ctrl+alt+t", "terminal:list")?;
    for n in 1..=9i64 {
        bindings.set(format!("ctrl+alt+{n}"), format!("terminal:tab-{n}"))?;
    }
    keymap.call_function::<()>("add", bindings)?;

    Ok(())
}

fn init_terminal(lua: &Lua) -> LuaResult<LuaValue> {
    let datadir: String = lua.globals().get("DATADIR")?;
    let schemes = load_color_schemes_json(&datadir)?;
    let scheme_names: Vec<String> = schemes
        .iter()
        .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(str::to_owned))
        .collect();

    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let defaults = build_config_defaults(lua, &scheme_names)?;
    let common = require_table(lua, "core.common")?;
    let merged: LuaTable =
        common.call_function("merge", (defaults, plugins.get::<LuaValue>("terminal")?))?;
    plugins.set("terminal", merged)?;

    let terminal_view = require_table(lua, "plugins.terminal.view")?;
    let tv_key = lua.create_registry_value(terminal_view.clone())?;

    register_commands(lua, &tv_key)?;

    Ok(LuaValue::Table(terminal_view))
}

/// Registers `plugins.terminal` (config + commands + keymap) and `plugins.terminal.colors`
/// (color scheme data loaded from `{DATADIR}/assets/terminal/color_schemes.json`).
/// The TerminalView class itself is in `terminal_view.rs`.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;

    preload.set(
        "plugins.terminal",
        lua.create_function(|lua, ()| init_terminal(lua))?,
    )?;

    preload.set(
        "plugins.terminal.colors",
        lua.create_function(|lua, ()| {
            let datadir: String = lua.globals().get("DATADIR")?;
            let schemes = load_color_schemes_json(&datadir)?;
            let tbl = build_color_schemes_table(lua, &schemes)?;
            Ok(LuaValue::Table(tbl))
        })?,
    )
}
