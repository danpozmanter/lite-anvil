use std::sync::Arc;

use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Height of the breadcrumb bar in pixels, derived from the UI font.
fn breadcrumb_height(lua: &Lua) -> LuaResult<f64> {
    let style = require_table(lua, "core.style")?;
    let font: LuaValue = style.get("font")?;
    let fh: f64 = match &font {
        LuaValue::Table(t) => t.call_method("get_height", ())?,
        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
        _ => return Ok(0.0),
    };
    let padding: LuaTable = style.get("padding")?;
    let py: f64 = padding.get("y")?;
    Ok(fh + py)
}

/// Returns `true` if `view` is a DocView (has a `doc` field with `abs_filename`).
fn is_docview_with_file(view: &LuaTable) -> LuaResult<bool> {
    let doc: LuaValue = view.get("doc")?;
    match doc {
        LuaValue::Table(d) => {
            let abs: LuaValue = d.get("abs_filename")?;
            Ok(!matches!(abs, LuaValue::Nil))
        }
        _ => Ok(false),
    }
}

/// Builds the breadcrumb segments from the file path.
fn path_segments(path: &str) -> Vec<String> {
    path.split('/').filter(|s| !s.is_empty()).map(String::from).collect()
}

/// Determines the current scope at the given line from cached document symbols.
/// Returns a list of scope names from outermost to innermost.
fn current_scope_at_line(symbols: &LuaTable, line: i64) -> LuaResult<Vec<String>> {
    let mut scope = Vec::new();
    let len = symbols.raw_len();
    for i in 1..=(len as i64) {
        let sym: LuaTable = match symbols.raw_get(i)? {
            LuaValue::Table(t) => t,
            _ => continue,
        };
        let range: LuaTable = match sym.get::<LuaValue>("range")? {
            LuaValue::Table(t) => t,
            _ => continue,
        };
        let start: LuaTable = range.get("start")?;
        let end: LuaTable = range.get("end")?;
        let start_line: i64 = start.get("line")?;
        let end_line: i64 = end.get("line")?;
        // LSP lines are 0-based, doc lines are 1-based
        if line > start_line && line <= end_line + 1 {
            let name: String = sym.get::<String>("name").unwrap_or_default();
            if !name.is_empty() {
                scope.push(name);
            }
            // Recurse into children
            if let LuaValue::Table(children) = sym.get::<LuaValue>("children")? {
                let nested = current_scope_at_line(&children, line)?;
                scope.extend(nested);
            }
        }
    }
    Ok(scope)
}

/// Patches `Node:update_layout` to reserve breadcrumb bar space below the tab bar
/// when the active view is a DocView with a file.
fn patch_update_layout(lua: &Lua) -> LuaResult<()> {
    let node_class = require_table(lua, "core.node")?;
    let old: LuaFunction = node_class.get("update_layout")?;
    let old_key = lua.create_registry_value(old)?;

    node_class.set(
        "update_layout",
        lua.create_function(move |lua, this: LuaTable| {
            let old_fn: LuaFunction = lua.registry_value(&old_key)?;
            old_fn.call::<()>(this.clone())?;

            let ntype: String = this.get("type")?;
            if ntype != "leaf" {
                return Ok(());
            }
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let bc_cfg: LuaValue = plugins.get("breadcrumbs")?;
            let enabled = match &bc_cfg {
                LuaValue::Table(t) => t.get::<bool>("enabled").unwrap_or(true),
                _ => true,
            };
            if !enabled {
                return Ok(());
            }

            let should_show_tabs: bool = this.call_method("should_show_tabs", ())?;
            if !should_show_tabs {
                return Ok(());
            }
            let av: LuaTable = this.get("active_view")?;
            if !is_docview_with_file(&av)? {
                return Ok(());
            }
            let bh = breadcrumb_height(lua)?;
            let av_pos: LuaTable = av.get("position")?;
            let av_size: LuaTable = av.get("size")?;
            let cur_y: f64 = av_pos.get("y")?;
            let cur_h: f64 = av_size.get("y")?;
            av_pos.set("y", cur_y + bh)?;
            av_size.set("y", (cur_h - bh).max(0.0))?;
            Ok(())
        })?,
    )?;
    Ok(())
}

/// Patches `Node:draw` to render the breadcrumb bar between tabs and the docview content.
fn patch_draw(lua: &Lua) -> LuaResult<()> {
    let node_class = require_table(lua, "core.node")?;
    let old: LuaFunction = node_class.get("draw")?;
    let old_key = lua.create_registry_value(old)?;

    node_class.set(
        "draw",
        lua.create_function(move |lua, this: LuaTable| {
            let ntype: String = this.get("type")?;
            if ntype == "leaf" {
                let config = require_table(lua, "core.config")?;
                let plugins: LuaTable = config.get("plugins")?;
                let bc_cfg: LuaValue = plugins.get("breadcrumbs")?;
                let enabled = match &bc_cfg {
                    LuaValue::Table(t) => t.get::<bool>("enabled").unwrap_or(true),
                    _ => true,
                };
                let should_show_tabs: bool = this.call_method("should_show_tabs", ())?;
                let av: LuaTable = this.get("active_view")?;
                if enabled && should_show_tabs && is_docview_with_file(&av)? {
                    draw_breadcrumbs(lua, &this, &av)?;
                }
            }
            let old_fn: LuaFunction = lua.registry_value(&old_key)?;
            old_fn.call::<()>(this)
        })?,
    )?;
    Ok(())
}

/// Draws the breadcrumb bar for the given node and docview.
fn draw_breadcrumbs(lua: &Lua, node: &LuaTable, docview: &LuaTable) -> LuaResult<()> {
    let style = require_table(lua, "core.style")?;
    let renderer: LuaTable = lua.globals().get("renderer")?;
    let draw_rect: LuaFunction = renderer.get("draw_rect")?;
    let draw_text: LuaFunction = renderer.get("draw_text")?;
    let common = require_table(lua, "core.common")?;

    let bh = breadcrumb_height(lua)?;
    let position: LuaTable = node.get("position")?;
    let size: LuaTable = node.get("size")?;
    let node_x: f64 = position.get("x")?;
    let node_w: f64 = size.get("x")?;

    // Breadcrumb bar sits right below the tab bar
    let av_pos: LuaTable = docview.get("position")?;
    let bar_y: f64 = av_pos.get::<f64>("y")? - bh;

    // Background
    let bg: LuaValue = style.get("background2")?;
    draw_rect.call::<()>((node_x, bar_y, node_w, bh, bg))?;
    // Divider at bottom
    let ds: f64 = style.get("divider_size")?;
    let divider: LuaValue = style.get("divider")?;
    draw_rect.call::<()>((node_x, bar_y + bh - ds, node_w, ds, divider.clone()))?;

    let font: LuaValue = style.get("font")?;
    let text_color: LuaValue = style.get("text")?;
    let dim_color: LuaValue = style.get("dim")?;
    let padding: LuaTable = style.get("padding")?;
    let px: f64 = padding.get("x")?;

    // Vertically center text in the bar
    let font_h: f64 = match &font {
        LuaValue::Table(t) => t.call_method("get_height", ())?,
        LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
        _ => bh,
    };
    let text_y = bar_y + (bh - font_h) / 2.0;

    let separator = " > ";
    let sep_w: f64 = match &font {
        LuaValue::Table(t) => t.call_method("get_width", separator.to_string())?,
        LuaValue::UserData(ud) => ud.call_method("get_width", separator.to_string())?,
        _ => 16.0,
    };

    // Build breadcrumb text from file path
    let doc: LuaTable = docview.get("doc")?;
    let abs_filename: String = doc.get("abs_filename")?;
    let home_encode: LuaFunction = common.get("home_encode")?;
    let display_path: String = home_encode.call(abs_filename)?;
    let segments = path_segments(&display_path);

    let mut x = node_x + px;
    let max_x = node_x + node_w - px;

    // Draw file path segments
    for (i, seg) in segments.iter().enumerate() {
        if x >= max_x {
            break;
        }
        let is_last_path = i == segments.len() - 1;
        let color = if is_last_path {
            text_color.clone()
        } else {
            dim_color.clone()
        };
        let seg_w: f64 = match &font {
            LuaValue::Table(t) => t.call_method("get_width", seg.clone())?,
            LuaValue::UserData(ud) => ud.call_method("get_width", seg.clone())?,
            _ => 0.0,
        };
        draw_text.call::<()>((font.clone(), seg.clone(), x, text_y, color))?;
        x += seg_w;
        if i < segments.len() - 1 {
            draw_text.call::<()>((font.clone(), separator, x, text_y, dim_color.clone()))?;
            x += sep_w;
        }
    }

    // Draw scope from cached symbols
    let cached_symbols: LuaValue = docview.get("_breadcrumb_symbols")?;
    if let LuaValue::Table(ref symbols) = cached_symbols {
        let line1: i64 = doc
            .call_method::<(i64, i64)>("get_selection", ())?
            .0;
        let scope = current_scope_at_line(symbols, line1)?;
        if !scope.is_empty() {
            // Draw separator before scope
            draw_text.call::<()>((
                font.clone(),
                separator,
                x,
                text_y,
                dim_color.clone(),
            ))?;
            x += sep_w;

            for (i, name) in scope.iter().enumerate() {
                if x >= max_x {
                    break;
                }
                let is_last = i == scope.len() - 1;
                let color = if is_last {
                    text_color.clone()
                } else {
                    dim_color.clone()
                };
                let name_w: f64 = match &font {
                    LuaValue::Table(t) => t.call_method("get_width", name.clone())?,
                    LuaValue::UserData(ud) => ud.call_method("get_width", name.clone())?,
                    _ => 0.0,
                };
                draw_text.call::<()>((font.clone(), name.clone(), x, text_y, color))?;
                x += name_w;
                if i < scope.len() - 1 {
                    draw_text.call::<()>((
                        font.clone(),
                        separator,
                        x,
                        text_y,
                        dim_color.clone(),
                    ))?;
                    x += sep_w;
                }
            }
        }
    }

    Ok(())
}

/// Patches `DocView:update` to periodically request document symbols from the LSP
/// and cache them on the view for breadcrumb scope display.
fn patch_docview_update(lua: &Lua) -> LuaResult<()> {
    let dv_class = require_table(lua, "core.docview")?;
    let old: LuaFunction = dv_class.get("update")?;
    let old_key = lua.create_registry_value(old)?;

    dv_class.set(
        "update",
        lua.create_function(move |lua, (this, rest): (LuaTable, LuaMultiValue)| {
            let old_fn: LuaFunction = lua.registry_value(&old_key)?;
            let mut args = LuaMultiValue::new();
            args.push_back(LuaValue::Table(this.clone()));
            args.extend(rest);
            old_fn.call::<LuaMultiValue>(args)?;

            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let bc_cfg: LuaValue = plugins.get("breadcrumbs")?;
            let enabled = match &bc_cfg {
                LuaValue::Table(t) => t.get::<bool>("enabled").unwrap_or(true),
                _ => true,
            };
            if !enabled {
                return Ok(LuaMultiValue::new());
            }

            if !is_docview_with_file(&this)? {
                return Ok(LuaMultiValue::new());
            }

            // Throttle symbol requests: once per second
            let system = require_table(lua, "system")?;
            let now: f64 = system.get::<LuaFunction>("get_time")?.call(())?;
            let last: f64 = this
                .get::<Option<f64>>("_breadcrumb_last_request")?
                .unwrap_or(0.0);
            if now - last < 1.0 {
                return Ok(LuaMultiValue::new());
            }
            this.set("_breadcrumb_last_request", now)?;

            // Request document symbols via LSP
            request_document_symbols(lua, &this)?;
            Ok(LuaMultiValue::new())
        })?,
    )?;
    Ok(())
}

/// Sends a `textDocument/documentSymbol` request and caches the raw (hierarchical) result
/// on the DocView as `_breadcrumb_symbols`.
fn request_document_symbols(lua: &Lua, docview: &LuaTable) -> LuaResult<()> {
    let lsp_manager: LuaValue = {
        let package: LuaTable = lua.globals().get("package")?;
        let loaded: LuaTable = package.get("loaded")?;
        loaded.get("lsp_manager")?
    };
    let mgr = match lsp_manager {
        LuaValue::Table(t) => t,
        _ => return Ok(()),
    };
    let clients: LuaValue = mgr.get("clients")?;
    let clients = match clients {
        LuaValue::Table(t) => t,
        _ => return Ok(()),
    };

    let doc: LuaTable = docview.get("doc")?;
    let abs: String = doc.get("abs_filename")?;

    // Find a client that supports documentSymbolProvider for this doc
    let common = require_table(lua, "core.common")?;
    let normalize: LuaFunction = common.get("normalize_path")?;
    let norm_abs: String = normalize.call(abs.clone())?;

    let mut uri = norm_abs.replace('\\', "/");
    if !uri.starts_with('/') {
        uri = format!("/{uri}");
    }
    uri = format!("file://{uri}");

    let td = lua.create_table()?;
    td.set("uri", uri)?;
    let params = lua.create_table()?;
    params.set("textDocument", td)?;

    // Find a suitable client
    for pair in clients.pairs::<LuaValue, LuaTable>() {
        let (_, client) = pair?;
        let caps: LuaValue = client.get("server_capabilities")?;
        let has_symbols = match &caps {
            LuaValue::Table(c) => !matches!(
                c.get::<LuaValue>("documentSymbolProvider")?,
                LuaValue::Nil | LuaValue::Boolean(false)
            ),
            _ => false,
        };
        if !has_symbols {
            continue;
        }
        let initialized: bool = client
            .get::<LuaValue>("initialized")?
            .as_boolean()
            .unwrap_or(false);
        if !initialized {
            continue;
        }

        let dv_key = lua.create_registry_value(docview.clone())?;
        let dv_key = Arc::new(dv_key);
        let cb = lua.create_function(move |lua, (result, _err): (LuaValue, LuaValue)| {
            let dv: LuaTable = lua.registry_value(&dv_key)?;
            match result {
                LuaValue::Table(symbols) => {
                    dv.set("_breadcrumb_symbols", symbols)?;
                }
                _ => {
                    dv.set("_breadcrumb_symbols", LuaValue::Nil)?;
                }
            }
            Ok(())
        })?;
        let request_fn: LuaFunction = client.get("request")?;
        request_fn.call::<()>((
            client.clone(),
            "textDocument/documentSymbol",
            params,
            cb,
        ))?;
        return Ok(());
    }
    Ok(())
}

/// Sets up config defaults for the breadcrumbs plugin.
fn set_config_defaults(lua: &Lua) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let common = require_table(lua, "core.common")?;

    let defaults = lua.create_table()?;
    defaults.set("enabled", true)?;

    let spec = lua.create_table()?;
    spec.set("name", "Breadcrumbs")?;

    let enabled_entry = lua.create_table()?;
    enabled_entry.set("label", "Enabled")?;
    enabled_entry.set("description", "Show breadcrumb navigation bar below tabs.")?;
    enabled_entry.set("path", "enabled")?;
    enabled_entry.set("type", "toggle")?;
    enabled_entry.set("default", true)?;
    spec.push(enabled_entry)?;

    defaults.set("config_spec", spec)?;

    let merged: LuaTable =
        common.call_function("merge", (defaults, plugins.get::<LuaValue>("breadcrumbs")?))?;
    plugins.set("breadcrumbs", merged)?;
    Ok(())
}

/// Registers toggle command.
fn register_commands(lua: &Lua) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;
    let cmds = lua.create_table()?;
    cmds.set(
        "breadcrumbs:toggle",
        lua.create_function(|lua, ()| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let bc: LuaTable = plugins.get("breadcrumbs")?;
            let enabled: bool = bc.get("enabled").unwrap_or(true);
            bc.set("enabled", !enabled)?;
            let core = require_table(lua, "core")?;
            core.set("redraw", true)?;
            Ok(())
        })?,
    )?;
    command.call_function::<()>("add", (LuaValue::Nil, cmds))?;
    Ok(())
}

/// Registers `plugins.breadcrumbs` as a preload module.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.breadcrumbs",
        lua.create_function(|lua, ()| {
            set_config_defaults(lua)?;
            patch_update_layout(lua)?;
            patch_draw(lua)?;
            patch_docview_update(lua)?;
            register_commands(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )?;
    Ok(())
}
